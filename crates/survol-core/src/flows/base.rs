//! Graph of the base revision, for the before / after of the flows.

use std::collections::{HashMap, HashSet};

use super::ImpactedFlow;
use crate::config::Config;
use crate::graph::{self, EdgeKind, Graph};
use crate::index::{self, Index};
use crate::model::Diff;
use crate::review::Review;

/// Code bases up to this many indexed files get a complete base graph
/// (parsing is shared with the head through the blob cache, resolution is
/// fast); bigger ones only the neighbourhood of the flows.
pub const FULL_BASE_FILES: usize = 5000;

/// Files of the neighbourhood kept for a bigger code base.
const MAX_BASE_FILES: usize = 3000;

/// Hops around the flows' files, in the head graph.
const HOPS: usize = 3;

/// Files of the base revision to index to compare `flows`: `None` (all)
/// for a code base of up to [`FULL_BASE_FILES`] files; else the files of
/// the flows' steps and of the changed files (old paths), plus their
/// neighbourhood in the head graph ([`HOPS`] file-level dependency hops,
/// both directions), up to [`MAX_BASE_FILES`] files.
pub fn base_paths(head: &Graph, diff: &Diff, flows: &[ImpactedFlow]) -> Option<HashSet<String>> {
    if head.files().len() <= FULL_BASE_FILES {
        return None;
    }
    let mut seeds: Vec<String> = Vec::new();
    for f in flows {
        for s in f.after.iter().flat_map(|a| &a.steps) {
            seeds.push(s.file.clone());
        }
    }
    for f in &diff.files {
        seeds.push(f.path.clone());
        seeds.extend(f.old_path.clone());
    }
    let mut adj: HashMap<&str, HashSet<&str>> = HashMap::new();
    for e in head.edges() {
        if matches!(e.kind, EdgeKind::Tests) {
            continue;
        }
        let (a, b) = (
            head.symbol(e.from).file.as_str(),
            head.symbol(e.to).file.as_str(),
        );
        if a != b {
            adj.entry(a).or_default().insert(b);
            adj.entry(b).or_default().insert(a);
        }
    }
    let mut out: HashSet<String> = seeds.iter().cloned().collect();
    let mut frontier: Vec<String> = out.iter().cloned().collect();
    for _ in 0..HOPS {
        let mut next = Vec::new();
        for f in &frontier {
            for &n in adj.get(f.as_str()).into_iter().flatten() {
                if out.len() >= MAX_BASE_FILES {
                    return Some(out);
                }
                if out.insert(n.to_string()) {
                    next.push(n.to_string());
                }
            }
        }
        frontier = next;
    }
    Some(out)
}

/// Graph of the review's base revision, restricted to [`base_paths`], with
/// the default rules and no diff. Parsed files come from the blob cache
/// shared with the head; the graph is cached next to the head's
/// (`base-graph.json`), keyed by the revisions and the files.
pub fn base_graph(
    review: &Review,
    cfg: &Config,
    head: &Graph,
    flows: &[ImpactedFlow],
    mut progress: impl FnMut(&str),
    use_cache: bool,
) -> crate::Result<Graph> {
    let survol = review.repo.survol_dir()?;
    let rules = graph::default_rules();
    let globs = &cfg.review.mechanical_globs;
    let paths = base_paths(head, &review.diff, flows);
    let mut key = format!(
        "base-{}",
        graph::cache_key(&review.base_sha, &review.head_sha, &rules, globs)
    );
    if let Some(p) = &paths {
        let mut sorted: Vec<&String> = p.iter().collect();
        sorted.sort();
        let mut h = blake3::Hasher::new();
        for s in sorted {
            h.update(s.as_bytes());
            h.update(b"\0");
        }
        key.push('-');
        key.push_str(&h.finalize().to_hex()[..16]);
    }
    let path = survol
        .join("cache")
        .join(&review.head_sha)
        .join("base-graph.json");
    if use_cache && let Some(g) = graph::load_cache(&path, &key) {
        progress("base graph loaded from cache");
        return Ok(g);
    }
    let opts = index::Options::new(globs, Some(&survol))?;
    let only = paths.as_ref().map(|p| move |path: &str| p.contains(path));
    let only_dyn: Option<&dyn Fn(&str) -> bool> = only.as_ref().map(|f| f as &dyn Fn(&str) -> bool);
    let index = Index::build_revision(
        &review.repo,
        &review.base_sha,
        only_dyn,
        &opts,
        &mut progress,
    )?;
    let mut g = graph::build(&index, &Diff::default(), &rules);
    g.set_origin(key, review.base_sha.clone(), review.base_sha.clone());
    let s = g.stats();
    progress(&format!(
        "base graph: {} files, {} symbols, {} edges",
        s.files, s.symbols, s.edges
    ));
    graph::save_cache(&path, &g)?;
    Ok(g)
}
