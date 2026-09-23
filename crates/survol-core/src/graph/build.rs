//! Graph construction: symbols, hunk mapping, generic resolution, rules.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::resolve::Tables;
use super::{
    Edge, EdgeKind, FileInfo, FrameworkRule, GRAPH_VERSION, Graph, GraphData, GraphStats, Role,
    SymIdx, Symbol, SymbolKind,
};
use crate::index::{FileIndex, INDEX_VERSION, Index, RefKind, Span};
use crate::model::{Diff, LineKind};

/// Builds the graph of `index` (head files + base versions) for `diff`, then
/// applies `rules` in order.
pub fn build(index: &Index, diff: &Diff, rules: &[Box<dyn FrameworkRule>]) -> Graph {
    let started = Instant::now();
    let mut b = GraphBuilder::new(index, diff);
    b.map_hunks(diff);
    b.tables = Tables::new(&b);
    b.resolve_all();
    for rule in rules {
        rule.apply(index, &mut b);
        b.stats.rules.push(rule.name().to_string());
    }
    b.stats.millis = started.elapsed().as_millis() as u64;
    b.finish()
}

/// The graph under construction, handed to [`FrameworkRule::apply`].
///
/// Symbols of head files are addressed by [`GraphBuilder::def_symbol`]
/// (file index in [`Index::files`] + definition index), which lets rules walk
/// the index (annotations, bindings, refs) and act on the matching symbols.
pub struct GraphBuilder<'a> {
    pub(super) index: &'a Index,
    pub(super) files: Vec<FileInfo>,
    pub(super) symbols: Vec<Symbol>,
    pub(super) edges: Vec<Edge>,
    edge_ids: HashMap<(SymIdx, SymIdx, EdgeKind), usize>,
    /// Per head file: its file symbol, then one symbol per definition.
    pub(super) file_syms: Vec<SymIdx>,
    pub(super) def_syms: Vec<Vec<SymIdx>>,
    pub(super) hunk_symbols: Vec<Vec<SymIdx>>,
    pub(super) tables: Tables,
    pub(super) stats: GraphStats,
}

impl<'a> GraphBuilder<'a> {
    fn new(index: &'a Index, diff: &Diff) -> Self {
        let changed: HashSet<&str> = diff.files.iter().map(|f| f.path.as_str()).collect();
        let mut b = GraphBuilder {
            index,
            files: Vec::new(),
            symbols: Vec::new(),
            edges: Vec::new(),
            edge_ids: HashMap::new(),
            file_syms: Vec::new(),
            def_syms: Vec::new(),
            hunk_symbols: vec![Vec::new(); diff.hunks.len()],
            tables: Tables::default(),
            stats: GraphStats {
                index: index.stats.clone(),
                ..Default::default()
            },
        };
        for f in &index.files {
            let test = is_test_path(&f.path);
            b.files.push(FileInfo {
                path: f.path.clone(),
                lang: f.lang,
                module: module_of(f),
                changed: changed.contains(f.path.as_str()),
                test,
            });
            let (file_sym, defs) = b.add_file_symbols(f);
            if test {
                for &s in std::iter::once(&file_sym).chain(&defs) {
                    b.symbols[s as usize].roles.push(Role::Test);
                }
            }
            b.file_syms.push(file_sym);
            b.def_syms.push(defs);
        }
        b
    }

