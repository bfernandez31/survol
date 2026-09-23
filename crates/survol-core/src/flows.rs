//! End-to-end flows: from the entry points (HTTP endpoints, front-end routes,
//! listeners, scheduled jobs, runners) down to persistence, external calls
//! and events, following the code graph.
//!
//! Only the flows the review touches are computed: the entry points whose
//! flow reaches a changed symbol ([`impacted`]). Each flow is a tree walked
//! depth-first from its entry, in call order:
//!
//! - `calls` edges, then the implementations of an interface method
//!   (`dispatch`, narrowed to the implementations the caller's class
//!   `injects` when it injects some);
//! - `http_calls` edges (front-end HTTP call → back-end endpoint), so a
//!   screen chains to its controller, service and repository;
//! - `publishes` edges (event → listener);
//! - from a class entry (an Angular routed component): its methods
//!   (`member`) and the components it `routes` to;
//! - a repository ends the flow (persistence) with the entities it uses
//!   (`persists`), an external client ends it (external call).
//!
//! Walks are bounded ([`Limits`]): depth, children per step, steps per flow
//! and the product of the edge confidences along the path. A symbol already
//! on the path is a cycle, one already expanded elsewhere in the flow is
//! shown once and referenced after; neither is expanded again. Branches that
//! reach neither a changed symbol nor a terminal are pruned (counted in
//! [`Step::hidden`]).
//!
//! **Before / after**: the same entries are walked on the graph of the base
//! revision ([`base_graph`], bounded to the files around the flows) and the
//! two trees compared by symbol id ([`compare`]): steps added, removed or
//! reached through another path, new or dropped external calls and
//! persistence accesses.

mod base;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::graph::{EdgeKind, Graph, Role, SymIdx, SymbolKind};

pub use base::{FULL_BASE_FILES, base_graph, base_paths};

/// Bounds of a flow walk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Steps below the entry.
    pub max_depth: usize,
    /// Children kept per step (those reaching a change first).
    pub max_children: usize,
    /// Steps per flow.
    pub max_steps: usize,
    /// Paths whose confidence (product of the edges) falls below are cut.
    pub min_confidence: f32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 12,
            max_children: 12,
            max_steps: 400,
            min_confidence: 0.1,
        }
    }
}

/// What starts a flow, from the `entry` tag set by the framework rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Front-end route (a routed component).
    Route,
    Http,
    /// Message or event listener (Kafka, Rabbit, JMS, SQS, application event).
    Listener,
    Scheduled,
    /// `CommandLineRunner`, `main`...
    Runner,
    Other,
}

impl EntryKind {
    /// Heading of a group of entries.
    pub fn title(self) -> &'static str {
        match self {
            EntryKind::Route => "Front-end routes",
            EntryKind::Http => "HTTP endpoints",
            EntryKind::Listener => "Listeners",
            EntryKind::Scheduled => "Scheduled jobs",
            EntryKind::Runner => "Runners / CLI",
            EntryKind::Other => "Other entry points",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// `Outer.method`.
    pub name: String,
    pub kind: EntryKind,
    /// `GET /api/owners/{id}`, `/owners/:id`, `kafka orders`...
    pub label: String,
    pub file: String,
    pub line: u32,
    pub changed: bool,
}

/// How a step is reached from its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// The entry itself.
    Entry,
    Calls,
    /// Implementation of the interface method above.
    Dispatch,
    /// Front-end HTTP call → back-end endpoint.
    HttpCalls,
    /// Event → listener.
    Publishes,
    /// Component → component it displays.
    Routes,
    /// Class entry → its method.
    Member,
    /// Repository → entity.
    Persists,
}

impl Via {
    pub fn label(self) -> &'static str {
        match self {
            Via::Entry => "entry",
            Via::Calls => "calls",
            Via::Dispatch => "impl",
            Via::HttpCalls => "http",
            Via::Publishes => "event",
            Via::Routes => "routes",
            Via::Member => "member",
            Via::Persists => "persists",
        }
    }
}

/// Architectural layer of a step, from its roles and its class's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    View,
    Controller,
    Service,
    Repository,
    Entity,
    External,
    Config,
    Code,
}

impl Layer {
    pub fn label(self) -> &'static str {
        match self {
            Layer::View => "view",
            Layer::Controller => "ctrl",
            Layer::Service => "svc",
            Layer::Repository => "repo",
            Layer::Entity => "entity",
            Layer::External => "ext",
            Layer::Config => "config",
            Layer::Code => "code",
        }
    }
}

/// Where a flow ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Terminal {
    /// A repository: the flow reads or writes the database.
    Persistence,
    /// An entity a repository works on.
    Entity,
    /// A call leaving the code base (HTTP client, Feign...).
    External,
    /// An event published without a listener in the code base.
    Event,
}

