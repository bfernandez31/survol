//! Changes that only follow a rename or a move: import / package lines, and
//! code that is the same once the renamed identifiers are substituted. No LLM
//! and no grammar: lines are compared as tokens, so any language works.
//!
//! A hunk follows a rename when its removed and added lines (import lines
//! aside, whitespace ignored) are the same token sequence, except for
//! identifiers consistently replaced by others, and every such replacement
//! is a *confirmed* rename: the old name is gone from the whole head
//! revision and the new one did not exist at the base. A call switched from
//! one existing function to another is therefore never taken for a rename.
//! String literals are compared as a whole: a changed URL or message is a
//! real change.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::{Diff, Hunk, LineKind, Side};

/// Why a hunk only follows a rename or a move; also its layer in the group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Only import / package lines changed.
    Imports,
    /// Same code once the renamed identifiers are replaced (import lines aside).
    Renamed,
}

impl Kind {
    pub fn layer(self) -> &'static str {
        match self {
            Kind::Imports => "imports",
            Kind::Renamed => "renames",
        }
    }
}

/// A confirmed rename and the hunks that only follow it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rename {
    pub old: String,
    pub new: String,
    pub hunks: usize,
}

/// The hunks of a diff that only follow a rename or a move.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renames {
    /// By hunk id.
    pub hunks: BTreeMap<usize, Kind>,
    /// Most used first.
    pub renames: Vec<Rename>,
}

/// Classifies the hunks of `diff`. `present(side, names)` returns the names
/// that occur as whole words somewhere in the base (`Side::Old`) or head
/// (`Side::New`) revision. Hunks of generated files are left alone: they are
/// mechanical already.
pub fn detect<E>(
    diff: &Diff,
    mut present: impl FnMut(Side, &BTreeSet<String>) -> Result<HashSet<String>, E>,
) -> Result<Renames, E> {
    let mut out = Renames::default();
    let mut candidates: Vec<(usize, Vec<(&str, &str)>)> = Vec::new();
    for hunk in &diff.hunks {
        let file = &diff.files[hunk.file];
        if file.is_generated {
            continue;
        }
        match shape(hunk, file.language.as_deref()) {
            Some(pairs) if pairs.is_empty() => {
                out.hunks.insert(hunk.id, Kind::Imports);
            }
            Some(pairs) => candidates.push((hunk.id, pairs)),
            None => {}
        }
    }
    if candidates.is_empty() {
        return Ok(out);
    }

    let all = || candidates.iter().flat_map(|(_, pairs)| pairs);
    let olds: BTreeSet<String> = all().map(|p| p.0.to_string()).collect();
    let news: BTreeSet<String> = all().map(|p| p.1.to_string()).collect();
    let still_there = present(Side::New, &olds)?;
    let was_there = present(Side::Old, &news)?;
    let confirmed =
        |&(old, new): &(&str, &str)| !still_there.contains(old) && !was_there.contains(new);

    let mut uses: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (hunk, pairs) in &candidates {
        if pairs.iter().all(confirmed) {
            out.hunks.insert(*hunk, Kind::Renamed);
            for &pair in pairs {
                *uses.entry(pair).or_default() += 1;
            }
        }
    }
    out.renames = uses
        .into_iter()
        .map(|((old, new), hunks)| Rename {
            old: old.to_string(),
            new: new.to_string(),
            hunks,
        })
        .collect();
    out.renames.sort_by_key(|r| std::cmp::Reverse(r.hunks));
    Ok(out)
}

