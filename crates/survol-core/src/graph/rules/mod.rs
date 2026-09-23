//! Framework rules: knowledge that generic resolution cannot infer
//! (annotations, dependency injection, routes, HTTP bindings...).
//!
//! A rule runs once, after generic resolution, with the whole [`Index`]
//! (annotations and their arguments, bindings, refs, imports) and a
//! [`GraphBuilder`] to add edges ([`GraphBuilder::add_edge`]) and mark
//! symbols ([`GraphBuilder::add_role`], [`GraphBuilder::symbol_mut`] for
//! tags). Helpers such as [`GraphBuilder::resolve_type`],
//! [`GraphBuilder::members`] and [`GraphBuilder::subtypes`] give access to
//! the generic resolution.
//!
//! To add a rule: implement [`FrameworkRule`] in a module of this directory,
//! register it in [`default_rules`], and bump [`super::GRAPH_VERSION`] when
//! it changes (cached graphs are keyed by it and by the rule names).
//!
//! Walking annotated definitions:
//!
//! ```ignore
//! for (fi, file) in index.files.iter().enumerate() {
//!     for (d, def) in file.defs.iter().enumerate() {
//!         if def.annotations.iter().any(|a| a.name == "Service") {
//!             let s = graph.def_symbol(fi, d);
//!             graph.add_role(s, Role::Component);
//!         }
//!     }
//! }
//! ```

mod test_naming;

use super::GraphBuilder;
use crate::index::Index;

pub use test_naming::TestNaming;

pub trait FrameworkRule: Send + Sync {
    /// Short stable name (`spring`, `angular`...): shown in stats and part of
    /// the graph cache key.
    fn name(&self) -> &str;

    /// Adds edges, roles and tags to `graph`.
    fn apply(&self, index: &Index, graph: &mut GraphBuilder);
}

/// Rules applied by [`crate::review::build_graph`], in order.
pub fn default_rules() -> Vec<Box<dyn FrameworkRule>> {
    vec![Box::new(TestNaming)]
}