/// A step not expanded because it was already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    /// Already on the path: a cycle.
    Cycle,
    /// Expanded elsewhere in the flow.
    Seen,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// Index of the symbol in the graph the flow was computed on.
    #[serde(skip)]
    pub symbol: SymIdx,
    pub id: String,
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    /// Definition line.
    pub line: u32,
    /// Line of the reference in the parent's file (0: none).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ref_line: u32,
    /// 0 for the entry.
    pub depth: usize,
    pub via: Via,
    pub layer: Layer,
    /// Confidence of the edge from the parent.
    pub edge_confidence: f32,
    /// Product of the edge confidences from the entry.
    pub confidence: f32,
    /// The symbol is changed by the review.
    pub changed: bool,
    /// A changed symbol is at or below this step.
    pub reaches_changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<Terminal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<Repeat>,
    /// Children left out by the limits.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Children pruned: they reach neither a change nor a terminal.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hidden: usize,
    /// Route, HTTP call or endpoint, event...: what the step is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn is_zero<T: Default + PartialEq>(n: &T) -> bool {
    *n == T::default()
}

/// A flow: its steps in depth-first order (`steps[0]` is the entry).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flow {
    pub entry: Entry,
    pub steps: Vec<Step>,
    /// Best path confidence to a changed step (1 when the entry changed).
    pub confidence: f32,
    pub changed_steps: usize,
    /// A limit cut the walk somewhere.
    pub truncated: bool,
}

impl Flow {
    /// Index of the parent of each step (`None` for the entry).
    pub fn parents(&self) -> Vec<Option<usize>> {
        parents(self.steps.iter().map(|s| s.depth))
    }

    /// Terminals of the flow of `kind`, by symbol id (once each).
    pub fn terminals(&self, kind: Terminal) -> Vec<&Step> {
        let mut seen = HashSet::new();
        self.steps
            .iter()
            .filter(|s| s.terminal == Some(kind) && seen.insert(s.id.as_str()))
            .collect()
    }
}

fn parents(depths: impl Iterator<Item = usize>) -> Vec<Option<usize>> {
    let mut stack: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    for (i, d) in depths.enumerate() {
        stack.truncate(d);
        out.push(stack.last().copied());
        stack.push(i);
    }
    out
}

/// A flow of the review: after (head), and before (base) once compared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactedFlow {
    pub entry: Entry,
    /// `None`: the entry point is gone in the head revision.
    pub after: Option<Flow>,
    /// `None`: not compared yet, or the entry point is new.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Flow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<FlowDiff>,
}

// ----- impact ---------------------------------------------------------------

/// Changed symbols a flow can reach: callables and types, not tests.
fn seeds(g: &Graph) -> Vec<SymIdx> {
    g.changed_symbols()
        .into_iter()
        .filter(|&s| {
            let sym = g.symbol(s);
            !sym.removed
                && !sym.has_role(Role::Test)
                && (sym.kind.is_callable() || sym.kind.is_type())
        })
        .collect()
}

/// Symbols from which a flow reaches a changed symbol, within `max_depth`
/// steps: the reverse of the walk's edges from the changed symbols.
fn reaching(g: &Graph, seeds: &[SymIdx], max_depth: usize) -> HashSet<SymIdx> {
    let mut seen: HashSet<SymIdx> = seeds.iter().copied().collect();
    let mut queue: VecDeque<(SymIdx, usize)> = seeds.iter().map(|&s| (s, 0)).collect();
    while let Some((s, d)) = queue.pop_front() {
        if d >= max_depth {
            continue;
        }
        for p in predecessors(g, s) {
            if !g.symbol(p).has_role(Role::Test) && seen.insert(p) {
                queue.push_back((p, d + 1));
            }
        }
    }
    seen
}

fn predecessors(g: &Graph, s: SymIdx) -> Vec<SymIdx> {
    let sym = g.symbol(s);
    let mut out: Vec<SymIdx> = g
        .edges_to(s)
        .filter(|e| match e.kind {
            EdgeKind::Calls => !sym.kind.is_type(),
            EdgeKind::HttpCalls | EdgeKind::Publishes | EdgeKind::Routes => true,
            EdgeKind::Uses => sym.has_role(Role::Entity) && is_repository(g, e.from),
            _ => false,
        })
        .map(|e| e.from)
        .collect();
    // An implementation is reached through the interface method.
    out.extend(
        g.edges_from(s)
            .filter(|e| e.kind == EdgeKind::Overrides)
            .map(|e| e.to),
    );
    // A method is reached from its class when the class is an entry.
    if sym.kind.is_callable()
        && let Some(c) = sym.container
        && g.symbol(c).kind.is_type()
    {
        out.push(c);
    }
    out
}

/// The flows of the entry points that reach a changed symbol, grouped by
/// kind of entry then by label.
pub fn impacted(g: &Graph, limits: &Limits) -> Vec<ImpactedFlow> {
    let reach = reaching(g, &seeds(g), limits.max_depth);
    let mut entries: Vec<SymIdx> = reach
        .iter()
        .copied()
        .filter(|&s| {
            let sym = g.symbol(s);
            sym.has_role(Role::EntryPoint) && !sym.has_role(Role::Test) && !sym.removed
        })
        .collect();
    entries.sort_unstable();
    let changed = |s: SymIdx| g.symbol(s).changed;
    let mut out: Vec<ImpactedFlow> = entries
        .into_iter()
        .map(|e| walk(g, e, &changed, &reach, limits))
        .filter(|f| f.changed_steps > 0)
        .map(|f| ImpactedFlow {
            entry: f.entry.clone(),
            after: Some(f),
            before: None,
            diff: None,
        })
        .collect();
    out.sort_by(|a, b| {
        (a.entry.kind, &a.entry.label, &a.entry.id).cmp(&(
            b.entry.kind,
            &b.entry.label,
            &b.entry.id,
        ))
    });
    out
}

