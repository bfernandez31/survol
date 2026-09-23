//! Prompt building: hunk compression, templates and chunking by module.

use std::fmt::Write;

use super::Draft;
use super::fallback::{dir_of, partition};
use crate::model::{Diff, FileStatus, Hunk, LineKind};

const GROUP_TEMPLATE: &str = include_str!("../../prompts/group.md");
const MERGE_TEMPLATE: &str = include_str!("../../prompts/merge.md");

/// Changed lines shown per hunk, and their maximum width.
const SNIPPET_LINES: usize = 3;
const SNIPPET_WIDTH: usize = 80;
const SECTION_WIDTH: usize = 60;
/// Project instructions are truncated to this size.
const MAX_INSTRUCTIONS: usize = 8_000;

/// The compressed hunks of one file (or of a slice of a big file).
#[derive(Debug, Clone)]
pub(super) struct Block {
    pub path: String,
    pub hunk_ids: Vec<usize>,
    pub text: String,
}

/// One block per file, split when a file alone exceeds `max_chars`.
pub(super) fn blocks(diff: &Diff, hunk_ids: &[usize], max_chars: usize) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut current_file = None;
    for &id in hunk_ids {
        let hunk = &diff.hunks[id];
        let line = hunk_line(hunk);
        let fits = out
            .last()
            .is_some_and(|b| b.text.len() + line.len() <= max_chars);
        if current_file != Some(hunk.file) || !fits {
            current_file = Some(hunk.file);
            out.push(Block {
                path: diff.files[hunk.file].path.clone(),
                hunk_ids: Vec::new(),
                text: file_line(diff, hunk.file),
            });
        }
        let b = out.last_mut().expect("pushed above");
        b.hunk_ids.push(id);
        b.text.push_str(&line);
    }
    out
}

fn file_line(diff: &Diff, file: usize) -> String {
    let f = &diff.files[file];
    let status = match f.status {
        FileStatus::Added => "added".to_string(),
        FileStatus::Modified => "modified".to_string(),
        FileStatus::Deleted => "deleted".to_string(),
        FileStatus::Renamed | FileStatus::Copied => format!(
            "{} from {}",
            if f.status == FileStatus::Renamed {
                "renamed"
            } else {
                "copied"
            },
            f.old_path.as_deref().unwrap_or("?")
        ),
    };
    let lang = f.language.as_deref().unwrap_or("text");
    format!("file {} ({status}, {lang})\n", f.path)
}

fn hunk_line(hunk: &Hunk) -> String {
    let mut s = format!("[{}] ", hunk.id);
    let section = squash(&hunk.section);
    if !section.is_empty() {
        let _ = write!(s, "@@ {} @@ ", truncate(&section, SECTION_WIDTH));
    }
    let _ = write!(s, "+{} -{}", hunk.added(), hunk.removed());
    let snippet: Vec<String> = hunk
        .lines
        .iter()
        .filter(|l| l.kind != LineKind::Context)
        .map(|l| (l.kind, squash(&l.text)))
        .filter(|(_, t)| !t.is_empty())
        .take(SNIPPET_LINES)
        .map(|(kind, t)| {
            let sign = if kind == LineKind::Added { '+' } else { '-' };
            format!("{sign}{}", truncate(&t, SNIPPET_WIDTH))
        })
        .collect();
    if !snippet.is_empty() {
        let _ = write!(s, " | {}", snippet.join(" ⏎ "));
    }
    s.push('\n');
    s
}

/// Collapses whitespace runs to one space.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn instructions_section(instructions: Option<&str>) -> String {
    match instructions.map(str::trim).filter(|i| !i.is_empty()) {
        Some(i) => format!(
            "\n# Project conventions\n\nThe team describes its architecture as follows. Use it to name capabilities and layers.\n\n{}\n",
            truncate(i, MAX_INSTRUCTIONS)
        ),
        None => String::new(),
    }
}

pub(super) fn group_prompt(blocks: &[Block], instructions: Option<&str>) -> String {
    let count: usize = blocks.iter().map(|b| b.hunk_ids.len()).sum();
    let hunks: String = blocks.iter().map(|b| b.text.as_str()).collect();
    GROUP_TEMPLATE
        .replace("{{count}}", &count.to_string())
        .replace("{{instructions}}", &instructions_section(instructions))
        .replace("{{hunks}}", hunks.trim_end())
}

