//! Code graph of a review: symbols of the head revision, the edges between
//! them, and which symbols each hunk changes.
//!
//! Built from the [`crate::index`] by heuristic resolution (names, imports,
//! packages, typed receivers, arity), see [`resolve`]. Every edge carries a
//! `confidence` in `0.0..=1.0`: an ambiguous reference gives several edges of
//! lower confidence, an unresolvable one gives none. Nothing is invented, and
//! the LLM never adds edges.
//!
//! Framework knowledge (Spring, Angular...) plugs in through [`FrameworkRule`]:
//! rules run after generic resolution and can add edges and mark symbols with
//! [`Role`]s and tags. See [`rules`].

mod build;
mod modules;
mod resolve;
pub mod rules;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

pub use crate::index::{Annotation, AnnotationArg, Lang, Span, SymbolKind};
pub use build::{GraphBuilder, build, cache_key, cache_path, is_test_path, load_cache, save_cache};
pub use modules::{ModuleEdge, ModuleMap, ModuleNode};
pub use rules::{FrameworkRule, default_rules};

/// Version of the graph construction: part of the cache key. Bump it when
/// resolution or the built-in rules change.
pub const GRAPH_VERSION: u32 = 1;

/// Index of a symbol in [`Graph::symbols`].
pub type SymIdx = u32;

/// What a symbol is for, beyond its syntax. Set by the builder (`Test`) and
/// by framework rules (the others).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Where a flow starts: HTTP endpoint, listener, scheduled job, route, CLI.
    EntryPoint,
    /// Managed component: `@Service`, `@Component`, `@Injectable`...
    Component,
    /// Persistence access: `@Repository`, `JpaRepository`...
    Repository,
    /// Configuration: `@Configuration`, `@Bean`, `environment.ts`...
    Config,
    /// Persistent entity: `@Entity`...
    Entity,
    /// UI component (Angular `@Component`...).
    View,
    /// Declared in a test file.
    Test,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    /// Stable id: `path#Outer.Inner.method/2` (`/n`: parameter count of a
    /// callable, `~k`: k-th definition with the same signature), or the path
    /// alone for a [`SymbolKind::File`].
    pub id: String,
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub lang: Lang,
    /// Whole definition, annotations included.
    pub span: Span,
    /// Line of the name: where to jump.
    pub line: u32,
    /// Enclosing symbol (the file symbol for top-level definitions).
    pub container: Option<SymIdx>,
    /// A hunk changes this symbol directly: it is the innermost symbol
    /// around at least one added or removed line.
    pub changed: bool,
    /// Only exists in the base revision (deleted, or renamed away).
    /// Removed symbols have no edges.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
    /// Declared parameters of a callable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<u16>,
    /// The last parameter is variadic.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub variadic: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<Role>,
    /// Free-form facts set by rules, e.g. `http.route = "GET /owners/{id}"`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,
}

impl Symbol {
    pub fn has_role(&self, role: Role) -> bool {
        self.roles.contains(&role)
    }

    pub fn annotation(&self, name: &str) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.name == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Call, method reference or instantiation (to the constructor, or the class).
    Calls,
    /// File → imported symbol (or file).
    Imports,
    /// Type → supertype (extends and implements).
    Inherits,
    /// Method → the supertype method it overrides or implements.
    Overrides,
    /// Symbol → type it mentions (signature, field, variable, generic).
    Uses,
    /// Test symbol → symbol it exercises.
    Tests,
    /// Injection point → injected implementation (framework rules).
    Injects,
    /// Front-end HTTP call → back-end endpoint (framework rules).
    HttpCalls,
    /// Configuration → configured symbol (framework rules).
    Configures,
    /// Route or template → component it displays (framework rules).
    Routes,
}

impl EdgeKind {
    /// Kinds meaning "`from` depends on `to`", used for ordering and modules.
    pub fn is_dependency(self) -> bool {
        !matches!(self, EdgeKind::Tests)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from: SymIdx,
    pub to: SymIdx,
    pub kind: EdgeKind,
    /// `1.0`: certain; `< 0.5`: guess among several candidates.
    pub confidence: f32,
    /// Line of the reference in `from`'s file (0 when not applicable).
    pub line: u32,
}

/// A file of the head revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub lang: Lang,
    /// Java/Kotlin package, else the directory.
    pub module: String,
    /// Part of the diff.
    pub changed: bool,
    /// Test file by convention, see [`is_test_path`].
    pub test: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphStats {
    pub files: usize,
    pub symbols: usize,
    pub edges: usize,
    /// Call and type references examined.
    pub refs: usize,
    /// References that gave at least one edge.
    pub resolved: usize,
    /// References dropped because too many candidates matched.
    pub ambiguous: usize,
    /// Time to resolve and apply rules (the index is timed separately).
    pub millis: u64,
    pub index: crate::index::IndexStats,
    /// Framework rules applied, in order.
    pub rules: Vec<String>,
}

/// A neighbour of a symbol in a query result.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub symbol: SymIdx,
    pub kind: EdgeKind,
    pub confidence: f32,
    /// Line of the reference, in the caller's file for [`Graph::callers`].
    pub line: u32,
    /// The link goes through this symbol: the overridden method for a
    /// caller through dynamic dispatch, the class for a test of the class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<SymIdx>,
}

