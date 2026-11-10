//! The graph folded to one level of a drill-down map: the modules of the
//! review, then the files of a module, then the symbols of a file.
//!
//! What lies outside the scope is kept as coarser nodes on the border: the
//! other modules around the files of a module; the other files of the module
//! and the other modules around the symbols of a file. Only the nodes the
//! review touches and their neighbours are kept, the heaviest first, up to a
//! maximum: a level is always small enough to be drawn.

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
    /// Inside nodes first, changed ones first among them.
    pub nodes: Vec<LevelNode>,
    /// Heaviest first.
    pub edges: Vec<LevelEdge>,
    /// Nodes left out: not around a change (`all` off), or over the maximum.
    pub hidden: usize,
}

/// The level of `scope`. `all`: every node, not only the changed ones and
/// their neighbours. At most `max` nodes are kept.
pub fn level(g: &Graph, scope: &Scope, all: bool, max: usize) -> Level {
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

    // Around the changes: the changed inside nodes and their neighbours.
    let changed = |n: &LevelNode| n.changed() && !n.outside;
    let mut keep: Vec<bool> = nodes.iter().map(changed).collect();
    if all || !keep.contains(&true) {
        keep = vec![true; nodes.len()];
    } else {
        for e in edges.values() {
            let (a, b) = (changed(&nodes[e.from]), changed(&nodes[e.to]));
            keep[e.from] |= b;
            keep[e.to] |= a;
        }
    }
    // The heaviest first, within the maximum.
    let mut weight = vec![0usize; nodes.len()];
    for e in edges.values().filter(|e| keep[e.from] && keep[e.to]) {
        weight[e.from] += e.count;
        weight[e.to] += e.count;
    }
    let mut order: Vec<usize> = (0..nodes.len()).filter(|&i| keep[i]).collect();
    order.sort_by_key(|&i| {
        let n = &nodes[i];
        (!changed(n), n.outside, std::cmp::Reverse(weight[i]), i)
    });
    order.truncate(max);
    let hidden = nodes.len() - order.len();
    let mut new_id = vec![usize::MAX; nodes.len()];
    for (new, &old) in order.iter().enumerate() {
        new_id[old] = new;
    }
    let mut slots: Vec<Option<LevelNode>> = nodes.into_iter().map(Some).collect();
    let nodes: Vec<LevelNode> = order.iter().filter_map(|&i| slots[i].take()).collect();
    let mut edges: Vec<LevelEdge> = edges
        .into_values()
        .filter(|e| new_id[e.from] != usize::MAX && new_id[e.to] != usize::MAX)
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
    Level {
        nodes,
        edges,
        hidden,
    }
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
            let changed = symbols.iter().filter(|&&s| g.symbol(s).changed).count();
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
