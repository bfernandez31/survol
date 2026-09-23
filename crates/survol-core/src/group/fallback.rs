//! Deterministic grouping by directory, used when the LLM output is unusable,
//! and the directory partitioning shared with prompt chunking.

use std::collections::BTreeMap;

use super::{Draft, Layer};
use crate::model::Diff;

/// Groups bigger than this (in hunks) are split by subdirectory.
const MAX_GROUP_HUNKS: usize = 40;

/// Directory part of a path (`""` at the root).
pub(super) fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// First `depth` components of the directory of `path`.
fn prefix(path: &str, depth: usize) -> &str {
    let dir = dir_of(path);
    match dir.match_indices('/').nth(depth.saturating_sub(1)) {
        Some((i, _)) if depth > 0 => &dir[..i],
        _ if depth == 0 => "",
        _ => dir,
    }
}

/// `(path, weight, item)`
pub(super) type Item<T> = (String, usize, T);
/// A directory and its items.
pub(super) type Part<T> = (String, Vec<Item<T>>);

/// Splits `(path, weight, item)` into directory groups of at most `max` total
/// weight, going one directory level deeper where a group is too heavy.
/// A single directory heavier than `max` stays one group.
pub(super) fn partition<T>(items: Vec<Item<T>>, max: usize) -> Vec<Part<T>> {
    split(items, 1, max)
}

fn split<T>(items: Vec<Item<T>>, depth: usize, max: usize) -> Vec<Part<T>> {
    let mut by_dir: BTreeMap<String, Vec<Item<T>>> = BTreeMap::new();
    for item in items {
        by_dir
            .entry(prefix(&item.0, depth).to_string())
            .or_default()
            .push(item);
    }
    let mut out = Vec::new();
    for (dir, group) in by_dir {
        let weight: usize = group.iter().map(|i| i.1).sum();
        let deeper = group
            .iter()
            .any(|i| prefix(&i.0, depth + 1).len() > dir.len());
        if weight > max && deeper {
            out.extend(split(group, depth + 1, max));
        } else {
            out.push((dir, group));
        }
    }
    out
}

/// One group per directory (split when big), layers guessed from paths.
pub(super) fn by_directory(diff: &Diff, hunk_ids: &[usize]) -> Vec<Draft> {
    let mut files: Vec<(String, usize, Vec<usize>)> = Vec::new();
    for &id in hunk_ids {
        let path = &diff.files[diff.hunks[id].file].path;
        match files.last_mut() {
            Some(f) if &f.0 == path => {
                f.1 += 1;
                f.2.push(id);
            }
            _ => files.push((path.clone(), 1, vec![id])),
        }
    }
    partition(files, MAX_GROUP_HUNKS)
        .into_iter()
        .map(|(dir, files)| {
            let mut layers: Vec<Layer> = Vec::new();
            for (path, _, ids) in files {
                let name = guess_layer(&path);
                match layers.iter_mut().find(|l| l.name == name) {
                    Some(l) => l.hunk_ids.extend(ids),
                    None => layers.push(Layer {
                        name: name.to_string(),
                        hunk_ids: ids,
                    }),
                }
            }
            Draft {
                title: if dir.is_empty() {
                    "Changes at the repository root".into()
                } else {
                    format!("Changes in {dir}/")
                },
                summary:
                    "Grouped by directory: no functional grouping is available for these hunks."
                        .into(),
                layers,
            }
        })
        .collect()
}

/// Technical layer suggested by a file path.
pub(super) fn guess_layer(path: &str) -> &'static str {
    let p = path.to_ascii_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    let has = |words: &[&str]| words.iter().any(|w| p.contains(w));
    if has(&["test", "spec", "/e2e/", "fixture"]) {
        "tests"
    } else if has(&[
        "migration",
        "changelog",
        "flyway",
        "liquibase",
        "repositor",
        "/dao",
    ]) || ext == "sql"
    {
        "persistence"
    } else if has(&["model", "entity", "entities", "domain", "dto"]) {
        "model"
    } else if matches!(
        name,
        "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "package.json"
            | "cargo.toml"
            | "cmakelists.txt"
            | "makefile"
            | "dockerfile"
    ) || matches!(ext, "cmake" | "gradle" | "kts")
        || has(&[".github/", ".gitlab-ci", "ci/"])
    {
        "build"
    } else if matches!(ext, "md" | "adoc" | "rst" | "txt") || has(&["docs/", "doc/"]) {
        "docs"
    } else if matches!(
        ext,
        "yml" | "yaml" | "properties" | "toml" | "json" | "xml" | "ini" | "conf" | "env"
    ) || has(&["config"])
    {
        "config"
    } else if has(&[
        "controller",
        "/api/",
        "resource",
        "endpoint",
        "/rest/",
        "route",
    ]) {
        "api"
    } else if matches!(
        ext,
        "html" | "css" | "scss" | "less" | "tsx" | "jsx" | "vue"
    ) || has(&["component", "/ui/", "/pages/", "/views/"])
    {
        "ui"
    } else if has(&["service", "usecase", "use-case", "application"]) {
        "service"
    } else {
        "core"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        assert_eq!(prefix("a/b/c/f.rs", 1), "a");
        assert_eq!(prefix("a/b/c/f.rs", 2), "a/b");
        assert_eq!(prefix("a/b/c/f.rs", 3), "a/b/c");
        assert_eq!(prefix("a/b/c/f.rs", 4), "a/b/c");
        assert_eq!(prefix("f.rs", 1), "");
        assert_eq!(prefix("a/f.rs", 0), "");
    }

    #[test]
    fn partitions_heavy_directories_deeper() {
        let items = ["a/x/1", "a/x/2", "a/y/1", "b/1", "a/2"]
            .iter()
            .map(|p| (p.to_string(), 1, ()))
            .collect();
        let dirs: Vec<_> = partition(items, 2).into_iter().map(|(d, _)| d).collect();
        assert_eq!(dirs, ["a", "a/x", "a/y", "b"]);
    }

    #[test]
    fn guesses_layers() {
        assert_eq!(guess_layer("src/test/java/OrderTest.java"), "tests");
        assert_eq!(guess_layer("src/main/resources/application.yml"), "config");
        assert_eq!(guess_layer("db/migration/V1__init.sql"), "persistence");
        assert_eq!(guess_layer("src/order/OrderController.java"), "api");
        assert_eq!(guess_layer("src/order/OrderService.java"), "service");
        assert_eq!(guess_layer("front/app/order-list.component.ts"), "ui");
        assert_eq!(guess_layer("CMakeLists.txt"), "build");
        assert_eq!(guess_layer("src/whisper.cpp"), "core");
    }
}
