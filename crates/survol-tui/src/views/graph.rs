//! Graph view: the code graph around the changes. Three modes:
//!
//! - **changed symbols**: what the diff changes, by module and file, with the
//!   number of callers (and how many live in files the diff does not touch);
//! - **symbol**: a navigable tree around one symbol (called by, calls, tests,
//!   then any other edge kind), each node expandable to walk the graph
//!   further in the same direction, with a back stack;
//! - **module map**: packages / directories with their dependencies.
//!
//! The right pane previews the code of the selected node. Rendering lives in
//! `ui/graph.rs`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::graph::{EdgeKind, Graph, Link, ModuleMap, Role, SymIdx, SymbolKind};
use survol_core::model::{Diff, LineKind};

use super::{Focus, Scroll};
use crate::app::{Action, Shared};
use crate::highlight::Spans;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Changed,
    /// Results of a `/` search.
    Found,
    Modules,
    Symbol,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Changed => "changed symbols",
            Mode::Found => "search",
            Mode::Modules => "module map",
            Mode::Symbol => "symbol",
        }
    }
}

/// A line of a symbol list (changed symbols, search results).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRow {
    Module(String),
    File(String),
    Symbol(SymIdx),
}

/// A line of the module map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModRow {
    /// Index into [`ModuleMap::modules`].
    Module(usize),
    /// A changed symbol of an unfolded module.
    Symbol { module: usize, sym: SymIdx },
}

/// A relation between symbols: one section of the symbol tree. Expanding a
/// node follows the same relation from it (callers of a caller...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rel {
    Callers,
    Callees,
    Tests,
    /// Other edges of this kind pointing to the symbol.
    In(EdgeKind),
    /// Other edges of this kind leaving the symbol.
    Out(EdgeKind),
}

impl Rel {
    pub fn label(self) -> String {
        match self {
            Rel::Callers => "Called by".into(),
            Rel::Callees => "Calls".into(),
            Rel::Tests => "Tests".into(),
            Rel::In(k) => format!("← {}", kind_name(k)),
            Rel::Out(k) => format!("{} →", kind_name(k)),
        }
    }

    /// The link's line is a reference made by the other symbol (in its
    /// file), not the other symbol's definition.
    pub fn at_reference(self) -> bool {
        matches!(self, Rel::Callers | Rel::Tests | Rel::In(_))
    }
}

/// `HttpCalls` → `http calls`: any edge kind, including future ones.
pub fn kind_name(k: impl std::fmt::Debug) -> String {
    let debug = format!("{k:?}");
    let mut out = String::new();
    for (i, c) in debug.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push(' ');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Short label of a symbol kind.
pub fn kind_label(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::File => "file",
        SymbolKind::Class => "class",
        SymbolKind::Interface => "iface",
        SymbolKind::Enum => "enum",
        SymbolKind::Record => "record",
        SymbolKind::Object => "object",
        SymbolKind::Annotation => "annot",
        SymbolKind::Method => "method",
        SymbolKind::Function => "fn",
        SymbolKind::Constructor => "ctor",
        SymbolKind::Field => "field",
    }
}

fn is_test(g: &Graph, s: SymIdx) -> bool {
    g.symbol(s).has_role(Role::Test)
}

/// Symbols linked to `s` by `rel`, best confidence first, one link each.
/// Test code calling `s` is listed under [`Rel::Tests`], not as a caller.
pub fn related(g: &Graph, rel: Rel, s: SymIdx) -> Vec<Link> {
    let edge_link = |symbol: SymIdx, e: &survol_core::graph::Edge| Link {
        symbol,
        kind: e.kind,
        confidence: e.confidence,
        line: e.line,
        via: None,
    };
    let mut links: Vec<Link> = match rel {
        Rel::Callers => {
            let mut v = g.callers(s);
            v.retain(|l| !is_test(g, l.symbol));
            return v;
        }
        Rel::Callees => return g.callees(s),
        Rel::Tests => {
            let mut v = g.tests_of(s);
            let extra: Vec<Link> = g
                .callers(s)
                .into_iter()
                .filter(|l| is_test(g, l.symbol) && !v.iter().any(|t| t.symbol == l.symbol))
                .collect();
            v.extend(extra);
            return v;
        }
        Rel::In(k) => g
            .edges_to(s)
            .filter(|e| e.kind == k)
            .map(|e| edge_link(e.from, e))
            .collect(),
        Rel::Out(k) => g
            .edges_from(s)
            .filter(|e| e.kind == k)
            .map(|e| edge_link(e.to, e))
            .collect(),
    };
    links.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then(a.symbol.cmp(&b.symbol))
    });
    let mut seen = HashSet::new();
    links.retain(|l| seen.insert(l.symbol));
    links
}

/// Sections of the tree of `root`: callers, callees and tests always, then
/// every other edge kind present, incoming then outgoing.
pub fn sections(g: &Graph, root: SymIdx) -> Vec<(Rel, Vec<Link>)> {
    let mut rels = vec![Rel::Callers, Rel::Callees, Rel::Tests];
    let ins: BTreeSet<EdgeKind> = g
        .edges_to(root)
        .map(|e| e.kind)
        .filter(|k| !matches!(k, EdgeKind::Calls | EdgeKind::Tests))
        .collect();
    let outs: BTreeSet<EdgeKind> = g
        .edges_from(root)
        .map(|e| e.kind)
        .filter(|k| *k != EdgeKind::Calls)
        .collect();
    rels.extend(ins.into_iter().map(Rel::In));
    rels.extend(outs.into_iter().map(Rel::Out));
    rels.into_iter().map(|r| (r, related(g, r, root))).collect()
}

/// Secondary sections bigger than this start folded.
const FOLD_ABOVE: usize = 6;

/// A line of the symbol tree.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeRow {
    Root(SymIdx),
    Section {
        rel: Rel,
        count: usize,
        folded: bool,
    },
    Node {
        rel: Rel,
        link: Link,
        /// Symbols from the section down to this node: the expansion key.
        path: Vec<SymIdx>,
        depth: usize,
        expandable: bool,
        expanded: bool,
        /// Already an ancestor: not expandable.
        cycle: bool,
    },
}

