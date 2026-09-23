//! What to ask the language servers about a graph, and how their answers
//! change its edges. Pure logic: the servers are behind [`Backend`].
//!
//! Only edges touching changed callables are examined:
//! - `references` on each changed callable: its call sites become callers
//!   (confirmed or new edges);
//! - `definition` at the site of each uncertain call edge (confidence < 1)
//!   from or to a changed callable: the edge is confirmed, or replaced by
//!   the edge to the real target (same-arity overloads, receivers typed by
//!   inference), or dropped when the target is a library;
//! - `definition` at the calls of a changed callable that the heuristic
//!   left unresolved or uncertain (callees).
//!
//! An edge confirmed by any answer stays; one only contradicted is removed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::graph::{Edge, EdgeKind, Graph, GraphData, Role, SymIdx, SymbolKind};
use crate::index::Lang;

use super::{Loc, LspError};

/// Something that answers definition and references requests.
pub trait Backend {
    fn definition(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError>;
    fn references(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError>;
}

impl Backend for super::server::Server {
    fn definition(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError> {
        super::server::Server::definition(self, path, line, column)
    }

    fn references(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError> {
        super::server::Server::references(self, path, line, column)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskKind {
    /// Call sites of this changed callable.
    References(SymIdx),
    /// What `name`, called from `from`, is.
    Definition { from: SymIdx, name: String },
}

/// One question to a server: a position (several columns when the name
/// occurs more than once on the line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub kind: TaskKind,
    pub path: String,
    pub line: u32,
    /// UTF-16 columns of the name on the line.
    pub columns: Vec<u32>,
    pub lang: Lang,
}

impl Task {
    /// Asks `backend`, merging the answers of every column.
    pub fn run(&self, backend: &mut dyn Backend) -> Result<Vec<Loc>, LspError> {
        let mut out: Vec<Loc> = Vec::new();
        let mut last_err = None;
        let mut ok = false;
        for &c in &self.columns {
            let res = match self.kind {
                TaskKind::References(_) => backend.references(&self.path, self.line, c),
                TaskKind::Definition { .. } => backend.definition(&self.path, self.line, c),
            };
            match res {
                Ok(locs) => {
                    ok = true;
                    for l in locs {
                        if !out.contains(&l) {
                            out.push(l);
                        }
                    }
                }
                Err(e) => last_err = Some(e),
            }
        }
        match (ok, last_err) {
            (false, Some(e)) => Err(e),
            _ => Ok(out),
        }
    }
}

/// Lines of the worktree files, read on demand.
pub struct Texts {
    root: PathBuf,
    files: HashMap<String, Option<Vec<String>>>,
}

impl Texts {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            files: HashMap::new(),
        }
    }

    /// Line `line` (1-based) of `path`.
    pub fn line(&mut self, path: &str, line: u32) -> Option<&str> {
        let root = &self.root;
        let lines = self.files.entry(path.to_string()).or_insert_with(|| {
            std::fs::read_to_string(root.join(path))
                .ok()
                .map(|t| t.lines().map(str::to_string).collect())
        });
        lines
            .as_ref()?
            .get(line.checked_sub(1)? as usize)
            .map(String::as_str)
    }
}

/// Changed callables of the head that a server can be asked about.
fn changed_callables(g: &Graph) -> Vec<SymIdx> {
    g.changed_symbols()
        .into_iter()
        .filter(|&s| {
            let sym = g.symbol(s);
            !sym.removed && sym.kind.is_callable() && sym.lang.is_code()
        })
        .collect()
}

/// The questions worth asking about `g`, most useful first: references of
/// changed callables, uncertain call edges, then calls made by changed
/// callables.
pub fn plan(g: &Graph, texts: &mut Texts) -> Vec<Task> {
    let changed = changed_callables(g);
    let is_changed = |s: SymIdx| g.symbol(s).changed && !g.symbol(s).removed;
    let mut tasks = Vec::new();
    for &s in &changed {
        let sym = g.symbol(s);
        let Some(text) = texts.line(&sym.file, sym.line) else {
            continue;
        };
        if let Some(&c) = word_columns(text, &sym.name).first() {
            tasks.push(Task {
                kind: TaskKind::References(s),
                path: sym.file.clone(),
                line: sym.line,
                columns: vec![c],
                lang: sym.lang,
            });
        }
    }
    let mut sites: HashSet<(String, u32, String)> = HashSet::new();
    let mut site = |tasks: &mut Vec<Task>,
                    texts: &mut Texts,
                    from: SymIdx,
                    line: u32,
                    name: &str,
                    calls_only: bool| {
        let file = &g.symbol(from).file;
        if !sites.insert((file.clone(), line, name.to_string())) {
            return;
        }
        let Some(text) = texts.line(file, line) else {
            return;
        };
        let mut columns = word_columns(text, name);
        if calls_only {
            columns.retain(|&c| is_call(text, c, name));
        }
        if columns.is_empty() {
            return;
        }
        tasks.push(Task {
            kind: TaskKind::Definition {
                from,
                name: name.to_string(),
            },
            path: file.clone(),
            line,
            columns,
            lang: g.symbol(from).lang,
        });
    };
    for e in g.edges() {
        if e.kind != EdgeKind::Calls
            || e.confidence >= 1.0
            || e.line == 0
            || g.symbol(e.from).removed
            || !(is_changed(e.from) || is_changed(e.to))
        {
            continue;
        }
        let name = g.symbol(e.to).name.clone();
        site(&mut tasks, texts, e.from, e.line, &name, false);
    }
    // Calls of the changed callables that are unresolved or uncertain.
    for &s in &changed {
        let sym = g.symbol(s).clone();
        for line in sym.span.start..=sym.span.end {
            let Some(text) = texts.line(&sym.file, line).map(str::to_string) else {
                break;
            };
            let Some(from) = g.symbol_at(&sym.file, line) else {
                continue;
            };
            for name in called_names(&text) {
                if (name == sym.name && line == sym.line) || !is_known_callee(g, &name) {
                    continue;
                }
                let certain = g.edges_from(from).any(|e| {
                    e.kind == EdgeKind::Calls
                        && e.line == line
                        && e.confidence >= 1.0
                        && g.symbol(e.to).name == name
                });
                if !certain {
                    site(&mut tasks, texts, from, line, &name, true);
                }
            }
        }
    }
    tasks
}

/// Some head symbol a call to `name` could reach.
fn is_known_callee(g: &Graph, name: &str) -> bool {
    g.find(name).into_iter().any(|s| {
        let sym = g.symbol(s);
        !sym.removed && (sym.kind.is_callable() || sym.kind.is_type())
    })
}

/// How the answers changed the graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    /// Existing edges confirmed (confidence raised to 1).
    pub confirmed: usize,
    /// Edges contradicted and removed.
    pub removed: usize,
    /// Edges found by the servers only.
    pub added: usize,
}

struct Edges {
    edges: Vec<Edge>,
    alive: Vec<bool>,
    ids: HashMap<(SymIdx, SymIdx, EdgeKind), usize>,
    confirmed: HashSet<usize>,
    contradicted: HashSet<usize>,
    added: usize,
}

impl Edges {
    fn new(edges: &[Edge]) -> Self {
        Self {
            ids: edges
                .iter()
                .enumerate()
                .map(|(i, e)| ((e.from, e.to, e.kind), i))
                .collect(),
            alive: vec![true; edges.len()],
            edges: edges.to_vec(),
            confirmed: HashSet::new(),
            contradicted: HashSet::new(),
            added: 0,
        }
    }

    fn get(&self, from: SymIdx, to: SymIdx, kind: EdgeKind) -> Option<usize> {
        self.ids.get(&(from, to, kind)).copied()
    }

    /// Confirms the edge, adding it when missing.
    fn confirm(&mut self, from: SymIdx, to: SymIdx, kind: EdgeKind, line: u32) {
        if from == to {
            return;
        }
        let i = match self.get(from, to, kind) {
            Some(i) => i,
            None => {
                self.ids.insert((from, to, kind), self.edges.len());
                self.edges.push(Edge {
                    from,
                    to,
                    kind,
                    confidence: 1.0,
                    line,
                    lsp: true,
                });
                self.alive.push(true);
                self.added += 1;
                self.edges.len() - 1
            }
        };
        self.confirmed.insert(i);
    }
}

/// Applies the answers (`results[i]` for `tasks[i]`, `None` when not
/// asked or failed) to `g`.
pub fn apply(
    g: &Graph,
    tasks: &[Task],
    results: &[Option<Vec<Loc>>],
    texts: &mut Texts,
) -> (GraphData, Changes) {
    let mut ed = Edges::new(g.edges());
    let original = g.edges().len();
    let answered = || {
        tasks
            .iter()
            .zip(results)
            .filter_map(|(t, r)| r.as_ref().map(|r| (t, r)))
    };
    for (t, locs) in answered() {
        if let TaskKind::References(s) = t.kind {
            apply_references(g, s, locs, texts, &mut ed);
        }
    }
    for (t, locs) in answered() {
        if let TaskKind::Definition { from, name } = &t.kind {
            apply_definition(g, *from, name, t.line, locs, &mut ed);
        }
    }

    // Confirmed wins over contradicted (an edge merges every call site of
    // a pair, the contradiction may come from another site).
    let mut changes = Changes {
        added: ed.added,
        ..Default::default()
    };
    for &i in &ed.confirmed {
        let e = &mut ed.edges[i];
        if i < original && !e.lsp {
            changes.confirmed += 1;
        }
        e.confidence = 1.0;
        e.lsp = true;
    }
    for &i in &ed.contradicted {
        if !ed.confirmed.contains(&i) {
            ed.alive[i] = false;
            changes.removed += 1;
        }
    }
    sync_tests(g, &mut ed);

    let mut data = g.data().clone();
    data.edges = ed
        .edges
        .into_iter()
        .zip(ed.alive)
        .filter_map(|(e, alive)| alive.then_some(e))
        .collect();
    data.stats.edges = data.edges.len();
    (data, changes)
}

fn apply_references(g: &Graph, s: SymIdx, locs: &[Loc], texts: &mut Texts, ed: &mut Edges) {
    let sym = g.symbol(s);
    for loc in locs {
        let Some(path) = &loc.path else { continue };
        if *path == sym.file && loc.line == sym.line {
            continue;
        }
        let Some(text) = texts.line(path, loc.line) else {
            continue;
        };
        let call = if text_at(text, loc.column).starts_with(sym.name.as_str()) {
            is_call(text, loc.column, &sym.name)
        } else {
            word_columns(text, &sym.name)
                .into_iter()
                .any(|c| is_call(text, c, &sym.name))
        };
        if !call {
            continue;
        }
        let Some(from) = g.symbol_at(path, loc.line) else {
            continue;
        };
        ed.confirm(from, s, EdgeKind::Calls, loc.line);
    }
}

fn apply_definition(g: &Graph, from: SymIdx, name: &str, line: u32, locs: &[Loc], ed: &mut Edges) {
    let targets: Vec<SymIdx> = locs.iter().filter_map(|l| map_loc(g, l, name)).collect();
    let at_site: Vec<usize> = g
        .edges_from(from)
        .filter(|e| e.kind == EdgeKind::Calls && e.line == line && g.symbol(e.to).name == name)
        .filter_map(|e| ed.get(e.from, e.to, e.kind))
        .collect();
    if !targets.is_empty() {
        for &i in &at_site {
            let to = ed.edges[i].to;
            if targets.iter().any(|&t| same_target(g, to, t)) {
                ed.confirmed.insert(i);
            } else {
                ed.contradicted.insert(i);
            }
        }
        for &t in &targets {
            let known = at_site.iter().any(|&i| same_target(g, ed.edges[i].to, t));
            if !known {
                ed.confirm(from, t, EdgeKind::Calls, line);
            }
        }
    } else if !locs.is_empty() && locs.iter().all(|l| l.path.is_none()) {
        // A library method: the heuristic guessed a workspace symbol.
        ed.contradicted.extend(at_site);
    }
}

/// `a` and `b` are the same call target: equal, or a class and its
/// constructor (an instantiation may point at either).
fn same_target(g: &Graph, a: SymIdx, b: SymIdx) -> bool {
    let ctor_of = |c: SymIdx, t: SymIdx| {
        g.symbol(c).kind == SymbolKind::Constructor && g.symbol(c).container == Some(t)
    };
    a == b || ctor_of(a, b) || ctor_of(b, a)
}

/// The head symbol named `name` declared at `loc`.
fn map_loc(g: &Graph, loc: &Loc, name: &str) -> Option<SymIdx> {
    let path = loc.path.as_deref()?;
    let syms = g.symbols_in_file(path);
    let named = || {
        syms.iter()
            .copied()
            .filter(|&s| g.symbol(s).name == name && g.symbol(s).kind != SymbolKind::File)
    };
    let innermost =
        |it: &mut dyn Iterator<Item = SymIdx>| it.min_by_key(|&s| g.symbol(s).span.lines());
    innermost(&mut named().filter(|&s| g.symbol(s).line == loc.line))
        .or_else(|| innermost(&mut named().filter(|&s| g.symbol(s).span.start == loc.line)))
}

/// Mirrors call edge changes from tests onto their `tests` edges.
fn sync_tests(g: &Graph, ed: &mut Edges) {
    let is_test = |s: SymIdx| g.symbol(s).has_role(Role::Test);
    for i in 0..ed.edges.len() {
        let e = ed.edges[i];
        if e.kind != EdgeKind::Calls || !is_test(e.from) || is_test(e.to) {
            continue;
        }
        let t = ed.get(e.from, e.to, EdgeKind::Tests);
        match (ed.alive[i], t) {
            (false, Some(t)) => ed.alive[t] = false,
            (true, Some(t)) if e.lsp => {
                ed.edges[t].confidence = 1.0;
                ed.edges[t].lsp = true;
            }
            (true, None) if e.lsp => {
                ed.ids
                    .insert((e.from, e.to, EdgeKind::Tests), ed.edges.len());
                ed.edges.push(Edge {
                    kind: EdgeKind::Tests,
                    ..e
                });
                ed.alive.push(true);
            }
            _ => {}
        }
    }
}

// ---- text helpers

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// UTF-16 columns where `name` occurs as a whole word in `text`.
pub fn word_columns(text: &str, name: &str) -> Vec<u32> {
    if name.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, _) in text.match_indices(name) {
        let before = text[..i].chars().next_back();
        let after = text[i + name.len()..].chars().next();
        if before.is_some_and(is_ident) || after.is_some_and(is_ident) {
            continue;
        }
        out.push(text[..i].encode_utf16().count() as u32);
    }
    out
}

/// The rest of `text` from UTF-16 column `column`.
fn text_at(text: &str, column: u32) -> &str {
    let mut units = 0u32;
    for (i, c) in text.char_indices() {
        if units >= column {
            return &text[i..];
        }
        units += c.len_utf16() as u32;
    }
    ""
}

/// `name` at `column` is called (`name(`, `name<T>(`, `name {` in Kotlin)
/// or referenced as a method (`::name`).
pub fn is_call(text: &str, column: u32, name: &str) -> bool {
    let rest = text_at(text, column);
    let Some(after) = rest.strip_prefix(name) else {
        return false;
    };
    let before = &text[..text.len() - rest.len()];
    if before.trim_end().ends_with("::") {
        return true;
    }
    let mut after = after.trim_start();
    if after.starts_with('<') {
        // Generic arguments: `name<A, B<C>>(`.
        let mut depth = 0;
        let mut end = None;
        for (i, c) in after.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        match end {
            Some(e) => after = after[e..].trim_start(),
            None => return false,
        }
    }
    after.starts_with('(') || after.starts_with('{')
}

/// Names directly followed by `(` in a line of code (candidate calls).
fn called_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (start, c) = chars[i];
        if is_ident(c) && !c.is_numeric() && (i == 0 || !is_ident(chars[i - 1].1)) {
            let mut j = i;
            while j < chars.len() && is_ident(chars[j].1) {
                j += 1;
            }
            let end = chars.get(j).map_or(text.len(), |&(k, _)| k);
            let name = &text[start..end];
            if is_call(text, text[..start].encode_utf16().count() as u32, name)
                && !KEYWORDS.contains(&name)
                && !out.iter().any(|n| n == name)
            {
                out.push(name.to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

const KEYWORDS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "return",
    "synchronized",
    "super",
    "this",
    "when",
    "fun",
    "function",
    "new",
    "try",
    "else",
    "do",
    "throw",
    "typeof",
    "await",
    "init",
    "constructor",
];