/// Size of the group prompt around the hunks.
pub(super) fn group_overhead(instructions: Option<&str>) -> usize {
    group_prompt(&[], instructions).len() + 16
}

pub(super) fn merge_prompt(drafts: &[Draft], diff: &Diff, instructions: Option<&str>) -> String {
    let mut groups = String::new();
    for (i, d) in drafts.iter().enumerate() {
        let mut dirs: Vec<&str> = Vec::new();
        for id in d.hunk_ids() {
            let dir = dir_of(&diff.files[diff.hunks[id].file].path);
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        let more = dirs.len().saturating_sub(3);
        dirs.truncate(3);
        let mut dirs = dirs
            .iter()
            .map(|d| if d.is_empty() { "/" } else { d })
            .collect::<Vec<_>>()
            .join(", ");
        if more > 0 {
            let _ = write!(dirs, " +{more}");
        }
        let layers = d
            .layers
            .iter()
            .map(|l| format!("{} {}", l.name, l.hunk_ids.len()))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            groups,
            "[{i}] {} ({dirs}; {layers}) — {}",
            squash(&d.title),
            squash(&d.summary)
        );
    }
    MERGE_TEMPLATE
        .replace("{{instructions}}", &instructions_section(instructions))
        .replace("{{groups}}", groups.trim_end())
}

/// Splits blocks into chunks of at most `budget` characters, keeping
/// directories together as much as possible.
pub(super) fn chunks(blocks: Vec<Block>, budget: usize) -> Vec<Vec<Block>> {
    let items = blocks
        .into_iter()
        .map(|b| (b.path.clone(), b.text.len(), b))
        .collect();
    let mut out: Vec<Vec<Block>> = Vec::new();
    let mut size = 0;
    for (_, part) in partition(items, budget) {
        // A directory that fits in a chunk is never split over two.
        let total: usize = part.iter().map(|i| i.1).sum();
        if total <= budget && size + total > budget {
            size = budget;
        }
        for (_, weight, block) in part {
            if out.is_empty() || size + weight > budget {
                out.push(Vec::new());
                size = 0;
            }
            size += weight;
            out.last_mut().expect("pushed above").push(block);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff() -> Diff {
        let raw = "diff --git a/src/Order.java b/src/Order.java\nnew file mode 100644\n\
            --- /dev/null\n+++ b/src/Order.java\n@@ -0,0 +1,4 @@ class X\n\
            +public class Order {\n+\n+    private final long id;\n+    private String veryLongName = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\";\n";
        crate::diff::parse(raw.as_bytes()).unwrap()
    }

    #[test]
    fn compresses_hunks() {
        let d = diff();
        let b = blocks(&d, &[0], 10_000);
        assert_eq!(b.len(), 1);
        let lines: Vec<_> = b[0].text.lines().collect();
        assert_eq!(lines[0], "file src/Order.java (added, java)");
        assert!(
            lines[1].starts_with(
                "[0] @@ class X @@ +4 -0 | +public class Order { ⏎ +private final long id; ⏎ +private String"
            ),
            "{}",
            lines[1]
        );
        assert!(lines[1].ends_with("…"));
        let p = group_prompt(&b, Some("Hexagonal architecture."));
        assert!(p.contains("There are 1 hunks"));
        assert!(p.contains("# Project conventions"));
        assert!(p.contains("Hexagonal architecture."));
        assert!(!p.contains("{{"));
    }

    #[test]
    fn chunks_keep_directories_together() {
        let block = |p: &str, n| Block {
            path: p.into(),
            hunk_ids: vec![],
            text: "x".repeat(n),
        };
        let c = chunks(
            vec![
                block("a/1", 40),
                block("a/2", 40),
                block("b/x/1", 30),
                block("b/y/1", 30),
                block("c", 10),
            ],
            100,
        );
        let paths: Vec<Vec<&str>> = c
            .iter()
            .map(|c| c.iter().map(|b| b.path.as_str()).collect())
            .collect();
        assert_eq!(paths, [vec!["c", "a/1", "a/2"], vec!["b/x/1", "b/y/1"]]);
    }
}