/// Serialized form of a [`Graph`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphData {
    /// Cache key, see [`cache_key`].
    pub key: String,
    pub base_sha: String,
    pub head_sha: String,
    pub files: Vec<FileInfo>,
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
    /// Symbols changed by each hunk, indexed by hunk id.
    pub hunk_symbols: Vec<Vec<SymIdx>>,
    pub stats: GraphStats,
}

/// The graph with its lookup tables. Read-only once built; serializes as
/// [`GraphData`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(from = "GraphData", into = "GraphData")]
pub struct Graph {
    data: GraphData,
    by_id: HashMap<String, SymIdx>,
    by_name: HashMap<String, Vec<SymIdx>>,
    /// Head symbols of each file, outermost first.
    by_file: HashMap<String, Vec<SymIdx>>,
    file_info: HashMap<String, usize>,
    out: Vec<Vec<u32>>,
    inn: Vec<Vec<u32>>,
    symbol_hunks: HashMap<SymIdx, Vec<usize>>,
    /// Loaded from the cache rather than computed.
    pub from_cache: bool,
}

impl From<GraphData> for Graph {
    fn from(data: GraphData) -> Self {
        Graph::new(data)
    }
}

impl From<Graph> for GraphData {
    fn from(g: Graph) -> Self {
        g.data
    }
}

impl Graph {
    pub fn new(data: GraphData) -> Self {
        let n = data.symbols.len();
        let mut g = Graph {
            out: vec![Vec::new(); n],
            inn: vec![Vec::new(); n],
            ..Default::default()
        };
        for (i, s) in data.symbols.iter().enumerate() {
            let i = i as SymIdx;
            g.by_id.insert(s.id.clone(), i);
            g.by_name.entry(s.name.clone()).or_default().push(i);
            if !s.removed {
                g.by_file.entry(s.file.clone()).or_default().push(i);
            }
        }
        for (e, edge) in data.edges.iter().enumerate() {
            g.out[edge.from as usize].push(e as u32);
            g.inn[edge.to as usize].push(e as u32);
        }
        for (h, syms) in data.hunk_symbols.iter().enumerate() {
            for &s in syms {
                g.symbol_hunks.entry(s).or_default().push(h);
            }
        }
        g.file_info = data
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
        g.data = data;
        g
    }

    pub fn data(&self) -> &GraphData {
        &self.data
    }

    pub fn symbols(&self) -> &[Symbol] {
        &self.data.symbols
    }

    pub fn symbol(&self, s: SymIdx) -> &Symbol {
        &self.data.symbols[s as usize]
    }

    pub fn edges(&self) -> &[Edge] {
        &self.data.edges
    }

    pub fn files(&self) -> &[FileInfo] {
        &self.data.files
    }

    pub fn file(&self, path: &str) -> Option<&FileInfo> {
        self.file_info.get(path).map(|&i| &self.data.files[i])
    }

    /// The file is part of the diff.
    pub fn is_file_changed(&self, path: &str) -> bool {
        self.file(path).is_some_and(|f| f.changed)
    }

    pub fn stats(&self) -> &GraphStats {
        &self.data.stats
    }

    pub fn by_id(&self, id: &str) -> Option<SymIdx> {
        self.by_id.get(id).copied()
    }

    /// Symbols named `name`. `Owner.find` matches `find` in a container
    /// named `Owner`.
    pub fn find(&self, name: &str) -> Vec<SymIdx> {
        let (container, name) = match name.rsplit_once('.') {
            Some((c, n)) => (Some(c.rsplit('.').next().unwrap_or(c)), n),
            None => (None, name),
        };
        let Some(all) = self.by_name.get(name) else {
            return Vec::new();
        };
        all.iter()
            .copied()
            .filter(|&s| {
                container.is_none_or(|c| {
                    self.symbol(s)
                        .container
                        .is_some_and(|p| self.symbol(p).name == c)
                })
            })
            .collect()
    }

    /// Innermost head symbol of `file` around `line` (the file symbol when
    /// the line is outside every definition).
    pub fn symbol_at(&self, file: &str, line: u32) -> Option<SymIdx> {
        self.by_file
            .get(file)?
            .iter()
            .copied()
            .filter(|&s| self.symbol(s).span.contains(line))
            .min_by_key(|&s| self.symbol(s).span.lines())
    }

