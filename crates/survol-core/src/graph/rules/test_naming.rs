//! Tests by naming convention: `FooTest`, `FooTests`, `FooIT`, `FooTestCase`
//! test `Foo`; `foo.spec.ts` / `foo.test.ts` test the types of `foo.ts`.

use super::FrameworkRule;
use crate::graph::{EdgeKind, GraphBuilder, Role};
use crate::index::Index;

/// Confidence of a test edge from naming alone.
const NAMING: f32 = 0.8;

pub struct TestNaming;

impl FrameworkRule for TestNaming {
    fn name(&self) -> &str {
        "test-naming"
    }

    fn apply(&self, index: &Index, graph: &mut GraphBuilder) {
        let mut edges = Vec::new();
        for (fi, file) in index.files.iter().enumerate() {
            if !graph.symbol(graph.file_symbol(fi)).has_role(Role::Test) {
                continue;
            }
            // `FooTest` → `Foo`.
            for (d, def) in file.defs.iter().enumerate() {
                if !def.kind.is_type() || def.parent.is_some() {
                    continue;
                }
                let Some(tested) = ["Tests", "Test", "IT", "TestCase"]
                    .iter()
                    .find_map(|s| def.name.strip_suffix(s))
                    .filter(|t| !t.is_empty())
                else {
                    continue;
                };
                let from = graph.def_symbol(fi, d);
                for (t, c) in graph.resolve_type(fi, tested) {
                    if !graph.symbol(t).has_role(Role::Test) {
                        edges.push((from, t, NAMING * c, def.line));
                    }
                }
            }
            // `foo.spec.ts` → types of `foo.ts`.
            let Some((stem, ext)) = file
                .path
                .split_once(".spec.")
                .or_else(|| file.path.split_once(".test."))
            else {
                continue;
            };
            let Some(target) = graph.file_index(&format!("{stem}.{ext}")) else {
                continue;
            };
            let from = graph.file_symbol(fi);
            for (d, def) in index.files[target].defs.iter().enumerate() {
                if def.parent.is_none() && (def.kind.is_type() || def.kind.is_callable()) {
                    edges.push((from, graph.def_symbol(target, d), NAMING, 1));
                }
            }
        }
        for (from, to, c, line) in edges {
            graph.add_edge(from, to, EdgeKind::Tests, c, line);
        }
    }
}
