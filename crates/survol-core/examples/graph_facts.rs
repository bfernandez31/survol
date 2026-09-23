//! Prints what the framework rules found in a repository: roles, entry
//! points, and framework edges (`injects`, `http_calls`, `configures`,
//! `routes`, `publishes`), with timings. Read-only: no cache is written.
//!
//! ```sh
//! cargo run --release -p survol-core --example graph_facts -- <repo> [rev] [--all] [--no-rules] [--with=<repo2>]
//! ```
//!
//! `--with=<repo2>` merges the HEAD index of a second repository under its
//! directory name ([`Index::merge`]): a front end and its back end kept in
//! separate repositories are then linked like a monorepo.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use survol_core::git::Git;
use survol_core::graph::{self, EdgeKind, Graph, Role};
use survol_core::index::{Index, Options};
use survol_core::model::Diff;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let all = args.iter().any(|a| a == "--all");
    let no_rules = args.iter().any(|a| a == "--no-rules");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let dir = pos
        .first()
        .expect("usage: graph_facts <repo> [rev] [--all] [--no-rules]");
    let rev = pos.get(1).map_or("HEAD", |s| s.as_str());
    let git = Git::new(Path::new(dir.as_str()));
    let head = git.rev_parse(rev).expect("rev");
    let opts = Options::new(&[], None).expect("options");
    let t = Instant::now();
    let mut index = Index::build(&git, &head, None, &opts, &mut |_| {}).expect("index");
    for other in args.iter().filter_map(|a| a.strip_prefix("--with=")) {
        let g2 = Git::new(Path::new(other));
        let h2 = g2.rev_parse("HEAD").expect("rev");
        let i2 = Index::build(&g2, &h2, None, &opts, &mut |_| {}).expect("index");
        let name = other
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(other);
        index.merge(i2, name);
    }
    let index_ms = t.elapsed().as_millis();
    if std::env::var("SIZE").is_ok() {
        let n: usize = index
            .files
            .iter()
            .map(|f| serde_json::to_vec(f).unwrap().len())
            .sum();
        println!("index json bytes: {n}");
    }
    let rules = if no_rules {
        vec![]
    } else {
        graph::default_rules()
    };
    // Time generic resolution alone, then with the rules.
    let t = Instant::now();
    let base = graph::build(&index, &Diff::default(), &[]);
    let generic_ms = t.elapsed().as_millis();
    let t = Instant::now();
    let g = graph::build(&index, &Diff::default(), &rules);
    let full_ms = t.elapsed().as_millis();
    println!(
        "files {} | symbols {} | edges {} (generic {}) | index {index_ms} ms | graph generic {generic_ms} ms, with rules {full_ms} ms",
        g.files().len(),
        g.symbols().len(),
        g.edges().len(),
        base.edges().len(),
    );
    let mut roles: BTreeMap<String, usize> = BTreeMap::new();
    for s in g.symbols() {
        for r in &s.roles {
            *roles.entry(format!("{r:?}")).or_default() += 1;
        }
    }
    println!("roles: {roles:?}");
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for e in g.edges() {
        *kinds.entry(format!("{:?}", e.kind)).or_default() += 1;
    }
    println!("edges: {kinds:?}");
    let limit = if all { usize::MAX } else { 15 };

    println!("\n== entry points");
    let entries: Vec<_> = (0..g.symbols().len() as u32)
        .filter(|&s| g.symbol(s).has_role(Role::EntryPoint))
        .collect();
    for &s in entries.iter().take(limit) {
        let sym = g.symbol(s);
        let tags: Vec<String> = sym.tags.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!(
            "  {} [{}] {}",
            g.display_name(s),
            sym.file,
            tags.join(" | ")
        );
    }
    println!("  ({} total)", entries.len());
    for kind in [
        EdgeKind::Injects,
        EdgeKind::HttpCalls,
        EdgeKind::Configures,
        EdgeKind::Routes,
        EdgeKind::Publishes,
    ] {
        print_edges(&g, kind, limit);
    }
    println!("\n== tagged symbols (external, http.calls)");
    let mut n = 0;
    for (i, s) in g.symbols().iter().enumerate() {
        if s.tags.contains_key("external") || s.tags.contains_key("http.calls") {
            n += 1;
            if n <= limit {
                let tags: Vec<String> = s.tags.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!(
                    "  {} [{}] {}",
                    g.display_name(i as u32),
                    s.file,
                    tags.join(" | ")
                );
            }
        }
    }
    println!("  ({n} total)");
}

fn print_edges(g: &Graph, kind: EdgeKind, limit: usize) {
    let edges: Vec<_> = g.edges().iter().filter(|e| e.kind == kind).collect();
    println!("\n== {kind:?} ({})", edges.len());
    for e in edges.iter().take(limit) {
        println!(
            "  {} -> {}  ({:.2}, line {})",
            g.display_name(e.from),
            g.display_name(e.to),
            e.confidence,
            e.line
        );
    }
}