/// The identifier replacements that turn the removed lines of `hunk` into
/// its added lines, import lines aside. Empty: only import lines changed.
/// `None`: the hunk changes something else.
fn shape<'a>(hunk: &'a Hunk, language: Option<&str>) -> Option<Vec<(&'a str, &'a str)>> {
    let code = has_imports(language);
    let mut in_import = false;
    let mut imports = false;
    let (mut old, mut new) = (Vec::new(), Vec::new());
    for line in &hunk.lines {
        let import = code && is_import(&line.text, language, &mut in_import);
        let side = match line.kind {
            LineKind::Context => continue,
            LineKind::Removed => &mut old,
            LineKind::Added => &mut new,
        };
        if import {
            imports = true;
        } else {
            tokens(&line.text, side);
        }
    }
    if old.len() != new.len() {
        return None;
    }
    let mut map: BTreeMap<&str, &str> = BTreeMap::new();
    for (a, b) in old.iter().zip(&new) {
        if a.text == b.text {
            continue;
        }
        if !(a.ident && b.ident) {
            return None;
        }
        if map
            .insert(a.text, b.text)
            .is_some_and(|prev| prev != b.text)
        {
            return None;
        }
    }
    (imports || !map.is_empty()).then(|| map.into_iter().collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Token<'a> {
    text: &'a str,
    ident: bool,
}

/// Splits a line into identifiers, numbers, string literals (one token each)
/// and single punctuation characters; whitespace is dropped.
fn tokens<'a>(line: &'a str, out: &mut Vec<Token<'a>>) {
    let bytes = line.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let start = i;
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        } else if word(b) {
            while i < bytes.len() && word(bytes[i]) {
                i += 1;
            }
        } else if matches!(b, b'"' | b'\'' | b'`') {
            i = closing_quote(bytes, i).unwrap_or(i) + 1;
        } else {
            // One character, whatever its width.
            i += 1;
            while !line.is_char_boundary(i) {
                i += 1;
            }
        }
        out.push(Token {
            text: &line[start..i],
            ident: word(b) && !b.is_ascii_digit(),
        });
    }
}