/// The flow from `entry` (any symbol), with `changed` telling which symbols
/// the review changes.
pub fn flow_from(
    g: &Graph,
    entry: SymIdx,
    changed: &dyn Fn(SymIdx) -> bool,
    limits: &Limits,
) -> Flow {
    let seeds: Vec<SymIdx> = (0..g.symbols().len() as SymIdx)
        .filter(|&s| changed(s))
        .collect();
    let reach = reaching(g, &seeds, limits.max_depth);
    walk(g, entry, changed, &reach, limits)
}

// ----- walk -----------------------------------------------------------------

struct Walker<'a> {
    g: &'a Graph,
    changed: &'a dyn Fn(SymIdx) -> bool,
    reach: &'a HashSet<SymIdx>,
    limits: &'a Limits,
    steps: Vec<Step>,
    path: Vec<SymIdx>,
    /// Expanded symbols: (keep, reaches a change).
    done: HashMap<SymIdx, (bool, bool)>,
    truncated: bool,
}

/// A child to visit: symbol, how, edge confidence, reference line.
type Child = (SymIdx, Via, f32, u32);

fn walk(
    g: &Graph,
    entry: SymIdx,
    changed: &dyn Fn(SymIdx) -> bool,
    reach: &HashSet<SymIdx>,
    limits: &Limits,
) -> Flow {
    let mut w = Walker {
        g,
        changed,
        reach,
        limits,
        steps: Vec::new(),
        path: Vec::new(),
        done: HashMap::new(),
        truncated: false,
    };
    w.visit((entry, Via::Entry, 1.0, 0), None, 1.0, 0);
    let steps = w.steps;
    let confidence = steps
        .iter()
        .filter(|s| s.changed)
        .map(|s| s.confidence)
        .fold(0.0_f32, f32::max);
    Flow {
        entry: entry_of(g, entry, changed(entry)),
        changed_steps: steps.iter().filter(|s| s.changed).count(),
        confidence,
        truncated: w.truncated,
        steps,
    }
}

