//! Data model shared by the engine, the CLI and the TUI.

use serde::{Deserialize, Serialize};

/// A merge request as seen by survol, independent of the forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeRequest {
    /// Forge-side project path, e.g. `group/sub/project`.
    pub project: String,
    pub iid: u64,
    pub title: String,
    pub description: String,
    pub source_branch: String,
    pub target_branch: String,
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
    pub web_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Path on the new side (the old path for a deleted file).
    pub path: String,
    /// Path on the old side, when it differs from `path`.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub language: Option<String>,
    pub binary: bool,
    /// Matched a mechanical glob (lockfile, generated code...).
    pub is_generated: bool,
    /// Rename similarity reported by git, in percent.
    pub similarity: Option<u8>,
    /// Indexes into [`Diff::hunks`].
    pub hunk_ids: Vec<usize>,
}

impl FileChange {
    pub fn display_path(&self) -> String {
        match &self.old_path {
            Some(old) if old != &self.path => format!("{old} → {}", self.path),
            _ => self.path.clone(),
        }
    }
}

/// A line range as written in a hunk header: `start` is 1-based, `len` may be 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u32,
    pub len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
    /// `\ No newline at end of file` follows this line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_newline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub id: usize,
    /// Index into [`Diff::files`].
    pub file: usize,
    pub old_range: LineRange,
    pub new_range: LineRange,
    /// Text after the second `@@`, usually the enclosing function.
    pub section: String,
    pub lines: Vec<DiffLine>,
    /// Stable identity of the hunk, independent of its position in the file.
    /// Used to carry the review state over new commits.
    pub content_hash: String,
}

impl Hunk {
    pub fn added(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| l.kind == LineKind::Added)
            .count()
    }

    pub fn removed(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| l.kind == LineKind::Removed)
            .count()
    }
}

/// Whole parsed diff of a review.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    pub files: Vec<FileChange>,
    pub hunks: Vec<Hunk>,
}

impl Diff {
    pub fn file_hunks(&self, file: usize) -> impl Iterator<Item = &Hunk> {
        self.files[file].hunk_ids.iter().map(|&h| &self.hunks[h])
    }

    /// Key identifying a file with no hunk (binary, pure rename...) in the review state.
    pub fn file_key(&self, file: usize) -> String {
        let f = &self.files[file];
        let mut h = blake3::Hasher::new();
        h.update(b"file\0");
        h.update(f.old_path.as_deref().unwrap_or("").as_bytes());
        h.update(b"\0");
        h.update(f.path.as_bytes());
        h.update(format!("\0{:?}", f.status).as_bytes());
        h.finalize().to_hex()[..16].to_string()
    }

    pub fn stats(&self) -> (usize, usize) {
        self.hunks
            .iter()
            .fold((0, 0), |(a, r), h| (a + h.added(), r + h.removed()))
    }

    /// Reorders files as a directory tree (each directory's files together,
    /// before its subdirectories), renumbering hunks to follow.
    pub fn sort_as_tree(&mut self) {
        let mut order: Vec<usize> = (0..self.files.len()).collect();
        order.sort_by(|&a, &b| tree_key(&self.files[a].path).cmp(&tree_key(&self.files[b].path)));

        let mut old_files: Vec<Option<FileChange>> = self.files.drain(..).map(Some).collect();
        let mut old_hunks: Vec<Option<Hunk>> = self.hunks.drain(..).map(Some).collect();
        for (new_file, &old_file) in order.iter().enumerate() {
            let mut file = old_files[old_file].take().expect("each file once");
            for id in &mut file.hunk_ids {
                let mut hunk = old_hunks[*id].take().expect("each hunk once");
                hunk.id = self.hunks.len();
                hunk.file = new_file;
                *id = hunk.id;
                self.hunks.push(hunk);
            }
            self.files.push(file);
        }
    }
}

/// Directory components, then file name.
fn tree_key(path: &str) -> (Vec<&str>, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (dir.split('/').collect(), name),
        None => (Vec::new(), path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_files_as_tree() {
        let raw = [
            ".github/w.yml",
            ".gitignore",
            "a-b/x",
            "a/z/y",
            "a/b",
            "README",
        ]
        .iter()
        .map(|p| {
            format!("diff --git a/{p} b/{p}\n--- a/{p}\n+++ b/{p}\n@@ -1 +1 @@\n-{p}\n+{p}!\n")
        })
        .collect::<String>();
        let mut d = crate::diff::parse(raw.as_bytes()).unwrap();
        d.sort_as_tree();
        let paths: Vec<_> = d.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                ".gitignore",
                "README",
                ".github/w.yml",
                "a/b",
                "a/z/y",
                "a-b/x"
            ]
        );
        for (i, f) in d.files.iter().enumerate() {
            let h = &d.hunks[f.hunk_ids[0]];
            assert_eq!((h.id, h.file), (i, i));
            assert_eq!(h.lines[0].text, f.path);
        }
    }
}