/// Index of the quote closing the literal opened at `open`, on the same line.
fn closing_quote(bytes: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b if b == bytes[open] => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Identifiers of a line of the repository, string literals and comments
/// included: what [`detect`] expects `present` to look for.
pub fn words(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    line.split(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
        .filter(|w| w.first().is_some_and(|b| !b.is_ascii_digit()))
}

/// Languages where a line starting with `import` is prose or data.
fn has_imports(language: Option<&str>) -> bool {
    !matches!(
        language,
        Some(
            "markdown"
                | "json"
                | "yaml"
                | "xml"
                | "html"
                | "properties"
                | "toml"
                | "sql"
                | "shell"
                | "css"
                | "scss"
        )
    )
}

/// Import, package or re-export line. `open` tracks a multi-line import
/// (`import {` ... `} from '...'`, `import (` ... `)`) across the lines of a
/// hunk. A JavaScript / TypeScript import without bindings (`import './x'`)
/// runs code: it is not counted.
fn is_import(text: &str, language: Option<&str>, open: &mut bool) -> bool {
    let t = text.trim();
    if *open {
        *open = !(t.contains('}') || t.contains(')'));
        return true;
    }
    let quoted = |s: &str| s.starts_with(['"', '\'', '`']);
    let js = matches!(language, Some("typescript" | "tsx" | "javascript"));
    let import = if let Some(rest) = t.strip_prefix("import").filter(|r| {
        r.starts_with([' ', '{', '*']) || r.is_empty() || r.trim_start().starts_with('(')
    }) {
        let rest = rest.trim_start();
        // `import(...)` with something inside is a dynamic import: code.
        !(js && quoted(rest)) && (rest == "(" || !rest.starts_with('('))
    } else if let Some(rest) = t.strip_prefix("from ") {
        rest.contains(" import ") || rest.ends_with(" import")
    } else if t.starts_with("export ") || t.starts_with('}') {
        t.split_once(" from ")
            .is_some_and(|(_, source)| quoted(source.trim_start()))
    } else if let Some(rest) = t.strip_prefix("pub use ").or(t.strip_prefix("use ")) {
        !rest.contains('(') && (rest.ends_with(';') || rest.ends_with('{'))
    } else if let Some(rest) = t.strip_prefix("using ") {
        rest.ends_with(';') && !rest.contains('(')
    } else {
        t.starts_with("package ")
            || t.starts_with("#include")
            || t.starts_with("#import")
            || t.starts_with("require ")
            || t.starts_with("require_relative ")
            || t.contains("= require(")
    };
    if import {
        let opened = |o: char, c: char| t.matches(o).count() > t.matches(c).count();
        *open = opened('{', '}') || opened('(', ')');
    }
    import
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One file per `(path, removed, added)`, each a single hunk.
    fn diff(files: &[(&str, &[&str], &[&str])]) -> Diff {
        let mut raw = String::new();
        for (path, removed, added) in files {
            raw.push_str(&format!(
                "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,{} +1,{} @@\n",
                removed.len(),
                added.len()
            ));
            for l in *removed {
                raw.push_str(&format!("-{l}\n"));
            }
            for l in *added {
                raw.push_str(&format!("+{l}\n"));
            }
        }
        crate::diff::parse(raw.as_bytes()).unwrap()
    }

    /// `head` / `base`: the names found in each revision.
    fn run(d: &Diff, head: &[&str], base: &[&str]) -> Renames {
        detect(d, |side, names| {
            let there = if side == Side::New { head } else { base };
            Ok::<_, ()>(
                names
                    .iter()
                    .filter(|n| there.contains(&n.as_str()))
                    .cloned()
                    .collect(),
            )
        })
        .unwrap()
    }

    #[test]
    fn import_and_package_lines_only() {
        let d = diff(&[
            (
                "A.java",
                &["package com.old;", "import com.old.Foo;"],
                &["package com.fresh.deep;", "import com.fresh.deep.Foo;"],
            ),
            (
                "a.ts",
                &["import { A } from './old/a';"],
                &["import {", "  A,", "  B,", "} from './fresh/a';"],
            ),
            ("b.ts", &["import './old';"], &["import './fresh';"]),
            ("c.py", &["from old import x"], &["from fresh import x"]),
            ("README.md", &["import the old"], &["import a fresh"]),
        ]);
        let r = run(&d, &["the"], &[]);
        assert_eq!(
            r.hunks,
            BTreeMap::from([(0, Kind::Imports), (1, Kind::Imports), (3, Kind::Imports)])
        );
        assert!(r.renames.is_empty());
    }

    #[test]
    fn same_code_once_renamed() {
        let d = diff(&[
            (
                "A.java",
                &["import a.OldName;", "  OldName x = new OldName(1);"],
                &[
                    "import b.NewName;",
                    "  NewName x =",
                    "      new NewName(1);",
                ],
            ),
            ("B.java", &["class OldName {"], &["class NewName {"]),
            // Another change besides the rename.
            ("C.java", &["OldName.run(1);"], &["NewName.run(2);"]),
            // Not the same shape.
            ("D.java", &["OldName.run();"], &["NewName.run().again();"]),
            // A changed string is a real change.
            ("E.java", &["get(\"/old\");"], &["get(\"/fresh\");"]),
        ]);
        let r = run(&d, &["NewName"], &["OldName"]);
        assert_eq!(
            r.hunks,
            BTreeMap::from([(0, Kind::Renamed), (1, Kind::Renamed)])
        );
        assert_eq!(
            r.renames,
            [Rename {
                old: "OldName".into(),
                new: "NewName".into(),
                hunks: 2
            }]
        );
    }

    #[test]
    fn a_call_switched_to_an_existing_name_is_not_a_rename() {
        let d = diff(&[("A.java", &["repo.save(x);"], &["repo.delete(x);"])]);
        // `save` is still used elsewhere at the head.
        assert!(run(&d, &["save", "delete"], &["save"]).hunks.is_empty());
        // `delete` existed at the base.
        assert!(run(&d, &["delete"], &["save", "delete"]).hunks.is_empty());
        assert_eq!(run(&d, &["delete"], &["save"]).hunks.len(), 1);
    }

    #[test]
    fn inconsistent_replacements_are_not_a_rename() {
        let d = diff(&[("A.java", &["a(); a();"], &["b(); c();"])]);
        assert!(run(&d, &[], &[]).hunks.is_empty());
    }

    #[test]
    fn words_of_a_line() {
        let w: Vec<&[u8]> = words(b"x = \"Foo_1 bar\"; // 2nd baz").collect();
        assert_eq!(w, [&b"x"[..], b"Foo_1", b"bar", b"baz"]);
    }
}