impl Walker<'_> {
    /// Visits `child` below `parent`; returns (keep, reaches a change).
    fn visit(
        &mut self,
        (s, via, edge_conf, ref_line): Child,
        parent: Option<SymIdx>,
        path_conf: f32,
        depth: usize,
    ) -> (bool, bool) {
        let g = self.g;
        let changed = (self.changed)(s);
        let i = self.steps.len();
        // An instantiation or an entity: the class is a leaf.
        let leaf = via == Via::Persists || via == Via::Calls && g.symbol(s).kind.is_type();
        let terminal = if leaf && via == Via::Calls {
            None
        } else {
            terminal(g, s, via)
        };
        self.steps.push(Step {
            symbol: s,
            id: g.symbol(s).id.clone(),
            name: g.display_name(s),
            kind: g.symbol(s).kind,
            file: g.symbol(s).file.clone(),
            line: g.symbol(s).line,
            ref_line,
            depth,
            via,
            layer: layer(g, s, via),
            edge_confidence: edge_conf,
            confidence: path_conf,
            changed,
            reaches_changed: changed,
            terminal,
            repeat: None,
            truncated: false,
            hidden: 0,
            detail: detail(g, s, via),
        });
        if leaf {
            return (changed || terminal.is_some(), changed);
        }
        if self.path.contains(&s) {
            self.steps[i].repeat = Some(Repeat::Cycle);
            return (changed || self.reach.contains(&s), changed);
        }
        if let Some(&(keep, reaches)) = self.done.get(&s) {
            self.steps[i].repeat = Some(Repeat::Seen);
            if let Some(first) = self.steps[..i].iter().find(|x| x.symbol == s) {
                self.steps[i].layer = first.layer;
            }
            self.steps[i].reaches_changed = reaches;
            return (keep, reaches);
        }
        let stops = matches!(
            terminal,
            Some(Terminal::Persistence | Terminal::External | Terminal::Entity)
        );
        let mut children = if stops {
            if terminal == Some(Terminal::Persistence) {
                entities(g, s)
            } else {
                Vec::new()
            }
        } else {
            self.children(s, parent)
        };
        children.retain(|c| path_conf * c.2 >= self.limits.min_confidence);
        if !children.is_empty()
            && (depth >= self.limits.max_depth || self.steps.len() >= self.limits.max_steps)
        {
            self.steps[i].truncated = true;
            self.truncated = true;
            children.clear();
        }
        if children.len() > self.limits.max_children {
            // Keep those reaching a change, then the most certain.
            children.sort_by(|a, b| {
                let ra = self.reach.contains(&a.0);
                let rb = self.reach.contains(&b.0);
                rb.cmp(&ra).then(b.2.total_cmp(&a.2))
            });
            children.truncate(self.limits.max_children);
            children.sort_by_key(|c| c.3);
            self.steps[i].truncated = true;
            self.truncated = true;
        }
        self.path.push(s);
        let mut any_kept = false;
        let mut reaches = changed;
        let mut hidden = 0;
        for c in children {
            let len = self.steps.len();
            let (keep, r) = self.visit(c, Some(s), path_conf * c.2, depth + 1);
            reaches |= r;
            if keep {
                any_kept = true;
            } else {
                self.steps.truncate(len);
                hidden += 1;
            }
        }
        self.path.pop();
        // An interface method takes the layer of its implementations.
        if self.steps[i].layer == Layer::Code
            && let Some(l) = self.steps[i + 1..]
                .iter()
                .find(|c| c.depth == depth + 1 && c.via == Via::Dispatch)
                .map(|c| c.layer)
        {
            self.steps[i].layer = l;
        }
        let step = &mut self.steps[i];
        step.hidden = hidden;
        step.reaches_changed = reaches;
        let keep = changed || any_kept || terminal.is_some();
        self.done.insert(s, (keep, reaches));
        (keep, reaches)
    }

    /// Where the flow goes from `s`, in source order.
    fn children(&self, s: SymIdx, parent: Option<SymIdx>) -> Vec<Child> {
        let g = self.g;
        let sym = g.symbol(s);
        let mut out: Vec<Child> = Vec::new();
        let mut push = |c: Child| {
            if g.symbol(c.0).has_role(Role::Test) || g.symbol(c.0).removed {
                return;
            }
            match out.iter_mut().find(|x| x.0 == c.0) {
                Some(x) if x.2 < c.2 => *x = c,
                Some(_) => {}
                None => out.push(c),
            }
        };
        if sym.kind.is_type() {
            for &m in g.symbols_in_file(&sym.file) {
                let ms = g.symbol(m);
                if ms.container == Some(s)
                    && matches!(ms.kind, SymbolKind::Method | SymbolKind::Function)
                {
                    push((m, Via::Member, 1.0, ms.line));
                }
            }
            for e in g.edges_from(s).filter(|e| e.kind == EdgeKind::Routes) {
                push((e.to, Via::Routes, e.confidence, e.line));
            }
            return out;
        }
        let mut calls = 0;
        for e in g.edges_from(s) {
            let via = match e.kind {
                EdgeKind::Calls => {
                    let k = g.symbol(e.to).kind;
                    if !(k.is_callable() || k.is_type()) {
                        continue;
                    }
                    calls += 1;
                    Via::Calls
                }
                EdgeKind::HttpCalls => Via::HttpCalls,
                EdgeKind::Publishes => Via::Publishes,
                _ => continue,
            };
            push((e.to, via, e.confidence, e.line));
        }
        // An interface (or bodiless) method: its implementations.
        let abstract_like = sym
            .container
            .is_some_and(|c| g.symbol(c).kind == SymbolKind::Interface)
            || calls == 0;
        if abstract_like {
            for c in implementations(g, s, parent) {
                push(c);
            }
        }
        out.sort_by_key(|c| (c.3, c.1 != Via::Dispatch));
        out
    }
}

/// Implementations of method `s` (through `overrides` edges), narrowed to
/// the classes the caller's class injects when it injects some of them.
fn implementations(g: &Graph, s: SymIdx, caller: Option<SymIdx>) -> Vec<Child> {
    let impls: Vec<(SymIdx, f32)> = g
        .edges_to(s)
        .filter(|e| e.kind == EdgeKind::Overrides)
        .filter(|e| !g.symbol(e.from).has_role(Role::Test))
        .map(|e| (e.from, e.confidence))
        .collect();
    if impls.is_empty() {
        return Vec::new();
    }
    let injected: HashMap<SymIdx, f32> = caller
        .and_then(|c| enclosing_type(g, c))
        .map(|t| {
            g.edges_from(t)
                .filter(|e| e.kind == EdgeKind::Injects)
                .map(|e| (e.to, e.confidence))
                .collect()
        })
        .unwrap_or_default();
    let of_injected: Vec<(SymIdx, f32)> = impls
        .iter()
        .filter_map(|&(m, c)| {
            let t = enclosing_type(g, m)?;
            injected.get(&t).map(|&ic| (m, c.max(ic)))
        })
        .collect();
    let (chosen, factor) = if !of_injected.is_empty() {
        (of_injected, 1.0)
    } else if impls.len() == 1 {
        (impls, 1.0)
    } else {
        (impls, 0.8)
    };
    chosen
        .into_iter()
        .map(|(m, c)| (m, Via::Dispatch, c * factor, g.symbol(s).line))
        .collect()
}

fn enclosing_type(g: &Graph, mut s: SymIdx) -> Option<SymIdx> {
    loop {
        let sym = g.symbol(s);
        if sym.kind.is_type() {
            return Some(s);
        }
        s = sym.container?;
    }
}

