//! Reading order of groups.
//!
//! Without a graph, [`order_groups`] ranks groups by their dominant layer:
//! foundations first, then services, then entry points, tests last, and the
//! mechanical group at the very end. With a graph, [`order_groups_with_graph`]
//! sorts them topologically (a group whose symbols others use comes first),
//! the layer rank breaking ties and ordering groups within a cycle.

use std::collections::{BTreeSet, HashMap};

use super::Group;
use crate::graph::{Graph, SymIdx};

/// Sorts `groups` for reading and sets their `order`. Stable: groups of the
/// same rank keep the order they came in (the LLM's suggestion).
pub fn order_groups(groups: &mut [Group]) {
    groups.sort_by_key(|g| (g.mechanical, rank(g)));
    for (i, g) in groups.iter_mut().enumerate() {
        g.order = i;
    }
}

/// Edges below this confidence do not order groups.
const MIN_CONFIDENCE: f32 = 0.3;

/// Sorts `groups` for reading along the dependencies between their symbols,
/// and sets their `order`.
///
/// Group A depends on group B when a symbol changed by A has a dependency
/// edge (call, type use, inheritance, injection, import...) to a symbol
/// changed by B: B comes first. Cycles are condensed (Tarjan) and read as
/// one block. Among groups free to come next, and inside a cycle, the
/// layer rank of [`order_groups`] decides, then the incoming order. The
/// mechanical group stays last.
pub fn order_groups_with_graph(groups: &mut Vec<Group>, graph: &Graph) {
    order_groups(groups);
    let n = groups.len();
    let mut owner: HashMap<SymIdx, Vec<usize>> = HashMap::new();
    for (g, group) in groups.iter().enumerate().filter(|(_, g)| !g.mechanical) {
        for &h in &group.hunk_ids {
            for &s in graph.symbols_of_hunk(h) {
                let v = owner.entry(s).or_default();
                if !v.contains(&g) {
                    v.push(g);
                }
            }
        }
    }
    // deps[a] = groups that must come before a.
    let mut deps: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (&s, users) in &owner {
        for e in graph.edges_from(s) {
            if !e.kind.is_dependency() || e.confidence < MIN_CONFIDENCE {
                continue;
            }
            for &b in owner.get(&e.to).into_iter().flatten() {
                for &a in users {
                    if a != b {
                        deps[a].insert(b);
                    }
                }
            }
        }
    }
    let rank: Vec<(bool, u8, usize)> = groups
        .iter()
        .enumerate()
        .map(|(i, g)| (g.mechanical, super::order::rank(g), i))
        .collect();
    let order = topo_order(&deps, &rank);
    let mut slots: Vec<Option<Group>> = groups.drain(..).map(Some).collect();
    groups.extend(
        order
            .into_iter()
            .map(|i| slots[i].take().expect("each group once")),
    );
    for (i, g) in groups.iter_mut().enumerate() {
        g.order = i;
    }
}

/// Topological order of `deps` (node → nodes that come before it), cycles
/// condensed, choosing the smallest `key` among the ready components.
fn topo_order<K: Ord + Copy>(deps: &[BTreeSet<usize>], key: &[K]) -> Vec<usize> {
    let comps = tarjan(deps);
    let n_comps = comps.iter().max().map_or(0, |m| m + 1);
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); n_comps];
    for (v, &c) in comps.iter().enumerate() {
        members[c].push(v);
    }
    for m in &mut members {
        m.sort_by_key(|&v| key[v]);
    }
    let mut before: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n_comps];
    let mut after: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n_comps];
    for (v, ds) in deps.iter().enumerate() {
        for &d in ds {
            let (cv, cd) = (comps[v], comps[d]);
            if cv != cd {
                before[cv].insert(cd);
                after[cd].insert(cv);
            }
        }
    }
    let mut pending: Vec<usize> = before.iter().map(BTreeSet::len).collect();
    let mut ready: BTreeSet<(K, usize)> = (0..n_comps)
        .filter(|&c| pending[c] == 0)
        .map(|c| (key[members[c][0]], c))
        .collect();
    let mut out = Vec::with_capacity(deps.len());
    while let Some(first) = ready.pop_first() {
        let c = first.1;
        out.extend(&members[c]);
        for &next in &after[c] {
            pending[next] -= 1;
            if pending[next] == 0 {
                ready.insert((key[members[next][0]], next));
            }
        }
    }
    out
}