impl TreeRow {
    pub fn depth(&self) -> usize {
        match self {
            TreeRow::Root(_) => 0,
            TreeRow::Section { .. } => 1,
            TreeRow::Node { depth, .. } => depth + 2,
        }
    }
}

/// Where the symbol view stands: kept on the back stack.
#[derive(Debug, Clone)]
pub struct SymbolState {
    pub root: SymIdx,
    /// Sections whose fold state differs from the default.
    toggled: HashSet<Rel>,
    expanded: HashSet<(Rel, Vec<SymIdx>)>,
    pub pos: Scroll,
}

impl SymbolState {
    pub fn new(root: SymIdx) -> Self {
        Self {
            root,
            toggled: HashSet::new(),
            expanded: HashSet::new(),
            pos: Scroll {
                height: 20,
                ..Scroll::default()
            },
        }
    }

    fn folded(&self, rel: Rel, count: usize) -> bool {
        let default =
            !matches!(rel, Rel::Callers | Rel::Callees | Rel::Tests) && count > FOLD_ABOVE;
        default != self.toggled.contains(&rel)
    }
}

/// Visible rows of the tree of `st`.
pub fn flatten(g: &Graph, st: &SymbolState) -> Vec<TreeRow> {
    let mut rows = vec![TreeRow::Root(st.root)];
    for (rel, links) in sections(g, st.root) {
        let folded = st.folded(rel, links.len());
        rows.push(TreeRow::Section {
            rel,
            count: links.len(),
            folded,
        });
        if !folded {
            push_nodes(g, st, rel, &links, &mut vec![st.root], &mut rows);
        }
    }
    rows
}

fn push_nodes(
    g: &Graph,
    st: &SymbolState,
    rel: Rel,
    links: &[Link],
    ancestors: &mut Vec<SymIdx>,
    rows: &mut Vec<TreeRow>,
) {
    for link in links {
        let cycle = ancestors.contains(&link.symbol);
        let mut path = ancestors[1..].to_vec();
        path.push(link.symbol);
        let children = if cycle {
            Vec::new()
        } else {
            related(g, rel, link.symbol)
        };
        let expanded = !children.is_empty() && st.expanded.contains(&(rel, path.clone()));
        rows.push(TreeRow::Node {
            rel,
            link: *link,
            path,
            depth: ancestors.len() - 1,
            expandable: !children.is_empty(),
            expanded,
            cycle,
        });
        if expanded {
            ancestors.push(link.symbol);
            push_nodes(g, st, rel, &children, ancestors, rows);
            ancestors.pop();
        }
    }
}

/// Changed symbols (or any list) by module, then file, then line.
pub fn symbol_list(g: &Graph, syms: impl IntoIterator<Item = SymIdx>) -> Vec<ListRow> {
    let mut v: Vec<(String, &str, u32, SymIdx)> = syms
        .into_iter()
        .filter(|&s| g.symbol(s).kind != SymbolKind::File)
        .map(|s| {
            let sym = g.symbol(s);
            (module_name(g, s), sym.file.as_str(), sym.line, s)
        })
        .collect();
    v.sort();
    let mut rows = Vec::new();
    let (mut module, mut file) = (None, None);
    for (m, f, _, s) in v {
        if module.as_deref() != Some(m.as_str()) {
            rows.push(ListRow::Module(m.clone()));
            module = Some(m);
            file = None;
        }
        if file != Some(f) {
            rows.push(ListRow::File(f.to_string()));
            file = Some(f);
        }
        rows.push(ListRow::Symbol(s));
    }
    rows
}

/// Package or directory of `s` (removed symbols: their directory).
fn module_name(g: &Graph, s: SymIdx) -> String {
    match g.module_of(s) {
        Some(m) => m.to_string(),
        None => {
            let f = &g.symbol(s).file;
            f.rsplit_once('/').map_or("", |(d, _)| d).to_string()
        }
    }
}

/// Callers of `s` (tests aside), and how many of them are in files the diff
/// leaves alone.
pub fn impact(g: &Graph, s: SymIdx) -> (usize, usize) {
    let callers = related(g, Rel::Callers, s);
    let untouched = callers
        .iter()
        .filter(|l| !g.is_file_changed(&g.symbol(l.symbol).file))
        .count();
    (callers.len(), untouched)
}

/// The symbol at a position of the diff: around the line (new side), else
/// the first symbol the hunk changes, else the file.
pub fn symbol_at_position(
    g: &Graph,
    diff: &Diff,
    file: usize,
    hunk: Option<usize>,
    line: Option<usize>,
) -> Option<SymIdx> {
    let path = &diff.files[file].path;
    let not_file = |s: &SymIdx| g.symbol(*s).kind != SymbolKind::File;
    if let (Some(h), Some(l)) = (hunk, line) {
        let dl = &diff.hunks[h].lines[l];
        if dl.kind != LineKind::Removed
            && let Some(n) = dl.new_line
            && let Some(s) = g.symbol_at(path, n).filter(not_file)
        {
            return Some(s);
        }
    }
    if let Some(h) = hunk {
        if let Some(&s) = g.symbols_of_hunk(h).iter().find(|s| not_file(s)) {
            return Some(s);
        }
        let start = diff.hunks[h].new_range.start;
        if let Some(s) = g.symbol_at(path, start).filter(not_file) {
            return Some(s);
        }
    }
    g.by_id(path)
}

/// What the selected row points at: a symbol and a line to show / open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub sym: SymIdx,
    pub file: String,
    pub line: u32,
    /// Only in the base revision.
    pub removed: bool,
}

impl Target {
    fn of_symbol(g: &Graph, s: SymIdx) -> Self {
        let sym = g.symbol(s);
        Self {
            sym: s,
            file: sym.file.clone(),
            line: sym.line,
            removed: sym.removed,
        }
    }
}

/// A file shown in the preview: lines, highlighting, changed lines.
pub struct Source {
    pub lines: Vec<String>,
    pub spans: Vec<Spans>,
    /// Lines added (head) or removed (base) by the diff.
    pub changed: HashSet<u32>,
}