    /// Adds the file symbol and one symbol per definition of `f`.
    fn add_file_symbols(&mut self, f: &FileIndex) -> (SymIdx, Vec<SymIdx>) {
        let file_sym = self.push(Symbol {
            id: f.path.clone(),
            name: f.path.rsplit('/').next().unwrap_or(&f.path).to_string(),
            kind: SymbolKind::File,
            file: f.path.clone(),
            lang: f.lang,
            span: Span {
                start: 1,
                end: f.lines.max(1),
            },
            line: 1,
            container: None,
            changed: false,
            removed: false,
            annotations: Vec::new(),
            params: None,
            variadic: false,
            roles: Vec::new(),
            tags: Default::default(),
        });
        let ids = f.def_ids();
        let mut defs: Vec<SymIdx> = Vec::with_capacity(f.defs.len());
        for (d, id) in f.defs.iter().zip(ids) {
            let container = Some(d.parent.map_or(file_sym, |p| defs[p as usize]));
            defs.push(self.push(Symbol {
                id,
                name: d.name.clone(),
                kind: d.kind,
                file: f.path.clone(),
                lang: f.lang,
                span: d.span,
                line: d.line,
                container,
                changed: false,
                removed: false,
                annotations: d.annotations.clone(),
                params: d.params,
                variadic: d.variadic,
                roles: Vec::new(),
                tags: Default::default(),
            }));
        }
        (file_sym, defs)
    }

    fn push(&mut self, s: Symbol) -> SymIdx {
        self.symbols.push(s);
        (self.symbols.len() - 1) as SymIdx
    }