/// Strongly connected component of each node (iterative Tarjan).
fn tarjan(deps: &[BTreeSet<usize>]) -> Vec<usize> {
    let n = deps.len();
    let adj: Vec<Vec<usize>> = deps.iter().map(|d| d.iter().copied().collect()).collect();
    let (mut index, mut low) = (vec![usize::MAX; n], vec![0; n]);
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut comp = vec![usize::MAX; n];
    let (mut next_index, mut next_comp) = (0, 0);
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        // (node, next edge to visit)
        let mut work = vec![(root, 0)];
        while let Some(&mut (v, ref mut i)) = work.last_mut() {
            if *i == 0 && index[v] == usize::MAX {
                index[v] = next_index;
                low[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = adj[v].get(*i) {
                *i += 1;
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    comp[w] = next_comp;
                    if w == v {
                        break;
                    }
                }
                next_comp += 1;
            }
        }
    }
    comp
}

/// Rank of the group's dominant layer (the one with the most hunks; the
/// lowest rank wins ties).
pub(super) fn rank(group: &Group) -> u8 {
    group
        .layers
        .iter()
        .map(|l| (std::cmp::Reverse(l.hunk_ids.len()), layer_rank(&l.name)))
        .min()
        .map_or(1, |(_, r)| r)
}