#[derive(Default)]
pub struct GraphView {
    pub mode: Mode,
    /// Changed symbols.
    pub list: Vec<ListRow>,
    pub list_pos: Scroll,
    /// `(callers, callers in untouched files)` of the listed symbols.
    pub impact: HashMap<SymIdx, (usize, usize)>,
    /// Search results.
    pub found: Vec<ListRow>,
    pub found_pos: Scroll,
    pub query: String,
    pub query_editing: bool,

    pub modules: Option<ModuleMap>,
    pub mod_rows: Vec<ModRow>,
    pub mod_pos: Scroll,
    open_modules: HashSet<usize>,

    pub symbol: Option<SymbolState>,
    pub tree: Vec<TreeRow>,
    /// Previous places, most recent last.
    pub back: Vec<(Mode, Option<SymbolState>)>,

    pub focus: Focus,
    pub list_hidden: bool,
    /// Preview offset from the target line, and horizontal scroll.
    pub preview_offset: isize,
    pub hscroll: usize,
    sources: HashMap<(String, bool), Rc<Source>>,
    pending: Option<char>,
}

impl GraphView {
    /// Keys this view handles before the global ones (search input, prefixes).
    pub fn captures_keys(&self) -> bool {
        self.query_editing || self.pending.is_some()
    }

    /// The graph just got built: fill the lists.
    pub fn on_graph_ready(&mut self, sh: &Shared) {
        let Some(g) = &sh.graph else {
            return;
        };
        let changed: Vec<SymIdx> = g.changed_symbols();
        self.list = symbol_list(g, changed.iter().copied());
        self.impact = changed.iter().map(|&s| (s, impact(g, s))).collect();
        self.list_pos.cursor = self.first_symbol_row(&self.list);
        let map = g.module_map();
        self.modules = Some(map);
        self.open_modules.clear();
        self.rebuild_modules(g);
        if let Some(st) = &self.symbol {
            self.tree = flatten(g, st);
        }
    }

    pub fn is_module_open(&self, m: usize) -> bool {
        self.open_modules.contains(&m)
    }

    fn first_symbol_row(&self, rows: &[ListRow]) -> usize {
        rows.iter()
            .position(|r| matches!(r, ListRow::Symbol(_)))
            .unwrap_or(0)
    }

    fn rebuild_modules(&mut self, g: &Graph) {
        let Some(map) = &self.modules else {
            return;
        };
        let changed = g.changed_symbols();
        let mut rows = Vec::new();
        for (i, m) in map.modules.iter().enumerate() {
            rows.push(ModRow::Module(i));
            if self.open_modules.contains(&i) {
                let mut syms: Vec<SymIdx> = changed
                    .iter()
                    .copied()
                    .filter(|&s| g.symbol(s).kind != SymbolKind::File)
                    .filter(|&s| module_name(g, s) == m.name)
                    .collect();
                syms.sort_by_key(|&s| (g.symbol(s).file.clone(), g.symbol(s).line));
                rows.extend(
                    syms.into_iter()
                        .map(|sym| ModRow::Symbol { module: i, sym }),
                );
            }
        }
        self.mod_rows = rows;
        self.mod_pos.clamp(self.mod_rows.len());
    }

    // ----- navigation ---------------------------------------------------

    /// Shows the tree of `s`, remembering where we were.
    pub fn show_symbol(&mut self, sh: &Shared, s: SymIdx) {
        let Some(g) = &sh.graph else {
            return;
        };
        if self.mode == Mode::Symbol && self.symbol.as_ref().is_some_and(|st| st.root == s) {
            return;
        }
        self.back.push((self.mode, self.symbol.take()));
        let st = SymbolState::new(s);
        self.tree = flatten(g, &st);
        self.symbol = Some(st);
        self.mode = Mode::Symbol;
        self.focus = Focus::List;
        self.preview_offset = 0;
    }

    /// Back to the previous place (`Backspace` / `Ctrl-o`).
    pub fn go_back(&mut self, sh: &Shared) {
        let Some(g) = &sh.graph else {
            return;
        };
        match self.back.pop() {
            Some((mode, st)) => {
                self.mode = mode;
                if let Some(st) = st {
                    self.tree = flatten(g, &st);
                    self.symbol = Some(st);
                }
            }
            None if self.mode != Mode::Changed => self.mode = Mode::Changed,
            None => {}
        }
        self.preview_offset = 0;
    }

    /// `m`: next mode (symbol only when there is one).
    fn cycle_mode(&mut self) {
        let order = [Mode::Changed, Mode::Found, Mode::Modules, Mode::Symbol];
        let i = order.iter().position(|m| *m == self.mode).unwrap_or(0);
        for k in 1..=order.len() {
            let m = order[(i + k) % order.len()];
            let available = match m {
                Mode::Found => !self.query.is_empty(),
                Mode::Symbol => self.symbol.is_some(),
                _ => true,
            };
            if available {
                self.mode = m;
                self.preview_offset = 0;
                return;
            }
        }
    }

    fn pos_mut(&mut self) -> &mut Scroll {
        match self.mode {
            Mode::Changed => &mut self.list_pos,
            Mode::Found => &mut self.found_pos,
            Mode::Modules => &mut self.mod_pos,
            Mode::Symbol => match &mut self.symbol {
                Some(st) => &mut st.pos,
                None => &mut self.list_pos,
            },
        }
    }

    pub fn pos(&self) -> Scroll {
        match self.mode {
            Mode::Changed => self.list_pos,
            Mode::Found => self.found_pos,
            Mode::Modules => self.mod_pos,
            Mode::Symbol => self.symbol.as_ref().map_or(self.list_pos, |st| st.pos),
        }
    }

    pub fn len(&self) -> usize {
        match self.mode {
            Mode::Changed => self.list.len(),
            Mode::Found => self.found.len(),
            Mode::Modules => self.mod_rows.len(),
            Mode::Symbol => self.tree.len(),
        }
    }

    pub fn set_list_height(&mut self, h: usize) {
        let len = self.len();
        let pos = self.pos_mut();
        if pos.height != h {
            pos.height = h;
            pos.clamp(len);
        }
    }