/// Entities a repository method works on (its types, else its class's).
fn entities(g: &Graph, s: SymIdx) -> Vec<Child> {
    let of = |x: SymIdx| -> Vec<Child> {
        g.edges_from(x)
            .filter(|e| {
                matches!(e.kind, EdgeKind::Uses | EdgeKind::Calls)
                    && g.symbol(e.to).has_role(Role::Entity)
                    && g.symbol(e.to).kind.is_type()
            })
            .map(|e| (e.to, Via::Persists, e.confidence, e.line))
            .take(3)
            .collect()
    };
    let mut out = of(s);
    if out.is_empty()
        && let Some(t) = enclosing_type(g, s)
        && t != s
    {
        out = of(t);
    }
    out
}

fn has_role_here_or_class(g: &Graph, s: SymIdx, role: Role) -> bool {
    g.symbol(s).has_role(role) || enclosing_type(g, s).is_some_and(|t| g.symbol(t).has_role(role))
}

fn is_repository(g: &Graph, s: SymIdx) -> bool {
    has_role_here_or_class(g, s, Role::Repository)
}

fn tag<'a>(g: &'a Graph, s: SymIdx, key: &str) -> Option<&'a str> {
    g.symbol(s).tags.get(key).map(String::as_str)
}

fn terminal(g: &Graph, s: SymIdx, via: Via) -> Option<Terminal> {
    let sym = g.symbol(s);
    if via == Via::Persists {
        return Some(Terminal::Entity);
    }
    if sym.kind.is_type() && via != Via::Entry {
        return None;
    }
    if is_repository(g, s) {
        return Some(Terminal::Persistence);
    }
    // A client call linked with some certainty to an endpoint of the code
    // base goes on; a guess does not hide an external call.
    let http_linked = g
        .edges_from(s)
        .any(|e| e.kind == EdgeKind::HttpCalls && e.confidence >= 0.5);
    let external = has_role_here_or_class(g, s, Role::External)
        || sym.tags.contains_key("external")
        || sym.tags.contains_key("http.calls");
    if external && !http_linked && sym.kind.is_callable() {
        return Some(Terminal::External);
    }
    if sym.tags.contains_key("event.publishes")
        && !g.edges_from(s).any(|e| e.kind == EdgeKind::Publishes)
    {
        return Some(Terminal::Event);
    }
    None
}

fn layer(g: &Graph, s: SymIdx, via: Via) -> Layer {
    if via == Via::Persists {
        return Layer::Entity;
    }
    let sym = g.symbol(s);
    let class = enclosing_type(g, s).map(|t| g.symbol(t));
    let has = |r: Role| sym.has_role(r) || class.is_some_and(|c| c.has_role(r));
    if has(Role::Repository) {
        Layer::Repository
    } else if has(Role::External) {
        Layer::External
    } else if has(Role::View) {
        Layer::View
    } else if tag(g, s, "entry") == Some("http")
        || class.is_some_and(|c| {
            c.tags
                .get("spring.stereotype")
                .is_some_and(|t| t.contains("Controller"))
        })
    {
        Layer::Controller
    } else if has(Role::Entity) {
        Layer::Entity
    } else if has(Role::Config) {
        Layer::Config
    } else if has(Role::Component) {
        Layer::Service
    } else {
        Layer::Code
    }
}

/// What the step is about: route, endpoint, HTTP call, event.
fn detail(g: &Graph, s: SymIdx, via: Via) -> Option<String> {
    let t = |k: &str| tag(g, s, k).map(str::to_string);
    match via {
        Via::HttpCalls => t("http.route"),
        Via::Publishes => t("event.type").or_else(|| t("entry")),
        _ => t("http.route")
            .or_else(|| t("angular.route").map(|r| format!("route {r}")))
            .or_else(|| t("http.calls").map(|c| format!("→ {c}")))
            .or_else(|| t("event.publishes").map(|e| format!("publishes {e}")))
            .or_else(|| t("persistence.table").map(|e| format!("table {e}"))),
    }
}

fn entry_of(g: &Graph, s: SymIdx, changed: bool) -> Entry {
    let sym = g.symbol(s);
    let first = |k: &str| tag(g, s, k).map(|v| v.to_string());
    let kinds = tag(g, s, "entry").unwrap_or("");
    let kind = kinds
        .split(", ")
        .map(|k| match k {
            "http" => EntryKind::Http,
            "route" => EntryKind::Route,
            "kafka" | "rabbit" | "jms" | "sqs" | "event" => EntryKind::Listener,
            "scheduled" => EntryKind::Scheduled,
            "runner" | "main" => EntryKind::Runner,
            _ => EntryKind::Other,
        })
        .min()
        .unwrap_or(EntryKind::Other);
    let label = match kind {
        EntryKind::Http => first("http.route"),
        EntryKind::Route => first("angular.route"),
        EntryKind::Listener => [
            "kafka.topics",
            "rabbit.queues",
            "jms.destination",
            "event.type",
        ]
        .iter()
        .find_map(|k| first(k).map(|v| format!("{} {v}", k.split('.').next().unwrap_or(k)))),
        EntryKind::Scheduled => first("schedule").map(|s| format!("scheduled {s}")),
        EntryKind::Runner | EntryKind::Other => None,
    }
    .unwrap_or_else(|| g.display_name(s));
    Entry {
        id: sym.id.clone(),
        name: g.display_name(s),
        kind,
        label,
        file: sym.file.clone(),
        line: sym.line,
        changed,
    }
}