    /// Maps every hunk to the innermost symbols around its changed lines:
    /// head symbols for added lines, base symbols for removed ones (the head
    /// symbol with the same id when it still exists, else a removed symbol).
    fn map_hunks(&mut self, diff: &Diff) {
        let mut ids: HashMap<String, SymIdx> = self
            .symbols
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i as SymIdx))
            .collect();
        let index = self.index;
        let mut base_ids: HashMap<&str, Vec<String>> = HashMap::new();
        for hunk in &diff.hunks {
            let file = &diff.files[hunk.file];
            let head = index.file(&file.path).map(|_| file.path.as_str());
            let old_path = file.old_path.as_deref().unwrap_or(&file.path);
            let base = index.base_file(old_path);
            let mut syms: Vec<SymIdx> = Vec::new();
            for l in &hunk.lines {
                let s = match (l.kind, l.new_line, l.old_line) {
                    (LineKind::Added, Some(n), _) => head.and_then(|p| self.head_symbol_at(p, n)),
                    (LineKind::Removed, _, Some(o)) => base.map(|bf| {
                        let bids = base_ids.entry(old_path).or_insert_with(|| bf.def_ids());
                        self.base_symbol(bf, bids, bf.def_at(o), &file.path, &mut ids)
                    }),
                    _ => None,
                };
                if let Some(s) = s
                    && !syms.contains(&s)
                {
                    syms.push(s);
                }
            }
            for &s in &syms {
                self.symbols[s as usize].changed = true;
            }
            self.hunk_symbols[hunk.id] = syms;
        }
    }

    /// Symbol of definition `def` (`None`: the file) of base file `bf`: the
    /// head symbol with the same id (after a rename, the id under `new_path`),
    /// else a removed symbol, created with its missing containers.
    fn base_symbol(
        &mut self,
        bf: &FileIndex,
        bids: &[String],
        def: Option<usize>,
        new_path: &str,
        ids: &mut HashMap<String, SymIdx>,
    ) -> SymIdx {
        let id = def.map_or_else(|| bf.path.clone(), |d| bids[d].clone());
        let renamed = (bf.path != new_path).then(|| id.replacen(&bf.path, new_path, 1));
        if let Some(&s) = renamed.as_ref().and_then(|r| ids.get(r)).or(ids.get(&id)) {
            return s;
        }
        let container = def.map(|d| {
            let parent = bf.defs[d].parent.map(|p| p as usize);
            self.base_symbol(bf, bids, parent, new_path, ids)
        });
        let sym = match def {
            None => Symbol {
                id: id.clone(),
                name: bf.path.rsplit('/').next().unwrap_or(&bf.path).to_string(),
                kind: SymbolKind::File,
                file: bf.path.clone(),
                lang: bf.lang,
                span: Span {
                    start: 1,
                    end: bf.lines.max(1),
                },
                line: 1,
                container: None,
                changed: false,
                removed: true,
                annotations: Vec::new(),
                params: None,
                variadic: false,
                roles: Vec::new(),
                tags: Default::default(),
            },
            Some(d) => {
                let d = &bf.defs[d];
                Symbol {
                    id: id.clone(),
                    name: d.name.clone(),
                    kind: d.kind,
                    file: bf.path.clone(),
                    lang: bf.lang,
                    span: d.span,
                    line: d.line,
                    container,
                    changed: false,
                    removed: true,
                    annotations: d.annotations.clone(),
                    params: d.params,
                    variadic: d.variadic,
                    roles: Vec::new(),
                    tags: Default::default(),
                }
            }
        };
        let s = self.push(sym);
        ids.insert(id, s);
        s
    }

    fn head_symbol_at(&self, path: &str, line: u32) -> Option<SymIdx> {
        let fi = self.file_index(path)?;
        let f = &self.index.files[fi];
        Some(match f.def_at(line) {
            Some(d) => self.def_syms[fi][d],
            None => self.file_syms[fi],
        })
    }

    fn finish(mut self) -> Graph {
        self.stats.files = self.files.len();
        self.stats.symbols = self.symbols.len();
        self.stats.edges = self.edges.len();
        Graph::new(GraphData {
            key: String::new(),
            base_sha: String::new(),
            head_sha: String::new(),
            files: self.files,
            symbols: self.symbols,
            edges: self.edges,
            hunk_symbols: self.hunk_symbols,
            stats: self.stats,
        })
    }

    // ---- API for framework rules

    pub fn index(&self) -> &'a Index {
        self.index
    }

    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    pub fn symbol(&self, s: SymIdx) -> &Symbol {
        &self.symbols[s as usize]
    }

    /// To set roles and tags.
    pub fn symbol_mut(&mut self, s: SymIdx) -> &mut Symbol {
        &mut self.symbols[s as usize]
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Adds `s` to the roles of a symbol (once).
    pub fn add_role(&mut self, s: SymIdx, role: Role) {
        let roles = &mut self.symbols[s as usize].roles;
        if !roles.contains(&role) {
            roles.push(role);
        }
    }

    /// Index in [`Index::files`] of a head file.
    pub fn file_index(&self, path: &str) -> Option<usize> {
        self.index
            .files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .ok()
    }

    /// Symbol of the file `file` (index in [`Index::files`]).
    pub fn file_symbol(&self, file: usize) -> SymIdx {
        self.file_syms[file]
    }

    /// Symbol of definition `def` of head file `file`.
    pub fn def_symbol(&self, file: usize, def: usize) -> SymIdx {
        self.def_syms[file][def]
    }

    /// Symbol of the scope of a reference or binding (`None`: the file).
    pub fn scope_symbol(&self, file: usize, scope: Option<u32>) -> SymIdx {
        scope.map_or(self.file_syms[file], |d| self.def_syms[file][d as usize])
    }

    /// Adds an edge, or raises the confidence of the same existing edge.
    /// Self edges are ignored.
    pub fn add_edge(
        &mut self,
        from: SymIdx,
        to: SymIdx,
        kind: EdgeKind,
        confidence: f32,
        line: u32,
    ) {
        if from == to {
            return;
        }
        let confidence = confidence.clamp(0.0, 1.0);
        match self.edge_ids.get(&(from, to, kind)) {
            Some(&e) => {
                let edge = &mut self.edges[e];
                edge.confidence = edge.confidence.max(confidence);
            }
            None => {
                self.edge_ids.insert((from, to, kind), self.edges.len());
                self.edges.push(Edge {
                    from,
                    to,
                    kind,
                    confidence,
                    line,
                });
            }
        }
    }

    /// Head symbols named `name`.
    pub fn find(&self, name: &str) -> &[SymIdx] {
        self.tables.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    /// Types named `name` as seen from head file `file` (same file, imports,
    /// package, then global), with the confidence of each.
    pub fn resolve_type(&self, file: usize, name: &str) -> Vec<(SymIdx, f32)> {
        self.tables.resolve_type(self, file, name)
    }

    /// Callable members named `name` of type `ty` or, failing that, of its
    /// nearest supertypes that have one.
    pub fn members(&self, ty: SymIdx, name: &str) -> Vec<SymIdx> {
        self.tables.members_in_hierarchy(ty, name)
    }

    /// Direct supertypes of a type, as resolved.
    pub fn supertypes(&self, ty: SymIdx) -> &[(SymIdx, f32)] {
        self.tables.supers.get(&ty).map_or(&[], Vec::as_slice)
    }

    /// Direct subtypes (implementations) of a type.
    pub fn subtypes(&self, ty: SymIdx) -> &[(SymIdx, f32)] {
        self.tables.subs.get(&ty).map_or(&[], Vec::as_slice)
    }

    /// Head file (index in [`Index::files`]) that the relative module
    /// specifier or path `spec` of file `file` points to (`./x`, `../x.js`,
    /// `./x.component.html`); usual extensions and `index` files tried.
    pub fn resolve_module(&self, file: usize, spec: &str) -> Option<usize> {
        super::resolve::resolve_module(self, &self.index.files[file].path, spec)
    }

    // ---- generic resolution

    fn resolve_all(&mut self) {
        self.resolve_inheritance();
        for fi in 0..self.index.files.len() {
            self.resolve_imports(fi);
            self.resolve_refs(fi);
        }
        self.add_test_edges();
    }

    fn resolve_inheritance(&mut self) {
        let index = self.index;
        for (fi, f) in index.files.iter().enumerate() {
            for r in &f.refs {
                if !matches!(r.kind, RefKind::Extends | RefKind::Implements) {
                    continue;
                }
                let from = self.scope_symbol(fi, r.scope);
                if !self.symbols[from as usize].kind.is_type() {
                    continue;
                }
                let targets = self.resolve_type(fi, &r.name);
                if targets.is_empty() {
                    self.tables.external_super.insert(from);
                }
                for (t, c) in targets {
                    self.add_edge(from, t, EdgeKind::Inherits, c, r.line);
                    self.tables.supers.entry(from).or_default().push((t, c));
                    self.tables.subs.entry(t).or_default().push((from, c));
                }
            }
        }
        self.resolve_overrides();
    }

    /// Method → same-name, same-arity method of the nearest supertype.
    fn resolve_overrides(&mut self) {
        let mut found = Vec::new();
        for (&ty, supers) in &self.tables.supers {
            for &m in self.tables.members_of(ty) {
                let sym = &self.symbols[m as usize];
                let name = &sym.name;
                if sym.kind != SymbolKind::Method {
                    continue;
                }
                for &(sup, c) in supers {
                    for base in self.tables.members_in_hierarchy(sup, name) {
                        if self.symbols[base as usize].params == sym.params {
                            found.push((m, base, 0.9 * c, sym.line));
                        }
                    }
                }
            }
        }
        for (m, base, c, line) in found {
            self.add_edge(m, base, EdgeKind::Overrides, c, line);
        }
    }

    fn resolve_imports(&mut self, fi: usize) {
        let from = self.file_syms[fi];
        for (to, c, line) in self.tables.import_targets(self, fi) {
            self.add_edge(from, to, EdgeKind::Imports, c, line);
        }
    }

    fn resolve_refs(&mut self, fi: usize) {
        let index = self.index;
        let f: &FileIndex = &index.files[fi];
        let mut found = Vec::new();
        for r in &f.refs {
            let kind = match r.kind {
                RefKind::Call | RefKind::New => EdgeKind::Calls,
                RefKind::Type => EdgeKind::Uses,
                RefKind::Extends | RefKind::Implements => continue,
            };
            self.stats.refs += 1;
            let from = self.scope_symbol(fi, r.scope);
            let targets = match self.tables.resolve_ref(self, fi, r) {
                Ok(t) => t,
                Err(super::resolve::Ambiguous) => {
                    self.stats.ambiguous += 1;
                    continue;
                }
            };
            let targets: Vec<_> = targets
                .into_iter()
                .filter(|&(t, _)| kind != EdgeKind::Uses || !self.is_within(from, t))
                .collect();
            if !targets.is_empty() {
                self.stats.resolved += 1;
            }
            for (t, c) in targets {
                found.push((from, t, kind, c, r.line));
            }
        }
        for (from, t, kind, c, line) in found {
            self.add_edge(from, t, kind, c, line);
        }
    }

    /// `s` is `ancestor` or inside it.
    fn is_within(&self, mut s: SymIdx, ancestor: SymIdx) -> bool {
        loop {
            if s == ancestor {
                return true;
            }
            match self.symbols[s as usize].container {
                Some(c) => s = c,
                None => return false,
            }
        }
    }

    /// Test symbol → exercised symbol, for every call from a test file to a
    /// non-test file.
    fn add_test_edges(&mut self) {
        let is_test = |b: &Self, s: SymIdx| b.symbols[s as usize].has_role(Role::Test);
        let tests: Vec<Edge> = self
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && is_test(self, e.from) && !is_test(self, e.to))
            .copied()
            .collect();
        for e in tests {
            self.add_edge(e.from, e.to, EdgeKind::Tests, e.confidence, e.line);
        }
    }
}