    fn select(&mut self, i: usize) {
        let len = self.len();
        let pos = self.pos_mut();
        pos.cursor = i;
        pos.clamp(len);
        self.preview_offset = 0;
    }

    fn move_by(&mut self, delta: isize) {
        let c = self.pos().cursor.saturating_add_signed(delta);
        self.select(c);
    }

    /// Next / previous header: module (lists, map) or section (tree).
    fn next_header(&mut self, forward: bool) {
        let is_head = |i: usize| match self.mode {
            Mode::Changed => matches!(self.list[i], ListRow::Module(_)),
            Mode::Found => matches!(self.found[i], ListRow::Module(_)),
            Mode::Modules => matches!(self.mod_rows[i], ModRow::Module(_)),
            Mode::Symbol => matches!(self.tree[i], TreeRow::Section { .. }),
        };
        let c = self.pos().cursor;
        let found = if forward {
            (c + 1..self.len()).find(|&i| is_head(i))
        } else {
            (0..c).rev().find(|&i| is_head(i))
        };
        if let Some(i) = found {
            self.select(i);
        }
    }

    fn refresh_tree(&mut self, g: &Graph) {
        if let Some(st) = &self.symbol {
            self.tree = flatten(g, st);
            let len = self.tree.len();
            if let Some(st) = &mut self.symbol {
                st.pos.clamp(len);
            }
        }
    }

    /// `l`: unfold / expand, or go to the first child.
    fn tree_right(&mut self, g: &Graph) {
        let c = self.pos().cursor;
        let Some(st) = &mut self.symbol else {
            return;
        };
        match self.tree.get(c).cloned() {
            Some(TreeRow::Section { rel, folded, .. }) => {
                if folded {
                    toggle(&mut st.toggled, rel);
                    self.refresh_tree(g);
                } else {
                    self.child_or_stay(c);
                }
            }
            Some(TreeRow::Node {
                rel,
                path,
                expandable,
                expanded,
                ..
            }) if expandable => {
                if expanded {
                    self.child_or_stay(c);
                } else {
                    st.expanded.insert((rel, path));
                    self.refresh_tree(g);
                }
            }
            _ => self.focus = Focus::Content,
        }
    }

    fn child_or_stay(&mut self, c: usize) {
        if self
            .tree
            .get(c + 1)
            .is_some_and(|r| r.depth() > self.tree[c].depth())
        {
            self.select(c + 1);
        }
    }

    /// `h`: fold / collapse, or go to the parent row.
    fn tree_left(&mut self, g: &Graph) {
        let c = self.pos().cursor;
        let Some(st) = &mut self.symbol else {
            return;
        };
        match self.tree.get(c).cloned() {
            Some(TreeRow::Section {
                rel, folded: false, ..
            }) => {
                toggle(&mut st.toggled, rel);
                self.refresh_tree(g);
            }
            Some(TreeRow::Node {
                rel,
                path,
                expanded: true,
                ..
            }) => {
                st.expanded.remove(&(rel, path));
                self.refresh_tree(g);
            }
            Some(row) => {
                let d = row.depth();
                if let Some(p) = (0..c).rev().find(|&i| self.tree[i].depth() < d) {
                    self.select(p);
                }
            }
            None => {}
        }
    }

    fn toggle_fold(&mut self, g: &Graph) {
        match self.mode {
            Mode::Symbol => {
                let c = self.pos().cursor;
                let Some(st) = &mut self.symbol else {
                    return;
                };
                match self.tree.get(c).cloned() {
                    Some(TreeRow::Section { rel, .. }) => toggle(&mut st.toggled, rel),
                    Some(TreeRow::Node {
                        rel,
                        path,
                        expandable: true,
                        ..
                    }) => {
                        let key = (rel, path);
                        if !st.expanded.remove(&key) {
                            st.expanded.insert(key);
                        }
                    }
                    _ => return,
                }
                self.refresh_tree(g);
            }
            Mode::Modules => {
                let c = self.mod_pos.cursor;
                let m = match self.mod_rows.get(c) {
                    Some(ModRow::Module(m)) | Some(ModRow::Symbol { module: m, .. }) => *m,
                    None => return,
                };
                if !self.open_modules.remove(&m) {
                    self.open_modules.insert(m);
                }
                self.rebuild_modules(g);
                if let Some(i) = self.mod_rows.iter().position(|r| *r == ModRow::Module(m)) {
                    self.select(i);
                }
            }
            _ => {}
        }
    }

    // ----- targets and actions ------------------------------------------

    /// What the selected row points at.
    pub fn target(&self, g: &Graph) -> Option<Target> {
        let c = self.pos().cursor;
        let from_list = |rows: &[ListRow]| match rows.get(c)? {
            ListRow::Symbol(s) => Some(Target::of_symbol(g, *s)),
            ListRow::File(f) => g.by_id(f).map(|s| Target::of_symbol(g, s)),
            ListRow::Module(_) => None,
        };
        match self.mode {
            Mode::Changed => from_list(&self.list),
            Mode::Found => from_list(&self.found),
            Mode::Modules => match self.mod_rows.get(c)? {
                ModRow::Symbol { sym, .. } => Some(Target::of_symbol(g, *sym)),
                ModRow::Module(_) => None,
            },
            Mode::Symbol => {
                let st = self.symbol.as_ref()?;
                match self.tree.get(c)? {
                    TreeRow::Root(_) | TreeRow::Section { .. } => {
                        Some(Target::of_symbol(g, st.root))
                    }
                    TreeRow::Node { rel, link, .. } => {
                        let mut t = Target::of_symbol(g, link.symbol);
                        if rel.at_reference() && link.line > 0 {
                            t.line = link.line;
                        }
                        Some(t)
                    }
                }
            }
        }
    }

