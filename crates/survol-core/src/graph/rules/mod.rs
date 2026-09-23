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
//! Rules: [`TestNaming`], [`Spring`], [`Angular`], [`HttpLink`] (in this order).
//!
//! Conventions shared by the rules (and shown by the views):
//! - roles: see [`super::Role`] (`External` for `@FeignClient` and HTTP client users);
//! - edges: `injects` (consumer → implementation or `@Bean` method),
//!   `http_calls` (HTTP call → endpoint), `configures` (config file, OpenAPI
//!   spec or `environment` → consumer; line of the key), `routes` (route
//!   table or template → component), `publishes` (publisher → listener);
//! - tags (several values joined by `, `): `entry` (`http`, `kafka`,
//!   `rabbit`, `jms`, `sqs`, `scheduled`, `event`, `runner`, `main`,
//!   `route`), `http.route` (`GET /api/owners/{id}`), `http.context_path`,
//!   `http.spec`, `http.calls`, `http.client`, `http.base_url`, `external`,
//!   `kafka.topics`, `rabbit.queues`, `jms.destination`, `schedule`,
//!   `event.type`, `event.publishes`, `spring.stereotype`, `spring.bean`,
//!   `bean.type`, `spring.profile`, `config.keys`, `config.prefix`,
//!   `persistence`, `persistence.entity`, `persistence.table`,
//!   `angular.selector`, `angular.template`, `angular.route`,
//!   `angular.provided_in`, `angular.pipe`, `angular.environment`.
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

mod angular;
mod http_link;
mod spring;
mod test_naming;
#[cfg(test)]
mod tests;
pub mod util;

use super::GraphBuilder;
use crate::index::Index;

pub use angular::Angular;
pub use http_link::HttpLink;
pub use spring::Spring;
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
    vec![
        Box::new(TestNaming),
        Box::new(Spring),
        Box::new(Angular),
        Box::new(HttpLink),
    ]
}
