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

impl ModuleMap {
    /// Per module: changed, or depending on / used by a changed module.
    pub fn impacted(&self) -> Vec<bool> {
        let mut keep: Vec<bool> = self.modules.iter().map(ModuleNode::changed).collect();
        for e in &self.edges {
            let (a, b) = (self.modules[e.from].changed(), self.modules[e.to].changed());
            keep[e.from] |= b;
            keep[e.to] |= a;
        }
        keep
    }

    /// Mermaid flowchart of the modules: changed modules highlighted, edges
    /// labelled with their weight. Modules with neither edge nor change are
    /// left out to keep the chart readable.
    pub fn to_mermaid(&self) -> String {
        self.to_mermaid_of(&vec![true; self.modules.len()])
    }

    /// [`Self::to_mermaid`] limited to the modules `keep` marks.
    pub fn to_mermaid_of(&self, keep: &[bool]) -> String {
        let edges: Vec<&ModuleEdge> = self
            .edges
            .iter()
            .filter(|e| keep[e.from] && keep[e.to])
            .collect();
        let mut used = vec![false; self.modules.len()];
        for e in &edges {
            used[e.from] = true;
            used[e.to] = true;
        }
        let mut out = String::from("flowchart LR\n");
        for (i, m) in self.modules.iter().enumerate() {
            if !keep[i] || (!used[i] && !m.changed()) {
                continue;
            }
            let name = m.name.replace('"', "'");
            if m.changed() {
                out.push_str(&format!(
                    "  m{i}[\"{name}<br/>{} changed symbol(s)\"]:::changed\n",
                    m.changed_symbols
                ));
            } else {
                out.push_str(&format!("  m{i}[\"{name}\"]\n"));
            }
        }
        for e in edges {
            out.push_str(&format!("  m{} -->|{}| m{}\n", e.from, e.count, e.to));
        }
        out.push_str("  classDef changed fill:#fde68a,stroke:#b45309,color:#000\n");
        out
    }
}

#[cfg(test)]
mod mermaid_tests {
    use super::*;

    #[test]
    fn mermaid_keeps_linked_and_changed_modules() {
        let node = |name: &str, changed_files| ModuleNode {
            name: name.into(),
            files: 1,
            changed_files,
            changed_symbols: changed_files,
            test: false,
        };
        let map = ModuleMap {
            modules: vec![node("a", 1), node("b", 0), node("lonely", 0)],
            edges: vec![ModuleEdge {
                from: 0,
                to: 1,
                count: 3,
                kinds: BTreeMap::new(),
            }],
        };
        let m = map.to_mermaid();
        assert!(m.starts_with("flowchart LR\n"));
        assert!(m.contains("m0[\"a<br/>1 changed symbol(s)\"]:::changed"));
        assert!(m.contains("m1[\"b\"]"));
        assert!(m.contains("m0 -->|3| m1"));
        assert!(!m.contains("lonely"));
    }

    #[test]
    fn impacted_keeps_changed_modules_and_their_neighbours() {
        let node = |name: &str, changed_files| ModuleNode {
            name: name.into(),
            files: 1,
            changed_files,
            changed_symbols: changed_files,
            test: false,
        };
        let edge = |from, to| ModuleEdge {
            from,
            to,
            count: 1,
            kinds: BTreeMap::new(),
        };
        // user -> a (changed) -> b -> far; other stands alone.
        let map = ModuleMap {
            modules: vec![
                node("a", 1),
                node("b", 0),
                node("far", 0),
                node("other", 0),
                node("user", 0),
            ],
            edges: vec![edge(4, 0), edge(0, 1), edge(1, 2)],
        };
        let keep = map.impacted();
        assert_eq!(keep, vec![true, true, false, false, true]);
        let m = map.to_mermaid_of(&keep);
        assert!(m.contains("m4 -->|1| m0") && m.contains("m0 -->|1| m1"));
        assert!(!m.contains("far") && !m.contains("m1 -->|1| m2"));
    }
}