    /// `Enter` on the selected row.
    fn enter(&mut self, sh: &mut Shared) -> Action {
        let Some(g) = &sh.graph else {
            return Action::None;
        };
        let c = self.pos().cursor;
        let sym = match self.mode {
            Mode::Changed | Mode::Found => {
                let rows = if self.mode == Mode::Changed {
                    &self.list
                } else {
                    &self.found
                };
                match rows.get(c) {
                    Some(ListRow::Symbol(s)) => Some(*s),
                    Some(ListRow::File(f)) => g.by_id(f),
                    _ => None,
                }
            }
            Mode::Modules => match self.mod_rows.get(c) {
                Some(ModRow::Symbol { sym, .. }) => Some(*sym),
                Some(ModRow::Module(_)) => {
                    self.toggle_fold(g);
                    None
                }
                None => None,
            },
            Mode::Symbol => match self.tree.get(c) {
                Some(TreeRow::Node { link, .. }) => Some(link.symbol),
                Some(TreeRow::Section { .. }) => {
                    self.toggle_fold(g);
                    None
                }
                _ => None,
            },
        };
        if let Some(s) = sym {
            if g.symbol(s).removed {
                sh.notify("removed symbol: it has no links in the head revision");
            }
            self.show_symbol(sh, s);
        }
        Action::None
    }

    /// `a`: the selected symbol, for a question to the LLM.
    pub fn ask_subject(&self, sh: &mut Shared) -> Option<(survol_core::ask::Subject, String)> {
        let Some(g) = &sh.graph else {
            sh.notify("the code graph is still being built…");
            return None;
        };
        match self.target(g) {
            Some(t) => Some((
                survol_core::ask::Subject::Symbol(g.symbol(t.sym).id.clone()),
                g.display_name(t.sym),
            )),
            None => {
                sh.notify("select a symbol to ask about");
                None
            }
        }
    }

    /// `gd`: the hunks of the selected symbol in the Diff view.
    fn to_diff(&self, sh: &mut Shared) -> Action {
        let Some(g) = &sh.graph else {
            return Action::None;
        };
        let Some(t) = self.target(g) else {
            return Action::None;
        };
        if let Some(&h) = g.hunks_of_symbol(t.sym).first() {
            return Action::ShowHunk(h);
        }
        // Unchanged symbol in a changed file: the hunk around the line, if any.
        let diff = &sh.review.diff;
        let hunk = diff
            .files
            .iter()
            .position(|f| f.path == t.file)
            .and_then(|fi| {
                diff.file_hunks(fi)
                    .find(|h| {
                        t.line >= h.new_range.start && t.line < h.new_range.start + h.new_range.len
                    })
                    .map(|h| h.id)
            });
        match hunk {
            Some(h) => Action::ShowHunk(h),
            None => {
                let name = g.display_name(t.sym);
                sh.notify(format!("{name} is not changed in this review"));
                Action::None
            }
        }
    }

    fn open_in_editor(&self, sh: &mut Shared) {
        let Some(g) = &sh.graph else {
            return;
        };
        let Some(t) = self.target(g) else {
            return;
        };
        if t.removed {
            return sh.notify("removed symbol: not in the head revision");
        }
        let line = t.line.saturating_add_signed(self.preview_offset as i32);
        sh.open_path(&t.file, line.max(1));
    }

    fn run_query(&mut self, sh: &mut Shared) {
        let Some(g) = &sh.graph else {
            return;
        };
        let q = self.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let mut hits = g.find(&q);
        if hits.is_empty() {
            let needle = q.to_lowercase();
            hits = (0..g.symbols().len() as SymIdx)
                .filter(|&s| {
                    let sym = g.symbol(s);
                    !sym.removed
                        && sym.kind != SymbolKind::File
                        && g.display_name(s).to_lowercase().contains(&needle)
                })
                .take(500)
                .collect();
        }
        hits.retain(|&s| g.symbol(s).kind != SymbolKind::File);
        match hits.len() {
            0 => sh.notify(format!("no symbol matches `{q}`")),
            1 => self.show_symbol(sh, hits[0]),
            n => {
                self.found = symbol_list(g, hits);
                self.found_pos = Scroll {
                    height: self.found_pos.height,
                    ..Scroll::default()
                };
                self.found_pos.cursor = self.first_symbol_row(&self.found);
                self.back.push((self.mode, self.symbol.clone()));
                self.mode = Mode::Found;
                sh.notify(format!("{n} symbols match `{q}`"));
            }
        }
    }