/// Test file by convention: under `src/test/`, a `test`/`tests`/`__tests__`
/// /`e2e` directory, named `*Test`, `*Tests`, `*IT`, `*TestCase`, or
/// `*.spec.*` / `*.test.*`.
pub fn is_test_path(path: &str) -> bool {
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    if dirs.split('/').any(|d| {
        matches!(
            d,
            "test"
                | "tests"
                | "__tests__"
                | "e2e"
                | "androidTest"
                | "testFixtures"
                | "integrationTest"
        )
    }) {
        return true;
    }
    if name.contains(".spec.") || name.contains(".test.") {
        return true;
    }
    let stem = name.split('.').next().unwrap_or(name);
    ["Test", "Tests", "IT", "TestCase"]
        .iter()
        .any(|s| stem.len() > s.len() && stem.ends_with(s))
}

/// Java/Kotlin package, else the directory (`.` for the root).
fn module_of(f: &FileIndex) -> String {
    if let Some(p) = &f.package {
        return p.clone();
    }
    match f.path.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => ".".to_string(),
    }
}

/// Key of a cached graph: versions, rules, revisions and indexing options.
pub fn cache_key(
    base_sha: &str,
    head_sha: &str,
    rules: &[Box<dyn FrameworkRule>],
    skip_globs: &[String],
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(
        format!("graph{GRAPH_VERSION}\0index{INDEX_VERSION}\0{base_sha}\0{head_sha}\0").as_bytes(),
    );
    for r in rules {
        h.update(r.name().as_bytes());
        h.update(b"\0");
    }
    for g in skip_globs {
        h.update(g.as_bytes());
        h.update(b"\0");
    }
    h.finalize().to_hex()[..16].to_string()
}

/// `.git/survol/cache/<head_sha>/graph.json`
pub fn cache_path(survol_dir: &Path, head_sha: &str) -> PathBuf {
    survol_dir.join("cache").join(head_sha).join("graph.json")
}

/// The cached graph, if there is one for `key`.
pub fn load_cache(path: &Path, key: &str) -> Option<Graph> {
    let bytes = std::fs::read(path).ok()?;
    let mut g: Graph = serde_json::from_slice(&bytes).ok()?;
    (g.data().key == key).then(|| {
        g.from_cache = true;
        g
    })
}

pub fn save_cache(path: &Path, graph: &Graph) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(graph)?)?;
    std::fs::rename(tmp, path)
}

impl Graph {
    /// Records the revisions and cache key the graph was built for.
    pub fn set_origin(&mut self, key: String, base_sha: String, head_sha: String) {
        self.data.key = key;
        self.data.base_sha = base_sha;
        self.data.head_sha = head_sha;
    }
}