/// Finds the impacted flow whose entry matches `name`: its label, name or
/// id contains it (case-insensitive).
pub fn find<'a>(flows: &'a [ImpactedFlow], name: &str) -> Vec<&'a ImpactedFlow> {
    let n = name.to_lowercase();
    flows
        .iter()
        .filter(|f| {
            [&f.entry.label, &f.entry.name, &f.entry.id]
                .iter()
                .any(|x| x.to_lowercase().contains(&n))
        })
        .collect()
}

// ----- before / after -------------------------------------------------------

/// How a step of the merged tree differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Same,
    /// Only after (head).
    Added,
    /// Only before (base).
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowStatus {
    /// Same steps before and after.
    Same,
    Changed,
    /// The entry point is new.
    New,
    /// The entry point is gone.
    Removed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffStep {
    #[serde(flatten)]
    pub step: Step,
    pub change: Change,
    /// Reached through another path before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rerouted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRef {
    pub id: String,
    pub name: String,
}

/// Before / after of a flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowDiff {
    pub status: FlowStatus,
    /// Both trees merged, depth-first.
    pub steps: Vec<DiffStep>,
    pub added: Vec<StepRef>,
    pub removed: Vec<StepRef>,
    /// In both, reached from other steps.
    pub rerouted: Vec<StepRef>,
    pub new_external: Vec<StepRef>,
    pub dropped_external: Vec<StepRef>,
    pub new_persistence: Vec<StepRef>,
    pub dropped_persistence: Vec<StepRef>,
}

impl FlowDiff {
    /// Worth showing: the flow is not the same before and after.
    pub fn is_relevant(&self) -> bool {
        self.status != FlowStatus::Same
    }

    /// One line: `+2 −3 steps, 1 rerouted, new external call…`.
    pub fn summary(&self) -> String {
        match self.status {
            FlowStatus::Same => return "same flow before and after".into(),
            FlowStatus::New => return "new entry point".into(),
            FlowStatus::Removed => return "entry point removed".into(),
            FlowStatus::Changed => {}
        }
        let mut parts = vec![format!(
            "+{} −{} step(s)",
            self.added.len(),
            self.removed.len()
        )];
        let mut n = |v: &[StepRef], what: &str| {
            if !v.is_empty() {
                parts.push(format!("{} {what}", v.len()));
            }
        };
        n(&self.rerouted, "rerouted");
        n(&self.new_external, "new external call(s)");
        n(&self.dropped_external, "external call(s) gone");
        n(&self.new_persistence, "new persistence access(es)");
        n(&self.dropped_persistence, "persistence access(es) gone");
        parts.join(", ")
    }
}

fn refs<'a>(steps: impl Iterator<Item = &'a Step>) -> Vec<StepRef> {
    let mut seen = HashSet::new();
    steps
        .filter(|s| seen.insert(s.id.clone()))
        .map(|s| StepRef {
            id: s.id.clone(),
            name: s.name.clone(),
        })
        .collect()
}

/// Compares the flow of the same entry before and after.
pub fn diff_flows(before: Option<&Flow>, after: Option<&Flow>) -> FlowDiff {
    let empty: Vec<Step> = Vec::new();
    let b = before.map_or(&empty, |f| &f.steps);
    let a = after.map_or(&empty, |f| &f.steps);
    let ids = |steps: &[Step]| -> HashSet<String> { steps.iter().map(|s| s.id.clone()).collect() };
    let (bi, ai) = (ids(b), ids(a));
    let edges = |steps: &[Step]| -> HashMap<String, HashSet<String>> {
        let mut m: HashMap<String, HashSet<String>> = HashMap::new();
        for (i, p) in parents(steps.iter().map(|s| s.depth))
            .into_iter()
            .enumerate()
        {
            if let Some(p) = p {
                m.entry(steps[i].id.clone())
                    .or_default()
                    .insert(steps[p].id.clone());
            }
        }
        m
    };
    let (be, ae) = (edges(b), edges(a));
    let root = a.first().or(b.first()).map(|s| s.id.as_str());
    let rerouted_ids: HashSet<String> = ai
        .intersection(&bi)
        .filter(|id| Some(id.as_str()) != root && be.get(*id) != ae.get(*id))
        .filter(|id| {
            // Entities are reached from whichever repository: not a reroute.
            a.iter()
                .find(|s| &s.id == *id)
                .is_some_and(|s| s.via != Via::Persists)
        })
        .cloned()
        .collect();
    let term = |steps: &[Step], other: &HashSet<String>, t: Terminal| {
        refs(
            steps
                .iter()
                .filter(|s| s.terminal == Some(t) && !other.contains(&s.id)),
        )
    };
    let status = match (before, after) {
        (None, Some(_)) => FlowStatus::New,
        (Some(_), None) => FlowStatus::Removed,
        _ if ai == bi && rerouted_ids.is_empty() => FlowStatus::Same,
        _ => FlowStatus::Changed,
    };
    let mut rerouted: Vec<&Step> = a.iter().filter(|s| rerouted_ids.contains(&s.id)).collect();
    rerouted.dedup_by(|x, y| x.id == y.id);
    FlowDiff {
        status,
        steps: merge(b, a, &rerouted_ids),
        added: refs(a.iter().filter(|s| !bi.contains(&s.id))),
        removed: refs(b.iter().filter(|s| !ai.contains(&s.id))),
        rerouted: refs(rerouted.into_iter()),
        new_external: term(a, &bi, Terminal::External),
        dropped_external: term(b, &ai, Terminal::External),
        new_persistence: term(a, &bi, Terminal::Persistence),
        dropped_persistence: term(b, &ai, Terminal::Persistence),
    }
}