    fn export_mermaid(&self, sh: &mut Shared) {
        let Some(map) = &self.modules else {
            return;
        };
        let dir = match sh.review.repo.survol_dir() {
            Ok(d) => d.join("exports"),
            Err(e) => return sh.notify(format!("cannot export: {e}")),
        };
        let head = survol_core::review::short(&sh.review.head_sha);
        let path = dir.join(format!("modules-{head}.md"));
        let text = format!(
            "# Module map of {}\n\n```mermaid\n{}```\n",
            sh.review.title(),
            map.to_mermaid()
        );
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text)) {
            Ok(()) => sh.notify(format!("module map written to {}", path.display())),
            Err(e) => sh.notify(format!("cannot write {}: {e}", path.display())),
        }
    }

    // ----- preview source -----------------------------------------------

    /// The file of `t`, read from the worktree when ready, else from git.
    pub fn source(&mut self, sh: &mut Shared, t: &Target) -> Option<Rc<Source>> {
        let key = (t.file.clone(), t.removed);
        if let Some(s) = self.sources.get(&key) {
            return Some(s.clone());
        }
        let diff = &sh.review.diff;
        let file = diff.files.iter().find(|f| f.path == t.file);
        let text = if t.removed {
            let old = file.and_then(|f| f.old_path.as_deref()).unwrap_or(&t.file);
            git_show(sh, &sh.review.base_sha, old)
        } else if sh.worktree_ready {
            std::fs::read_to_string(sh.review.worktree.join(&t.file))
                .ok()
                .or_else(|| git_show(sh, &sh.review.head_sha, &t.file))
        } else {
            git_show(sh, &sh.review.head_sha, &t.file)
        }?;
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let spans = if lines.len() > 20_000 {
            lines
                .iter()
                .map(|l| vec![(Default::default(), crate::highlight::expand_tabs(l))])
                .collect()
        } else {
            sh.highlighter
                .lines(&t.file, lines.iter().map(String::as_str))
        };
        let mut changed = HashSet::new();
        if let Some(f) = file {
            for h in diff.file_hunks(diff.files.iter().position(|x| x.path == f.path)?) {
                for l in &h.lines {
                    match (t.removed, l.kind) {
                        (false, LineKind::Added) => changed.extend(l.new_line),
                        (true, LineKind::Removed) => changed.extend(l.old_line),
                        _ => {}
                    }
                }
            }
        }
        let src = Rc::new(Source {
            lines,
            spans,
            changed,
        });
        self.sources.insert(key, src.clone());
        Some(src)
    }

    // ----- keys ---------------------------------------------------------

    pub fn on_key(&mut self, sh: &mut Shared, key: KeyEvent) -> Action {
        if self.query_editing {
            self.on_query_key(sh, key);
            return Action::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(p) = self.pending.take() {
            match (p, key.code) {
                ('g', KeyCode::Char('g')) => match self.focus {
                    Focus::List => self.select(0),
                    Focus::Content => self.preview_offset = 0,
                },
                ('g', KeyCode::Char('d')) => return self.to_diff(sh),
                ('z', KeyCode::Char('a' | 'o' | 'c')) => {
                    if let Some(g) = &sh.graph {
                        self.toggle_fold(g);
                    }
                }
                _ => {}
            }
            return Action::None;
        }
        let Some(g) = &sh.graph else {
            return Action::None;
        };
        match key.code {
            KeyCode::Char('g') | KeyCode::Char('z') if !ctrl => {
                if let KeyCode::Char(c) = key.code {
                    self.pending = Some(c);
                }
            }
            KeyCode::Char('o') if ctrl => self.go_back(sh),
            KeyCode::Backspace => self.go_back(sh),
            KeyCode::Char('h') if ctrl => {
                self.list_hidden = false;
                self.focus = Focus::List;
            }
            KeyCode::Char('l') if ctrl => self.focus = Focus::Content,
            KeyCode::Char('B') => {
                self.list_hidden = !self.list_hidden;
                if self.list_hidden {
                    self.focus = Focus::Content;
                }
            }
            KeyCode::Char('m') => self.cycle_mode(),
            KeyCode::Char('/') => {
                self.query_editing = true;
                self.query.clear();
            }
            KeyCode::Char('e') => self.open_in_editor(sh),
            KeyCode::Char('x') => self.export_mermaid(sh),
            KeyCode::Char('o') => self.toggle_fold(g),
            KeyCode::Char('0') => self.hscroll = 0,
            _ => match self.focus {
                Focus::List => return self.on_list_key(sh, key),
                Focus::Content => return self.on_preview_key(sh, key),
            },
        }
        Action::None
    }

    fn on_list_key(&mut self, sh: &mut Shared, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.pos().height / 2).max(1) as isize;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('d') if ctrl => self.move_by(half),
            KeyCode::Char('u') if ctrl => self.move_by(-half),
            KeyCode::PageDown => self.move_by(half * 2),
            KeyCode::PageUp => self.move_by(-half * 2),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::Home => self.select(0),
            KeyCode::Char('n') | KeyCode::Char('J') | KeyCode::Char(']') => self.next_header(true),
            KeyCode::Char('N') | KeyCode::Char('K') | KeyCode::Char('[') => self.next_header(false),
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some(g) = &sh.graph {
                    match self.mode {
                        Mode::Symbol => self.tree_right(g),
                        Mode::Modules => {
                            if matches!(self.mod_rows.get(self.mod_pos.cursor), Some(ModRow::Module(m)) if !self.open_modules.contains(m))
                            {
                                self.toggle_fold(g);
                            }
                        }
                        _ => self.focus = Focus::Content,
                    }
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if let Some(g) = &sh.graph {
                    match self.mode {
                        Mode::Symbol => self.tree_left(g),
                        Mode::Modules => match self.mod_rows.get(self.mod_pos.cursor) {
                            Some(ModRow::Module(m)) if self.open_modules.contains(m) => {
                                self.toggle_fold(g)
                            }
                            Some(ModRow::Symbol { module, .. }) => {
                                let m = *module;
                                if let Some(i) =
                                    self.mod_rows.iter().position(|r| *r == ModRow::Module(m))
                                {
                                    self.select(i);
                                }
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }
            KeyCode::Enter => return self.enter(sh),
            KeyCode::Esc if self.mode == Mode::Found => self.go_back(sh),
            _ => {}
        }
        Action::None
    }

    fn on_preview_key(&mut self, sh: &mut Shared, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.preview_offset += 1,
            KeyCode::Char('k') | KeyCode::Up => self.preview_offset -= 1,
            KeyCode::Char('d') if ctrl => self.preview_offset += 10,
            KeyCode::Char('u') if ctrl => self.preview_offset -= 10,
            KeyCode::Char('h') | KeyCode::Left => self.hscroll = self.hscroll.saturating_sub(8),
            KeyCode::Char('l') | KeyCode::Right => self.hscroll += 8,
            KeyCode::Enter => self.open_in_editor(sh),
            KeyCode::Esc => self.focus = Focus::List,
            _ => {}
        }
        Action::None
    }

    fn on_query_key(&mut self, sh: &mut Shared, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                self.query_editing = false;
                self.run_query(sh);
            }
            KeyCode::Esc => {
                self.query_editing = false;
                self.query.clear();
            }
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(c)
            }
            _ => {}
        }
    }
}

fn toggle(set: &mut HashSet<Rel>, rel: Rel) {
    if !set.remove(&rel) {
        set.insert(rel);
    }
}

