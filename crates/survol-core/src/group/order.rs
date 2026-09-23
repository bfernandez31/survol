//! Reading order of groups.
//!
//! Placeholder heuristic until the graph (step 3) provides a topological
//! order: foundations first, then services, then entry points, tests last,
//! and the mechanical group at the very end.

use super::Group;

/// Sorts `groups` for reading and sets their `order`. Stable: groups of the
/// same rank keep the order they came in (the LLM's suggestion).
pub fn order_groups(groups: &mut [Group]) {
    groups.sort_by_key(|g| (g.mechanical, rank(g)));
    for (i, g) in groups.iter_mut().enumerate() {
        g.order = i;
    }
}

/// Rank of the group's dominant layer (the one with the most hunks; the
/// lowest rank wins ties).
fn rank(group: &Group) -> u8 {
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
}