fn children_lists(steps: &[Step]) -> Vec<Vec<usize>> {
    let mut out = vec![Vec::new(); steps.len()];
    for (i, p) in parents(steps.iter().map(|s| s.depth))
        .into_iter()
        .enumerate()
    {
        if let Some(p) = p {
            out[p].push(i);
        }
    }
    out
}

/// Both trees merged from their roots, matching children by symbol id.
fn merge(before: &[Step], after: &[Step], rerouted: &HashSet<String>) -> Vec<DiffStep> {
    let bc = children_lists(before);
    let ac = children_lists(after);
    let mut out = Vec::new();
    let m = Merger {
        before,
        after,
        bc: &bc,
        ac: &ac,
        rerouted,
    };
    let root = |s: &[Step]| (!s.is_empty()).then_some(0);
    m.node(root(before), root(after), 0, &mut out);
    out
}

struct Merger<'a> {
    before: &'a [Step],
    after: &'a [Step],
    bc: &'a [Vec<usize>],
    ac: &'a [Vec<usize>],
    rerouted: &'a HashSet<String>,
}

impl Merger<'_> {
    fn node(&self, b: Option<usize>, a: Option<usize>, depth: usize, out: &mut Vec<DiffStep>) {
        let (step, change) = match (b, a) {
            (Some(_), Some(a)) => (&self.after[a], Change::Same),
            (None, Some(a)) => (&self.after[a], Change::Added),
            (Some(b), None) => (&self.before[b], Change::Removed),
            (None, None) => return,
        };
        let mut step = step.clone();
        step.depth = depth;
        let rerouted = self.rerouted.contains(&step.id);
        out.push(DiffStep {
            step,
            change,
            rerouted,
        });
        let repeat = |x: Option<usize>, s: &[Step]| x.is_some_and(|i| s[i].repeat.is_some());
        if repeat(b, self.before) || repeat(a, self.after) {
            return;
        }
        let bk: &[usize] = b.map_or(&[], |i| &self.bc[i]);
        let ak: &[usize] = a.map_or(&[], |i| &self.ac[i]);
        // After's order; a child only before goes after its previous sibling.
        let mut pairs: Vec<(Option<usize>, Option<usize>)> = Vec::new();
        let mut used = vec![false; bk.len()];
        for &x in ak {
            let m = bk
                .iter()
                .enumerate()
                .find(|(k, y)| !used[*k] && self.before[**y].id == self.after[x].id)
                .map(|(k, y)| {
                    used[k] = true;
                    *y
                });
            pairs.push((m, Some(x)));
        }
        for (k, &y) in bk.iter().enumerate() {
            if used[k] {
                continue;
            }
            let at = (0..k)
                .rev()
                .find_map(|p| pairs.iter().position(|(bb, _)| *bb == Some(bk[p])))
                .map_or(0, |i| i + 1);
            pairs.insert(at, (Some(y), None));
            used[k] = true;
        }
        for (bb, aa) in pairs {
            self.node(bb, aa, depth + 1, out);
        }
    }
}

/// Walks the entries of `flows` on the base graph and compares: fills
/// `before` and `diff`. Entries that only exist in the base revision and
/// sit in a changed file are added as removed flows.
pub fn compare(flows: &mut Vec<ImpactedFlow>, head: &Graph, base: &Graph, limits: &Limits) {
    // Changed in the review, by id (head changes and removed symbols).
    let changed_ids: HashSet<&str> = head
        .changed_symbols()
        .into_iter()
        .map(|s| head.symbol(s).id.as_str())
        .collect();
    let changed = |s: SymIdx| changed_ids.contains(base.symbol(s).id.as_str());
    let seeds: Vec<SymIdx> = (0..base.symbols().len() as SymIdx)
        .filter(|&s| changed(s))
        .collect();
    let reach = reaching(base, &seeds, limits.max_depth);
    for f in flows.iter_mut() {
        let before = base
            .by_id(&f.entry.id)
            .map(|e| walk(base, e, &changed, &reach, limits));
        f.diff = Some(diff_flows(before.as_ref(), f.after.as_ref()));
        f.before = before;
    }
    // Entry points gone with the review.
    let known: HashSet<String> = flows.iter().map(|f| f.entry.id.clone()).collect();
    let mut gone: Vec<ImpactedFlow> = Vec::new();
    for &s in &reach {
        let sym = base.symbol(s);
        if !sym.has_role(Role::EntryPoint)
            || sym.has_role(Role::Test)
            || known.contains(&sym.id)
            || head.by_id(&sym.id).is_some_and(|h| !head.symbol(h).removed)
            || !head.is_file_changed(&sym.file) && head.file(&sym.file).is_some()
        {
            continue;
        }
        let before = walk(base, s, &changed, &reach, limits);
        gone.push(ImpactedFlow {
            entry: before.entry.clone(),
            diff: Some(diff_flows(Some(&before), None)),
            after: None,
            before: Some(before),
        });
    }
    flows.extend(gone);
    flows.sort_by(|a, b| {
        (a.entry.kind, &a.entry.label, &a.entry.id).cmp(&(
            b.entry.kind,
            &b.entry.label,
            &b.entry.id,
        ))
    });
}