    /// Head symbols of `file`, outermost first.
    pub fn symbols_in_file(&self, file: &str) -> &[SymIdx] {
        self.by_file.get(file).map_or(&[], Vec::as_slice)
    }

    /// Symbols changed by hunk `hunk` (innermost around its changed lines).
    pub fn symbols_of_hunk(&self, hunk: usize) -> &[SymIdx] {
        self.data.hunk_symbols.get(hunk).map_or(&[], Vec::as_slice)
    }

    /// Hunks changing `s` directly.
    pub fn hunks_of_symbol(&self, s: SymIdx) -> &[usize] {
        self.symbol_hunks.get(&s).map_or(&[], Vec::as_slice)
    }

    /// Changed symbols, in file order.
    pub fn changed_symbols(&self) -> Vec<SymIdx> {
        (0..self.data.symbols.len() as SymIdx)
            .filter(|&s| self.symbol(s).changed)
            .collect()
    }

    /// Outgoing edges of `s`.
    pub fn edges_from(&self, s: SymIdx) -> impl Iterator<Item = &Edge> {
        self.out
            .get(s as usize)
            .into_iter()
            .flatten()
            .map(|&e| &self.data.edges[e as usize])
    }

    /// Incoming edges of `s`.
    pub fn edges_to(&self, s: SymIdx) -> impl Iterator<Item = &Edge> {
        self.inn
            .get(s as usize)
            .into_iter()
            .flatten()
            .map(|&e| &self.data.edges[e as usize])
    }

    /// Who calls `s`: direct callers, then callers of the methods `s`
    /// overrides (dynamic dispatch, `via` the overridden method, confidence
    /// lowered). Sorted by decreasing confidence.
    pub fn callers(&self, s: SymIdx) -> Vec<Link> {
        let mut out: Vec<Link> = self
            .edges_to(s)
            .filter(|e| e.kind == EdgeKind::Calls)
            .map(|e| link(e.from, e, None, 1.0))
            .collect();
        for o in self.edges_from(s).filter(|e| e.kind == EdgeKind::Overrides) {
            out.extend(
                self.edges_to(o.to)
                    .filter(|e| e.kind == EdgeKind::Calls)
                    .map(|e| link(e.from, e, Some(o.to), 0.7 * o.confidence)),
            );
        }
        sort_links(out)
    }

    /// What `s` calls, sorted by decreasing confidence.
    pub fn callees(&self, s: SymIdx) -> Vec<Link> {
        sort_links(
            self.edges_from(s)
                .filter(|e| e.kind == EdgeKind::Calls)
                .map(|e| link(e.to, e, None, 1.0))
                .collect(),
        )
    }

    /// Tests exercising `s`: direct ones, then test classes and files of its
    /// enclosing types (`via` the type, confidence halved). Test methods that
    /// merely instantiate an enclosing type are not listed.
    pub fn tests_of(&self, s: SymIdx) -> Vec<Link> {
        let mut out: Vec<Link> = self
            .edges_to(s)
            .filter(|e| e.kind == EdgeKind::Tests)
            .map(|e| link(e.from, e, None, 1.0))
            .collect();
        let mut c = self.symbol(s).container;
        while let Some(t) = c {
            if self.symbol(t).kind.is_type() {
                out.extend(
                    self.edges_to(t)
                        .filter(|e| e.kind == EdgeKind::Tests)
                        .filter(|e| {
                            let k = self.symbol(e.from).kind;
                            k.is_type() || k == SymbolKind::File
                        })
                        .map(|e| link(e.from, e, Some(t), 0.5)),
                );
            }
            c = self.symbol(t).container;
        }
        sort_links(out)
    }

    /// Package / directory level dependency graph.
    pub fn module_map(&self) -> ModuleMap {
        modules::module_map(self)
    }

    /// Module of `s`: its file's package or directory.
    pub fn module_of(&self, s: SymIdx) -> Option<&str> {
        self.file(&self.symbol(s).file).map(|f| f.module.as_str())
    }

    /// `Outer.method` for display.
    pub fn display_name(&self, s: SymIdx) -> String {
        let sym = self.symbol(s);
        match sym.container.map(|c| self.symbol(c)) {
            Some(c) if c.kind != SymbolKind::File => format!("{}.{}", c.name, sym.name),
            _ => sym.name.clone(),
        }
    }
}

fn link(symbol: SymIdx, e: &Edge, via: Option<SymIdx>, factor: f32) -> Link {
    Link {
        symbol,
        kind: e.kind,
        confidence: e.confidence * factor,
        line: e.line,
        via,
    }
}

/// Best confidence first; one link per symbol (the best one).
fn sort_links(mut links: Vec<Link>) -> Vec<Link> {
    links.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then(a.symbol.cmp(&b.symbol))
    });
    let mut seen = std::collections::HashSet::new();
    links.retain(|l| seen.insert(l.symbol));
    links
}
