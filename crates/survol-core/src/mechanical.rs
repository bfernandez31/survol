//! Deterministic detection of mechanical changes (lockfiles, generated code,
//! pure renames, binaries, whitespace-only edits). No LLM involved: these form
//! the "mechanical / noise" group.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::model::{Diff, Hunk, LineKind};

pub const DEFAULT_GLOBS: &[&str] = &[
    "**/package-lock.json",
    "**/yarn.lock",
    "**/pnpm-lock.yaml",
    "**/bun.lockb",
    "**/Cargo.lock",
    "**/gradle.lockfile",
    "**/gradle/wrapper/**",
    "**/gradlew",
    "**/gradlew.bat",
    "**/mvnw",
    "**/mvnw.cmd",
    "**/.mvn/wrapper/**",
    "**/*.min.js",
    "**/*.min.css",
    "**/*.map",
    "**/*.snap",
    "**/generated/**",
    "**/generated-sources/**",
    "**/__generated__/**",
    "**/*.generated.*",
    "**/*.g.dart",
    "**/*.pb.go",
];

pub fn globset(extra: &[String]) -> Result<GlobSet, globset::Error> {
    let mut b = GlobSetBuilder::new();
    for g in DEFAULT_GLOBS
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
    {
        b.add(Glob::new(g)?);
    }
    b.build()
}

/// Sets `is_generated` on files matching the globs.
pub fn mark(diff: &mut Diff, globs: &GlobSet) {
    for f in &mut diff.files {
        f.is_generated = globs.is_match(&f.path);
    }
}

/// Why a change is mechanical; also the layer it goes to in the mechanical group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Lockfile, generated code... (matched a glob).
    Generated,
    /// Only whitespace changed.
    Whitespace,
}

impl Kind {
    pub fn layer(self) -> &'static str {
        match self {
            Kind::Generated => "generated",
            Kind::Whitespace => "formatting",
        }
    }
}

/// Mechanical kind of a hunk, if any.
pub fn hunk_kind(diff: &Diff, hunk: &Hunk) -> Option<Kind> {
    if diff.files[hunk.file].is_generated {
        Some(Kind::Generated)
    } else if is_whitespace_only(hunk) {
        Some(Kind::Whitespace)
    } else {
        None
    }
}

/// Removed and added lines are equal once all whitespace is stripped
/// (reindentation, line wrapping, trailing spaces, blank lines).
pub fn is_whitespace_only(hunk: &Hunk) -> bool {
    let squashed = |kind| -> String {
        hunk.lines
            .iter()
            .filter(|l| l.kind == kind)
            .flat_map(|l| l.text.chars())
            .filter(|c| !c.is_whitespace())
            .collect()
    };
    squashed(LineKind::Removed) == squashed(LineKind::Added)
}

/// Files without any hunk: pure renames or copies, binaries, mode changes,
/// empty files. There is nothing to read in them.
pub fn hunkless_files(diff: &Diff) -> impl Iterator<Item = usize> + '_ {
    (0..diff.files.len()).filter(|&f| diff.files[f].hunk_ids.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_whitespace_only_hunks() {
        let raw = "diff --git a/A.java b/A.java\n--- a/A.java\n+++ b/A.java\n\
            @@ -1,2 +1,4 @@\n-if (a) { b(); }\n+if (a) {\n+    b();\n+}\n x\n\
            @@ -10,1 +11,1 @@\n-int a = 1;\n+int a = 2;\n\
            @@ -20,0 +21,1 @@\n+\n\
            diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n\
            diff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n\
            diff --git a/package-lock.json b/package-lock.json\n--- a/package-lock.json\n\
            +++ b/package-lock.json\n@@ -1 +1 @@\n-1\n+2\n";
        let mut d = crate::diff::parse(raw.as_bytes()).unwrap();
        mark(&mut d, &globset(&[]).unwrap());
        let kinds: Vec<_> = d.hunks.iter().map(|h| hunk_kind(&d, h)).collect();
        assert_eq!(
            kinds,
            [
                Some(Kind::Whitespace),
                None,
                Some(Kind::Whitespace),
                Some(Kind::Generated)
            ]
        );
        let files: Vec<_> = hunkless_files(&d)
            .map(|f| d.files[f].path.as_str())
            .collect();
        assert_eq!(files, ["logo.png", "y"]);
    }

    #[test]
    fn matches_lockfiles_and_generated() {
        let g = globset(&["**/*.lock.json".into()]).unwrap();
        assert!(g.is_match("front/package-lock.json"));
        assert!(g.is_match("package-lock.json"));
        assert!(g.is_match("build/generated/sources/Foo.java"));
        assert!(g.is_match("x/deps.lock.json"));
        assert!(!g.is_match("src/main/java/Generator.java"));
    }
}
