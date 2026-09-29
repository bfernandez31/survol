//! Persistent "reviewed" state of a review.
//!
//! Reviewed items are keyed by hunk `content_hash` (or [`Diff::file_key`] for
//! files without hunks), not by position: when a new commit arrives, hunks that
//! did not change stay reviewed and only the modified ones come back.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Diff;

const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewState {
    pub version: u32,
    /// Head the state was last saved for.
    pub head_sha: String,
    pub reviewed: BTreeSet<String>,
    /// How the Diff view lists the files (`tree`, `pairs`, `flat`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explorer: Option<String>,
}

impl ReviewState {
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Writes atomically, dropping entries that no longer exist in `diff`.
    pub fn save(&mut self, path: &Path, head_sha: &str, diff: &Diff) -> io::Result<()> {
        let live: BTreeSet<String> = diff
            .hunks
            .iter()
            .map(|h| h.content_hash.clone())
            .chain((0..diff.files.len()).map(|f| diff.file_key(f)))
            .collect();
        self.reviewed.retain(|k| live.contains(k));
        self.version = FORMAT_VERSION;
        self.head_sha = head_sha.to_string();

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
        )?;
        std::fs::rename(tmp, path)
    }

    pub fn is_hunk_reviewed(&self, diff: &Diff, hunk: usize) -> bool {
        self.reviewed.contains(&diff.hunks[hunk].content_hash)
    }

    pub fn set_hunk(&mut self, diff: &Diff, hunk: usize, reviewed: bool) {
        self.set(diff.hunks[hunk].content_hash.clone(), reviewed);
    }

    /// A file is reviewed when all its hunks are (or its own key, if it has none).
    pub fn is_file_reviewed(&self, diff: &Diff, file: usize) -> bool {
        let f = &diff.files[file];
        if f.hunk_ids.is_empty() {
            self.reviewed.contains(&diff.file_key(file))
        } else {
            f.hunk_ids.iter().all(|&h| self.is_hunk_reviewed(diff, h))
        }
    }

    pub fn set_file(&mut self, diff: &Diff, file: usize, reviewed: bool) {
        let f = &diff.files[file];
        if f.hunk_ids.is_empty() {
            self.set(diff.file_key(file), reviewed);
        }
        for &h in &f.hunk_ids {
            self.set_hunk(diff, h, reviewed);
        }
    }

    /// Reviewed hunks count, and total.
    pub fn progress(&self, diff: &Diff) -> (usize, usize) {
        let files = (0..diff.files.len()).filter(|&f| diff.files[f].hunk_ids.is_empty());
        let total = diff.hunks.len() + files.clone().count();
        let done = (0..diff.hunks.len())
            .filter(|&h| self.is_hunk_reviewed(diff, h))
            .count()
            + files.filter(|&f| self.is_file_reviewed(diff, f)).count();
        (done, total)
    }

    fn set(&mut self, key: String, reviewed: bool) {
        if reviewed {
            self.reviewed.insert(key);
        } else {
            self.reviewed.remove(&key);
        }
    }
}

/// `.git/survol/reviews/<key>/state.json`
pub fn state_path(survol_dir: &Path, key: &str) -> PathBuf {
    survol_dir.join("reviews").join(key).join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse;

    const V1: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n\
        @@ -1,1 +1,1 @@\n-a\n+b\n@@ -10,1 +10,1 @@\n-c\n+d\n\
        diff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n";
    // Second hunk changed, first one only moved.
    const V2: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n\
        @@ -3,1 +3,1 @@\n-a\n+b\n@@ -12,1 +12,1 @@\n-c\n+e\n\
        diff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n";

    #[test]
    fn carries_over_unchanged_hunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(dir.path(), "mr-1");
        let d1 = parse(V1.as_bytes()).unwrap();

        let mut s = ReviewState::load(&path).unwrap();
        s.set_file(&d1, 0, true);
        s.set_file(&d1, 1, true);
        assert_eq!(s.progress(&d1), (3, 3));
        s.save(&path, "sha1", &d1).unwrap();

        let d2 = parse(V2.as_bytes()).unwrap();
        let s = ReviewState::load(&path).unwrap();
        assert_eq!(s.head_sha, "sha1");
        assert!(s.is_hunk_reviewed(&d2, 0));
        assert!(!s.is_hunk_reviewed(&d2, 1));
        assert!(!s.is_file_reviewed(&d2, 0));
        assert!(s.is_file_reviewed(&d2, 1));
        assert_eq!(s.progress(&d2), (2, 3));
    }
}