fn git_show(sh: &Shared, rev: &str, path: &str) -> Option<String> {
    let bytes = sh
        .review
        .repo
        .bytes(&["show", &format!("{rev}:{path}")])
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use survol_core::git::Git;
    use survol_core::graph::{self, Lang};
    use survol_core::index::{Index, parse_file};
    use survol_core::review::Review;
    use survol_core::review_state::ReviewState;

    use super::*;
    use crate::views::Row;
    use crate::views::diff::DiffView;

    const SERVICE: &str = "package app.owner;\n\
        public class OwnerService {\n\
            private final OwnerRepository repo;\n\
            OwnerService(OwnerRepository repo) { this.repo = repo; }\n\
            public Owner find(int id) {\n\
                return repo.findById(id);\n\
            }\n\
        }\n";
    const REPO: &str = "package app.owner;\n\
        public interface OwnerRepository {\n\
            Owner findById(int id);\n\
        }\n";
    const CONTROLLER: &str = "package app.web;\n\
        import app.owner.OwnerService;\n\
        public class OwnerController {\n\
            private final OwnerService service;\n\
            OwnerController(OwnerService service) { this.service = service; }\n\
            Object show(int id) { return service.find(id); }\n\
        }\n";
    const ADMIN: &str = "package app.web;\n\
        public class Admin {\n\
            OwnerController controller;\n\
            void run() { controller.show(1); }\n\
            void a() { b(); }\n\
            void b() { a(); }\n\
        }\n";
    const TEST: &str = "package app.owner;\n\
        class OwnerServiceTest {\n\
            OwnerService service;\n\
            void findsOwner() { service.find(1); }\n\
        }\n";
    const PATH: &str = "src/main/java/app/owner/OwnerService.java";

    /// Changes line 6 of OwnerService (inside `find`).
    fn raw_diff() -> String {
        format!(
            "{} a/{PATH} b/{PATH}\n--- a/{PATH}\n+++ b/{PATH}\n\
             @@ -6,1 +6,1 @@\n-return repo.findById(0);\n+return repo.findById(id);\n",
            ["diff", "--git"].join(" ")
        )
    }

    fn setup(dir: &std::path::Path) -> (Shared, GraphView) {
        let diff = survol_core::diff::parse(raw_diff().as_bytes()).unwrap();
        let mut files: Vec<_> = [
            (PATH, SERVICE),
            ("src/main/java/app/owner/OwnerRepository.java", REPO),
            ("src/main/java/app/web/OwnerController.java", CONTROLLER),
            ("src/main/java/app/web/Admin.java", ADMIN),
            ("src/test/java/app/owner/OwnerServiceTest.java", TEST),
        ]
        .iter()
        .map(|(p, s)| parse_file(p, Lang::from_path(p).unwrap(), s))
        .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let index = Index {
            files,
            base_files: Vec::new(),
            stats: Default::default(),
        };
        let g = graph::build(&index, &diff, &graph::default_rules());
        let review = Review {
            mr: None,
            base_sha: "base".into(),
            head_sha: "head".into(),
            diff,
            worktree: dir.to_path_buf(),
            state_key: "test".into(),
            repo: Git::new(dir),
        };
        let mut sh = Shared::new(review, ReviewState::default(), dir.join("state.json"));
        sh.graph = Some(g);
        let mut v = GraphView::default();
        v.on_graph_ready(&sh);
        (sh, v)
    }

    fn sym(sh: &Shared, name: &str) -> SymIdx {
        let g = sh.graph.as_ref().unwrap();
        let mut found = g.find(name);
        found.retain(|&s| g.symbol(s).kind != SymbolKind::Constructor);
        assert_eq!(found.len(), 1, "{name}");
        found[0]
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    /// Display names of the visible node rows, with their depth.
    fn nodes(sh: &Shared, v: &GraphView) -> Vec<(usize, String)> {
        let g = sh.graph.as_ref().unwrap();
        v.tree
            .iter()
            .filter_map(|r| match r {
                TreeRow::Node { link, depth, .. } => Some((*depth, g.display_name(link.symbol))),
                _ => None,
            })
            .collect()
    }

    fn select_node(sh: &Shared, v: &mut GraphView, name: &str) {
        let g = sh.graph.as_ref().unwrap();
        let i = v
            .tree
            .iter()
            .position(
                |r| matches!(r, TreeRow::Node { link, .. } if g.display_name(link.symbol) == name),
            )
            .unwrap_or_else(|| panic!("{name} not in the tree"));
        v.select(i);
    }

    #[test]
    fn changed_symbols_with_callers_in_untouched_files() {
        let dir = tempfile::tempdir().unwrap();
        let (sh, v) = setup(dir.path());
        let find = sym(&sh, "OwnerService.find");
        assert!(v.list.contains(&ListRow::Symbol(find)));
        assert_eq!(v.list[0], ListRow::Module("app.owner".into()));
        assert_eq!(v.list[v.list_pos.cursor], ListRow::Symbol(find));
        // The controller is not part of the diff (the test is not a caller).
        assert_eq!(v.impact[&find], (1, 1));
        let g = sh.graph.as_ref().unwrap();
        assert_eq!(v.target(g).unwrap().sym, find);
    }

    #[test]
    fn symbol_tree_sections_and_recursive_expansion() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        let find = sym(&sh, "OwnerService.find");
        v.on_key(&mut sh, code(KeyCode::Enter));
        assert_eq!(v.mode, Mode::Symbol);
        assert_eq!(v.tree[0], TreeRow::Root(find));
        let sections: Vec<Rel> = v
            .tree
            .iter()
            .filter_map(|r| match r {
                TreeRow::Section { rel, .. } => Some(*rel),
                _ => None,
            })
            .collect();
        assert_eq!(sections[..3], [Rel::Callers, Rel::Callees, Rel::Tests]);
        // Test code shows under Tests only.
        let n = nodes(&sh, &v);
        assert_eq!(n[0], (0, "OwnerController.show".to_string()));
        assert_eq!(
            n.iter()
                .filter(|(_, s)| s == "OwnerServiceTest.findsOwner")
                .count(),
            1
        );
        // A caller's line is where it makes the call.
        select_node(&sh, &mut v, "OwnerController.show");
        let t = v.target(sh.graph.as_ref().unwrap()).unwrap();
        assert_eq!(
            (t.file.as_str(), t.line),
            ("src/main/java/app/web/OwnerController.java", 6)
        );
        // l expands: the caller's callers, one level deeper; l again enters it.
        v.on_key(&mut sh, key('l'));
        assert!(nodes(&sh, &v).contains(&(1, "Admin.run".to_string())));
        v.on_key(&mut sh, key('l'));
        let g = sh.graph.as_ref().unwrap();
        assert_eq!(g.display_name(v.target(g).unwrap().sym), "Admin.run");
        // h goes to the parent, then collapses it.
        v.on_key(&mut sh, key('h'));
        v.on_key(&mut sh, key('h'));
        assert!(!nodes(&sh, &v).iter().any(|(_, n)| n == "Admin.run"));
        // h on a node goes to its section, then folds it; l unfolds.
        v.on_key(&mut sh, key('h'));
        assert!(matches!(
            v.tree[v.pos().cursor],
            TreeRow::Section {
                rel: Rel::Callers,
                ..
            }
        ));
        v.on_key(&mut sh, key('h'));
        assert!(
            !nodes(&sh, &v)
                .iter()
                .any(|(_, n)| n == "OwnerController.show")
        );
        v.on_key(&mut sh, key('l'));
        assert!(
            nodes(&sh, &v)
                .iter()
                .any(|(_, n)| n == "OwnerController.show")
        );
    }

    #[test]
    fn enter_focuses_a_node_and_back_restores_the_place() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        let find = sym(&sh, "OwnerService.find");
        let show = sym(&sh, "OwnerController.show");
        v.show_symbol(&sh, find);
        select_node(&sh, &mut v, "OwnerController.show");
        v.on_key(&mut sh, key('l'));
        let cursor = v.pos().cursor;
        v.on_key(&mut sh, code(KeyCode::Enter));
        assert_eq!(v.symbol.as_ref().unwrap().root, show);
        assert_eq!(v.back.len(), 2);
        // Backspace: previous root, cursor and expansion kept.
        v.on_key(&mut sh, code(KeyCode::Backspace));
        assert_eq!(v.symbol.as_ref().unwrap().root, find);
        assert_eq!(v.pos().cursor, cursor);
        assert!(nodes(&sh, &v).contains(&(1, "Admin.run".to_string())));
        // Ctrl-o: back to the changed symbols.
        v.on_key(
            &mut sh,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        );
        assert_eq!(v.mode, Mode::Changed);
        // m cycles through the modes; the symbol view is still there.
        v.on_key(&mut sh, key('m'));
        assert_eq!(v.mode, Mode::Modules);
        v.on_key(&mut sh, key('m'));
        assert_eq!(v.mode, Mode::Symbol);
    }

    #[test]
    fn cycles_are_not_expanded() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        v.show_symbol(&sh, sym(&sh, "Admin.a"));
        select_node(&sh, &mut v, "Admin.b");
        v.on_key(&mut sh, key('l'));
        let cycle = v.tree.iter().any(|r| {
            matches!(
                r,
                TreeRow::Node {
                    depth: 1,
                    cycle: true,
                    expandable: false,
                    ..
                }
            )
        });
        assert!(cycle, "{:?}", v.tree);
    }

    #[test]
    fn gd_and_gs_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        let find = sym(&sh, "OwnerService.find");
        v.show_symbol(&sh, find);
        v.on_key(&mut sh, key('g'));
        assert_eq!(v.on_key(&mut sh, key('d')), Action::ShowHunk(0));
        // An unchanged symbol has no hunk to show.
        select_node(&sh, &mut v, "OwnerController.show");
        v.on_key(&mut sh, key('g'));
        assert_eq!(v.on_key(&mut sh, key('d')), Action::None);
        assert!(sh.message().unwrap().contains("not changed"));

        // gs from the Diff view, on the added line.
        let mut d = DiffView::new(&sh);
        let row = d
            .rows
            .iter()
            .position(|r| matches!(r, Row::Line { line: 1, .. }))
            .unwrap();
        d.pos.cursor = row;
        d.on_key(&mut sh, key('g'));
        assert_eq!(d.on_key(&mut sh, key('s')), Action::ShowSymbol(find));
    }

    #[test]
    fn module_map_drills_into_changed_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        v.on_key(&mut sh, key('m'));
        assert_eq!(v.mode, Mode::Modules);
        let map = v.modules.as_ref().unwrap();
        let owner = map
            .modules
            .iter()
            .position(|m| m.name == "app.owner")
            .unwrap();
        let i = v
            .mod_rows
            .iter()
            .position(|r| *r == ModRow::Module(owner))
            .unwrap();
        v.select(i);
        v.on_key(&mut sh, code(KeyCode::Enter));
        let find = sym(&sh, "OwnerService.find");
        assert!(v.mod_rows.contains(&ModRow::Symbol {
            module: owner,
            sym: find
        }));
        v.on_key(&mut sh, key('j'));
        v.on_key(&mut sh, code(KeyCode::Enter));
        assert_eq!(v.mode, Mode::Symbol);
        v.on_key(&mut sh, code(KeyCode::Backspace));
        assert_eq!(v.mode, Mode::Modules);
    }

    #[test]
    fn search_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        v.on_key(&mut sh, key('/'));
        assert!(v.captures_keys());
        for c in "Admin.run".chars() {
            v.on_key(&mut sh, key(c));
        }
        v.on_key(&mut sh, code(KeyCode::Enter));
        assert_eq!(v.mode, Mode::Symbol);
        assert_eq!(v.symbol.as_ref().unwrap().root, sym(&sh, "Admin.run"));
        // Several matches (part of a name): a result list.
        v.on_key(&mut sh, key('/'));
        for c in "own".chars() {
            v.on_key(&mut sh, key(c));
        }
        v.on_key(&mut sh, code(KeyCode::Enter));
        assert_eq!(v.mode, Mode::Found);
        assert!(v.found.iter().any(|r| matches!(r, ListRow::Symbol(_))));
    }

    #[test]
    fn names_any_edge_kind() {
        assert_eq!(kind_name(EdgeKind::HttpCalls), "http calls");
        assert_eq!(
            kind_name(survol_core::graph::Role::EntryPoint),
            "entry point"
        );
        assert_eq!(Rel::In(EdgeKind::Injects).label(), "← injects");
    }
}
