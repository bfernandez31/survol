//! The graph folded to one level of a drill-down map: the modules of the
//! review, then the files of a module, then the symbols of a file.
//!
//! What lies outside the scope is kept as coarser nodes on the border: the
//! other modules around the files of a module; the other files of the module
//! and the other modules around the symbols of a file. Only the outside nodes
//! linked to an inside one are kept.

use std::collections::{BTreeMap, HashMap};

use super::{EdgeKind, Graph, SymIdx, SymbolKind};

/// What a level shows the inside of.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The modules.
    Root,
    /// The files of a module.
    Module(String),
    /// The symbols of a file (path).
    File(String),
}

/// What a node stands for: also the scope entering it leads to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeRef {
    Module(String),
    File(String),
    Symbol(SymIdx),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelNode {
    pub of: NodeRef,
    pub label: String,
    /// Outside the scope: a neighbour kept on the border.
    pub outside: bool,
    /// Changed symbols inside (a changed file without any counts as one).
    pub changes: usize,
    /// A symbol that only exists in the base revision.
    pub removed: bool,
    /// Files of a module, symbols of a file.
    pub size: usize,
}

impl LevelNode {
    pub fn changed(&self) -> bool {
        self.changes > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelEdge {
    /// Indexes into [`Level::nodes`]: `from` depends on `to`.
    pub from: usize,
    pub to: usize,
    /// Symbol-level edges folded into this one.
    pub count: usize,
    pub kinds: BTreeMap<EdgeKind, usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Level {
    /// Changed inside nodes first (the most changed, members before types,
    /// then the most linked), then the other inside nodes, then the outside
    /// ones, the most linked first.
    pub nodes: Vec<LevelNode>,
    /// Heaviest first.
    pub edges: Vec<LevelEdge>,
}

/// The level of `scope`.
pub fn level(g: &Graph, scope: &Scope) -> Level {
    // A file scope shows its module's other files apart.
    let home = match scope {
        Scope::File(path) => g.file(path).map(|f| f.module.as_str()),
        _ => None,
    };
    let key_of = |s: SymIdx| -> Option<(NodeRef, bool)> {
        let sym = g.symbol(s);
        let module = g.file(&sym.file)?.module.as_str();
        Some(match scope {
            Scope::Root => (NodeRef::Module(module.to_string()), false),
            Scope::Module(m) if m == module => (NodeRef::File(sym.file.clone()), false),
            Scope::Module(_) => (NodeRef::Module(module.to_string()), true),
            Scope::File(path) if *path == sym.file => (NodeRef::Symbol(s), false),
            Scope::File(_) if Some(module) == home => (NodeRef::File(sym.file.clone()), true),
            Scope::File(_) => (NodeRef::Module(module.to_string()), true),
        })
    };

    // Per module: files, changed files, changed symbols.
    let mut modules: HashMap<&str, [usize; 3]> = HashMap::new();
    for f in g.files() {
        let m = modules.entry(f.module.as_str()).or_default();
        m[0] += 1;
        m[1] += usize::from(f.changed);
    }
    for s in g.changed_symbols() {
        if let Some(m) = g.module_of(s).and_then(|m| modules.get_mut(m)) {
            m[2] += 1;
        }
    }

    let mut ids: HashMap<NodeRef, usize> = HashMap::new();
    let mut nodes: Vec<LevelNode> = Vec::new();
    let mut node = |of: NodeRef, outside: bool, nodes: &mut Vec<LevelNode>| -> usize {
        *ids.entry(of.clone()).or_insert_with(|| {
            nodes.push(describe(g, &modules, of, outside));
            nodes.len() - 1
        })
    };
    // Everything inside, connected or not.
    match scope {
        Scope::Root => {
            for f in g.files() {
                node(NodeRef::Module(f.module.clone()), false, &mut nodes);
            }
        }
        Scope::Module(m) => {
            for f in g.files().iter().filter(|f| f.module == *m) {
                node(NodeRef::File(f.path.clone()), false, &mut nodes);
            }
        }
        Scope::File(path) => {
            let symbols = (0..g.symbols().len() as SymIdx).filter(|&s| {
                let sym = g.symbol(s);
                sym.file == *path && sym.kind != SymbolKind::File
            });
            for s in symbols {
                node(NodeRef::Symbol(s), false, &mut nodes);
            }
        }
    }
    let mut edges: BTreeMap<(usize, usize), LevelEdge> = BTreeMap::new();
    for e in g.edges().iter().filter(|e| e.kind.is_dependency()) {
        // Inside a file, its imports say nothing the other edges do not.
        if matches!(scope, Scope::File(_)) && g.symbol(e.from).kind == SymbolKind::File {
            continue;
        }
        let (Some((a, a_out)), Some((b, b_out))) = (key_of(e.from), key_of(e.to)) else {
            continue;
        };
        if a == b || (a_out && b_out) {
            continue;
        }
        let (a, b) = (node(a, a_out, &mut nodes), node(b, b_out, &mut nodes));
        let edge = edges.entry((a, b)).or_insert_with(|| LevelEdge {
            from: a,
            to: b,
            count: 0,
            kinds: BTreeMap::new(),
        });
        edge.count += 1;
        *edge.kinds.entry(e.kind).or_default() += 1;
    }

    // Changed inside nodes first, the most changed first, then the heaviest.
    // A type is changed when one of its members is: the members first.
    let changed = |n: &LevelNode| n.changed() && !n.outside;
    let mut weight = vec![0usize; nodes.len()];
    for e in edges.values() {
        weight[e.from] += e.count;
        weight[e.to] += e.count;
    }
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by_key(|&i| {
        let n = &nodes[i];
        let changes = if changed(n) { n.changes } else { 0 };
        let is_type = matches!(n.of, NodeRef::Symbol(s) if g.symbol(s).kind.is_type());
        (
            !changed(n),
            n.outside,
            std::cmp::Reverse(changes),
            is_type,
            std::cmp::Reverse(weight[i]),
            i,
        )
    });
    let mut new_id = vec![0; nodes.len()];
    for (new, &old) in order.iter().enumerate() {
        new_id[old] = new;
    }
    let mut slots: Vec<Option<LevelNode>> = nodes.into_iter().map(Some).collect();
    let nodes: Vec<LevelNode> = order.iter().filter_map(|&i| slots[i].take()).collect();
    let mut edges: Vec<LevelEdge> = edges
        .into_values()
        .map(|e| LevelEdge {
            from: new_id[e.from],
            to: new_id[e.to],
            ..e
        })
        .collect();
    edges.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then((a.from, a.to).cmp(&(b.from, b.to)))
    });
    Level { nodes, edges }
}

fn describe(
    g: &Graph,
    modules: &HashMap<&str, [usize; 3]>,
    of: NodeRef,
    outside: bool,
) -> LevelNode {
    let (label, changes, removed, size) = match &of {
        NodeRef::Module(m) => {
            let [files, changed_files, symbols] =
                modules.get(m.as_str()).copied().unwrap_or_default();
            (m.clone(), symbols.max(changed_files), false, files)
        }
        NodeRef::File(path) => {
            let symbols = g.symbols_in_file(path);
            let changed = symbols
                .iter()
                .filter(|&&s| g.symbol(s).changed && g.symbol(s).kind != SymbolKind::File)
                .count();
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            (
                name,
                changed.max(usize::from(g.is_file_changed(path))),
                false,
                symbols.len().saturating_sub(1),
            )
        }
        NodeRef::Symbol(s) => {
            let sym = g.symbol(*s);
            (
                g.display_name(*s),
                usize::from(sym.changed || sym.removed),
                sym.removed,
                0,
            )
        }
    };
    LevelNode {
        of,
        label,
        outside,
        changes,
        removed,
        size,
    }
}

/// The scope containing `scope`, `None` at the root.
pub fn parent(g: &Graph, scope: &Scope) -> Option<Scope> {
    match scope {
        Scope::Root => None,
        Scope::Module(_) => Some(Scope::Root),
        Scope::File(path) => Some(
            g.file(path)
                .map_or(Scope::Root, |f| Scope::Module(f.module.clone())),
        ),
    }
}

/// The scope showing `of` as one of its inside nodes.
pub fn home(g: &Graph, of: &NodeRef) -> Scope {
    match of {
        NodeRef::Module(_) => Scope::Root,
        NodeRef::File(path) => g
            .file(path)
            .map_or(Scope::Root, |f| Scope::Module(f.module.clone())),
        NodeRef::Symbol(s) => Scope::File(g.symbol(*s).file.clone()),
    }
}

/// Whether symbol `s` lives in `of`.
fn contains(g: &Graph, of: &NodeRef, s: SymIdx) -> bool {
    match of {
        NodeRef::Module(m) => g.module_of(s) == Some(m.as_str()),
        NodeRef::File(path) => g.symbol(s).file == *path,
        NodeRef::Symbol(x) => *x == s,
    }
}

/// Following the links from `from` into `to` (coarser, outside the level of
/// `from`): the node of `to` as fine as `from` the most linked with it, the
/// file for a file, the symbol for a symbol. `None` for a module, or when no
/// such node is linked.
pub fn follow(g: &Graph, from: &NodeRef, to: &NodeRef) -> Option<NodeRef> {
    let mut counts: BTreeMap<NodeRef, usize> = BTreeMap::new();
    for e in g.edges().iter().filter(|e| e.kind.is_dependency()) {
        let there = if contains(g, from, e.from) && contains(g, to, e.to) {
            e.to
        } else if contains(g, from, e.to) && contains(g, to, e.from) {
            e.from
        } else {
            continue;
        };
        let node = match from {
            NodeRef::Module(_) => return None,
            NodeRef::File(_) => NodeRef::File(g.symbol(there).file.clone()),
            // A file's own symbol stands for its imports: not a node.
            NodeRef::Symbol(_) if g.symbol(there).kind == SymbolKind::File => continue,
            NodeRef::Symbol(_) => NodeRef::Symbol(there),
        };
        *counts.entry(node).or_default() += 1;
    }
    // The most linked, the first in order on a tie.
    let best = counts.values().copied().max()?;
    counts
        .into_iter()
        .find(|&(_, n)| n == best)
        .map(|(node, _)| node)
}
