//! Deterministic detection of mechanical changes (lockfiles, generated code...).
//! No LLM involved: these files form the "mechanical / noise" group.

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::model::Diff;

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

#[cfg(test)]
mod tests {
    use super::*;

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