/// 0: models, persistence, config, build; 1: services and other code;
/// 2: api, ui and other entry points; 3: tests and docs.
/// Matched on word prefixes, so "ui" does not match "build".
pub fn layer_rank(name: &str) -> u8 {
    let n = name.to_ascii_lowercase();
    let has = |keys: &[&str]| {
        n.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| keys.iter().any(|k| word.starts_with(k)))
    };
    if has(&[
        "test",
        "spec",
        "e2e",
        "fixture",
        "docs",
        "documentation",
        "example",
        "sample",
        "bench",
    ]) {
        3
    } else if has(&[
        "api",
        "controller",
        "endpoint",
        "rest",
        "route",
        "web",
        "ui",
        "front",
        "view",
        "component",
        "page",
        "cli",
        "entry",
        "handler",
        "job",
        "consumer",
        "listener",
    ]) {
        2
    } else if has(&[
        "model",
        "entit",
        "domain",
        "schema",
        "persist",
        "database",
        "db",
        "migration",
        "repositor",
        "dao",
        "config",
        "contract",
        "type",
        "dto",
        "build",
        "depend",
    ]) {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::super::Layer;
    use super::*;

    fn group(title: &str, layers: &[(&str, usize)], mechanical: bool) -> Group {
        Group {
            id: 0,
            title: title.into(),
            summary: String::new(),
            layers: layers
                .iter()
                .map(|(n, c)| Layer {
                    name: n.to_string(),
                    hunk_ids: (0..*c).collect(),
                })
                .collect(),
            order: 0,
            hunk_ids: vec![],
            file_ids: vec![],
            mechanical,
        }
    }

    #[test]
    fn orders_foundations_first_and_mechanical_last() {
        let mut g = vec![
            group("noise", &[("generated", 9)], true),
            group("tests", &[("tests", 5), ("service", 1)], false),
            group("endpoint", &[("api", 3), ("service", 2)], false),
            group("service", &[("service", 3), ("model", 1)], false),
            group(
                "entities",
                &[("persistence", 2), ("model", 2), ("api", 1)],
                false,
            ),
            group("other", &[("code", 1)], false),
        ];
        order_groups(&mut g);
        let titles: Vec<_> = g.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles,
            ["entities", "service", "other", "endpoint", "tests", "noise"]
        );
        assert_eq!(g[5].order, 5);
        assert_eq!(layer_rank("build"), 0);
        assert_eq!(layer_rank("unit tests"), 3);
        assert_eq!(layer_rank("front-end"), 2);
    }

    fn graph(n_symbols: usize, edges: &[(u32, u32)], hunk_symbols: Vec<Vec<u32>>) -> Graph {
        use crate::graph::{Edge, EdgeKind, GraphData, Lang, Span, Symbol, SymbolKind};
        let symbols = (0..n_symbols)
            .map(|i| Symbol {
                id: format!("f#s{i}"),
                name: format!("s{i}"),
                kind: SymbolKind::Method,
                file: "f".into(),
                lang: Lang::Java,
                span: Span { start: 1, end: 1 },
                line: 1,
                container: None,
                changed: true,
                removed: false,
                annotations: vec![],
                params: None,
                variadic: false,
                roles: vec![],
                tags: Default::default(),
            })
            .collect();
        let edges = edges
            .iter()
            .map(|&(from, to)| Edge {
                from,
                to,
                kind: EdgeKind::Calls,
                confidence: 0.9,
                line: 1,
                lsp: false,
            })
            .collect();
        Graph::new(GraphData {
            symbols,
            edges,
            hunk_symbols,
            ..Default::default()
        })
    }

    fn with_hunks(mut g: Group, hunks: &[usize]) -> Group {
        g.hunk_ids = hunks.to_vec();
        g
    }

    #[test]
    fn orders_by_graph_with_cycles() {
        let mut groups = vec![
            with_hunks(group("noise", &[("generated", 1)], true), &[5]),
            with_hunks(group("tests", &[("tests", 1)], false), &[4]),
            // Entities by layer, but they call the endpoint: after it.
            with_hunks(group("entities", &[("model", 1)], false), &[0]),
            with_hunks(group("endpoint", &[("api", 1)], false), &[1]),
            // Service and helpers call each other: one block, service first
            // (lower layer rank... equal here, so incoming order).
            with_hunks(group("helpers", &[("service", 1)], false), &[2]),
            with_hunks(group("service", &[("service", 1)], false), &[3]),
        ];
        // symbols: 0 entities, 1 endpoint, 2 helpers, 3 service, 4 tests, 5 noise
        let g = graph(
            6,
            &[(0, 1), (1, 3), (3, 2), (2, 3), (4, 0), (4, 3), (5, 0)],
            vec![vec![0], vec![1], vec![2], vec![3], vec![4], vec![5]],
        );
        order_groups_with_graph(&mut groups, &g);
        let titles: Vec<_> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "helpers", "service", "endpoint", "entities", "tests", "noise"
            ]
        );
        assert!(groups.iter().enumerate().all(|(i, g)| g.order == i));

        // Without edges: the layer order.
        let mut groups2 = groups.clone();
        order_groups_with_graph(
            &mut groups2,
            &graph(
                6,
                &[],
                vec![vec![0], vec![1], vec![2], vec![3], vec![4], vec![5]],
            ),
        );
        let titles: Vec<_> = groups2.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "entities", "helpers", "service", "endpoint", "tests", "noise"
            ]
        );
    }

    #[test]
    fn tarjan_finds_components() {
        let deps: Vec<BTreeSet<usize>> = vec![
            BTreeSet::from([1]),
            BTreeSet::from([2]),
            BTreeSet::from([0]),
            BTreeSet::from([2]),
        ];
        let c = tarjan(&deps);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_ne!(c[3], c[0]);
        assert_eq!(topo_order(&deps, &[3, 2, 1, 0]), [2, 1, 0, 3]);
    }
}