// ----- Mermaid --------------------------------------------------------------

/// A node of a Mermaid chart.
struct MNode<'a> {
    step: &'a Step,
    change: Option<Change>,
}

fn mermaid(title: &str, nodes: &[MNode]) -> String {
    let mut out = format!("---\ntitle: {}\n---\nflowchart TD\n", esc(title));
    let mut ids: HashMap<&str, usize> = HashMap::new();
    // A node's class: added / removed when all its occurrences are, else
    // changed when the review changes it.
    let mut changes: HashMap<&str, (bool, bool, bool)> = HashMap::new();
    for n in nodes {
        let c = changes.entry(n.step.id.as_str()).or_default();
        match n.change {
            Some(Change::Added) => c.0 = true,
            Some(Change::Removed) => c.1 = true,
            _ => c.2 = true,
        }
    }
    let mut classes: Vec<(usize, &str)> = Vec::new();
    for n in nodes {
        let s = n.step;
        if ids.contains_key(s.id.as_str()) {
            continue;
        }
        let k = ids.len();
        ids.insert(&s.id, k);
        let mut label = format!("{}<br/><small>{}", esc(&s.name), s.layer.label());
        if let Some(d) = &s.detail {
            label.push_str(&format!(" · {}", esc(d)));
        }
        label.push_str("</small>");
        let (open, close) = match (s.depth, s.terminal) {
            (0, _) => ("([", "])"),
            (_, Some(Terminal::Persistence | Terminal::Entity)) => ("[(", ")]"),
            (_, Some(Terminal::External)) => ("{{", "}}"),
            (_, Some(Terminal::Event)) => (">", "]"),
            _ => ("[", "]"),
        };
        out.push_str(&format!("  n{k}{open}\"{label}\"{close}\n"));
        match changes[s.id.as_str()] {
            (true, false, false) => classes.push((k, "added")),
            (false, true, false) => classes.push((k, "removed")),
            _ if s.changed => classes.push((k, "changed")),
            _ => {}
        }
    }
    let mut seen = HashSet::new();
    let depths = parents(nodes.iter().map(|n| n.step.depth));
    for (i, p) in depths.into_iter().enumerate() {
        let Some(p) = p else { continue };
        let (from, to) = (
            ids[nodes[p].step.id.as_str()],
            ids[nodes[i].step.id.as_str()],
        );
        if !seen.insert((from, to)) {
            continue;
        }
        let s = nodes[i].step;
        let mut label = s.via.label().to_string();
        if s.edge_confidence < 0.995 {
            label.push_str(&format!(" {:.2}", s.edge_confidence));
        }
        let arrow = match nodes[i].change {
            Some(Change::Removed) => "-.->",
            Some(Change::Added) => "==>",
            _ => "-->",
        };
        out.push_str(&format!("  n{from} {arrow}|{label}| n{to}\n"));
    }
    out.push_str("  classDef changed fill:#fde68a,stroke:#b45309,color:#000\n");
    out.push_str("  classDef added fill:#bbf7d0,stroke:#15803d,color:#000\n");
    out.push_str(
        "  classDef removed fill:#fecaca,stroke:#b91c1c,color:#000,stroke-dasharray: 4 3\n",
    );
    for (k, c) in classes {
        out.push_str(&format!("  class n{k} {c}\n"));
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('"', "#quot;")
        .replace('<', "#lt;")
        .replace('>', "#gt;")
}

impl Flow {
    /// Mermaid flowchart of the flow: changed steps highlighted,
    /// repositories and entities as databases, external calls as hexagons.
    pub fn to_mermaid(&self) -> String {
        let nodes: Vec<MNode> = self
            .steps
            .iter()
            .map(|step| MNode { step, change: None })
            .collect();
        mermaid(&self.entry.label, &nodes)
    }
}

impl FlowDiff {
    /// Mermaid flowchart of both trees: added steps green (thick arrows),
    /// removed ones red (dotted arrows).
    pub fn to_mermaid(&self, title: &str) -> String {
        let nodes: Vec<MNode> = self
            .steps
            .iter()
            .map(|d| MNode {
                step: &d.step,
                change: Some(d.change),
            })
            .collect();
        mermaid(&format!("{title} (before / after)"), &nodes)
    }
}

impl ImpactedFlow {
    /// The flow as Mermaid: the before / after chart when they differ.
    pub fn to_mermaid(&self) -> String {
        match (&self.diff, &self.after) {
            (Some(d), _) if d.is_relevant() => d.to_mermaid(&self.entry.label),
            (_, Some(a)) => a.to_mermaid(),
            (Some(d), None) => d.to_mermaid(&self.entry.label),
            (None, None) => String::new(),
        }
    }
}
