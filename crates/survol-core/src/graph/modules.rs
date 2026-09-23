//! Module map: the graph folded to packages (Java/Kotlin) or directories.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::{EdgeKind, Graph};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleMap {
    /// Sorted by name.
    pub modules: Vec<ModuleNode>,
    /// Dependencies between modules, heaviest first.
    pub edges: Vec<ModuleEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleNode {
    /// Package or directory.
    pub name: String,
    pub files: usize,
    /// Files of the module that are part of the diff.
    pub changed_files: usize,
    /// Symbols changed by the diff.
    pub changed_symbols: usize,
    /// Only test files.
    pub test: bool,
}

impl ModuleNode {
    pub fn changed(&self) -> bool {
        self.changed_files > 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleEdge {
    /// Indexes into [`ModuleMap::modules`]: `from` depends on `to`.
    pub from: usize,
    pub to: usize,
    /// Symbol-level edges folded into this one.
    pub count: usize,
    pub kinds: BTreeMap<EdgeKind, usize>,
}

pub(super) fn module_map(g: &Graph) -> ModuleMap {
    let mut names: Vec<&str> = g.files().iter().map(|f| f.module.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let pos: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let mut modules: Vec<ModuleNode> = names
        .iter()
        .map(|n| ModuleNode {
            name: n.to_string(),
            files: 0,
            changed_files: 0,
            changed_symbols: 0,
            test: true,
        })
        .collect();
    for f in g.files() {
        let m = &mut modules[pos[f.module.as_str()]];
        m.files += 1;
        m.changed_files += usize::from(f.changed);
        m.test &= f.test;
    }
    for s in g.changed_symbols() {
        if let Some(m) = g.module_of(s) {
            modules[pos[m]].changed_symbols += 1;
        }
    }
    let mut edges: BTreeMap<(usize, usize), ModuleEdge> = BTreeMap::new();
    for e in g.edges().iter().filter(|e| e.kind.is_dependency()) {
        let (Some(a), Some(b)) = (g.module_of(e.from), g.module_of(e.to)) else {
            continue;
        };
        let (a, b) = (pos[a], pos[b]);
        if a == b {
            continue;
        }
        let me = edges.entry((a, b)).or_insert_with(|| ModuleEdge {
            from: a,
            to: b,
            count: 0,
            kinds: BTreeMap::new(),
        });
        me.count += 1;
        *me.kinds.entry(e.kind).or_default() += 1;
    }
    let mut edges: Vec<ModuleEdge> = edges.into_values().collect();
    edges.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then((a.from, a.to).cmp(&(b.from, b.to)))
    });
    ModuleMap { modules, edges }
}
