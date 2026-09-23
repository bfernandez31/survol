//! Parser for `git diff` unified output.
//!
//! Expects the output of [`crate::git::Git::diff`], which forces `a/` and `b/`
//! prefixes and disables colors, external diff drivers and path quoting.

use crate::model::{Diff, DiffLine, FileChange, FileStatus, Hunk, LineKind, LineRange};

#[derive(Debug, thiserror::Error)]
#[error("diff parse error at line {line}: {message}")]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

pub fn parse(input: &[u8]) -> Result<Diff, ParseError> {
    Parser::default().run(input)
}

#[derive(Default)]
struct Parser {
    diff: Diff,
    /// Hunk being filled, with the old and new line counts still expected.
    open: Option<(Hunk, u32, u32)>,
}

impl Parser {
    fn run(mut self, input: &[u8]) -> Result<Diff, ParseError> {
        for (n, raw) in input.split(|&b| b == b'\n').enumerate() {
            let line = String::from_utf8_lossy(raw);
            let line = line.strip_suffix('\r').unwrap_or(&line);
            let err = |message: &str| ParseError {
                line: n + 1,
                message: message.to_string(),
            };

            if let Some((hunk, old_left, new_left)) = &mut self.open {
                if let Some(rest) = line.strip_prefix('\\') {
                    // `\ No newline at end of file` applies to the previous line.
                    if rest.contains("newline")
                        && let Some(last) = hunk.lines.last_mut()
                    {
                        last.no_newline = true;
                    }
                    continue;
                }
                if *old_left > 0 || *new_left > 0 {
                    let (kind, text) = match line.as_bytes().first() {
                        Some(b'+') => (LineKind::Added, &line[1..]),
                        Some(b'-') => (LineKind::Removed, &line[1..]),
                        Some(b' ') => (LineKind::Context, &line[1..]),
                        // Some tools strip the trailing space of empty context lines.
                        None => (LineKind::Context, ""),
                        _ => return Err(err("unexpected line inside a hunk")),
                    };
                    let old_next = hunk.old_range.start + (hunk.old_range.len - *old_left);
                    let new_next = hunk.new_range.start + (hunk.new_range.len - *new_left);
                    let (old_line, new_line) = match kind {
                        LineKind::Added => (None, Some(new_next)),
                        LineKind::Removed => (Some(old_next), None),
                        LineKind::Context => (Some(old_next), Some(new_next)),
                    };
                    if kind != LineKind::Added {
                        *old_left = old_left
                            .checked_sub(1)
                            .ok_or_else(|| err("hunk too long"))?;
                    }
                    if kind != LineKind::Removed {
                        *new_left = new_left
                            .checked_sub(1)
                            .ok_or_else(|| err("hunk too long"))?;
                    }
                    hunk.lines.push(DiffLine {
                        kind,
                        old_line,
                        new_line,
                        text: text.to_string(),
                        no_newline: false,
                    });
                    continue;
                }
                self.close_hunk();
            }

            if let Some(rest) = line.strip_prefix("diff --git ") {
                self.diff.files.push(file_from_header(rest));
            } else if let Some(rest) = line.strip_prefix("@@ ") {
                let file = self
                    .diff
                    .files
                    .len()
                    .checked_sub(1)
                    .ok_or_else(|| err("hunk before any file header"))?;
                let (old_range, new_range, section) =
                    parse_hunk_header(rest).ok_or_else(|| err("malformed hunk header"))?;
                let hunk = Hunk {
                    id: self.diff.hunks.len(),
                    file,
                    old_range,
                    new_range,
                    section,
                    lines: Vec::new(),
                    content_hash: String::new(),
                };
                self.open = Some((hunk, old_range.len, new_range.len));
            } else if let Some(file) = self.diff.files.last_mut() {
                file_extended_header(file, line);
            } else if !line.is_empty() {
                return Err(err("content before any file header"));
            }
        }
        self.close_hunk();

        for file in &mut self.diff.files {
            file.language = language_of(&file.path).map(str::to_string);
        }
        Ok(self.diff)
    }

    fn close_hunk(&mut self) {
        if let Some((mut hunk, _, _)) = self.open.take() {
            let file = &mut self.diff.files[hunk.file];
            hunk.content_hash = hunk_hash(file, &hunk.lines);
            file.hunk_ids.push(hunk.id);
            self.diff.hunks.push(hunk);
        }
    }
}

fn file_from_header(rest: &str) -> FileChange {
    // `a/<old> b/<new>`. Only reliable when both paths are equal; the extended
    // header (rename, ---/+++) corrects it otherwise.
    let (old, new) = split_git_header(rest);
    FileChange {
        old_path: (old != new).then(|| old.clone()),
        path: new,
        status: FileStatus::Modified,
        language: None,
        binary: false,
        is_generated: false,
        similarity: None,
        hunk_ids: Vec::new(),
    }
}

fn split_git_header(rest: &str) -> (String, String) {
    let rest = rest.strip_prefix("a/").unwrap_or(rest);
    // Same path on both sides: "<p> b/<p>" has an odd split point in the middle.
    let bytes = rest.len();
    if bytes % 2 == 1 {
        let half = bytes / 2;
        if rest.is_char_boundary(half) && rest[half..].starts_with(" b/") {
            let (a, b) = (&rest[..half], &rest[half + 3..]);
            if a == b {
                return (a.to_string(), b.to_string());
            }
        }
    }
    match rest.find(" b/") {
        Some(i) => (rest[..i].to_string(), rest[i + 3..].to_string()),
        None => (rest.to_string(), rest.to_string()),
    }
}

fn file_extended_header(file: &mut FileChange, line: &str) {
    if line.starts_with("new file mode") {
        file.status = FileStatus::Added;
    } else if line.starts_with("deleted file mode") {
        file.status = FileStatus::Deleted;
    } else if let Some(p) = line.strip_prefix("rename from ") {
        file.status = FileStatus::Renamed;
        file.old_path = Some(p.to_string());
    } else if let Some(p) = line.strip_prefix("rename to ") {
        file.path = p.to_string();
    } else if let Some(p) = line.strip_prefix("copy from ") {
        file.status = FileStatus::Copied;
        file.old_path = Some(p.to_string());
    } else if let Some(p) = line.strip_prefix("copy to ") {
        file.path = p.to_string();
    } else if let Some(s) = line.strip_prefix("similarity index ") {
        file.similarity = s.trim_end_matches('%').parse().ok();
    } else if let Some(p) = line.strip_prefix("--- ") {
        if let Some(p) = p.strip_prefix("a/")
            && file.status != FileStatus::Renamed
            && file.status != FileStatus::Copied
        {
            file.old_path = (p != file.path).then(|| p.to_string());
        }
    } else if let Some(p) = line.strip_prefix("+++ ") {
        match p.strip_prefix("b/") {
            Some(p) => file.path = p.to_string(),
            // `+++ /dev/null`: deleted file, `path` keeps the old path.
            None => file.status = FileStatus::Deleted,
        }
    } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
        file.binary = true;
    }
    if file.status == FileStatus::Deleted || file.status == FileStatus::Added {
        file.old_path = None;
    }
}

/// Parses `-a,b +c,d @@ section`.
fn parse_hunk_header(rest: &str) -> Option<(LineRange, LineRange, String)> {
    let (ranges, section) = rest
        .split_once(" @@")
        .unwrap_or((rest.trim_end_matches(" @@"), ""));
    let (old, new) = ranges.split_once(' ')?;
    Some((
        parse_range(old.strip_prefix('-')?)?,
        parse_range(new.strip_prefix('+')?)?,
        section.trim().to_string(),
    ))
}

fn parse_range(s: &str) -> Option<LineRange> {
    let (start, len) = match s.split_once(',') {
        Some((a, b)) => (a.parse().ok()?, b.parse().ok()?),
        None => (s.parse().ok()?, 1),
    };
    Some(LineRange { start, len })
}

fn hunk_hash(file: &FileChange, lines: &[DiffLine]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(file.path.as_bytes());
    h.update(b"\0");
    for l in lines {
        h.update(match l.kind {
            LineKind::Context => b" ",
            LineKind::Added => b"+",
            LineKind::Removed => b"-",
        });
        h.update(l.text.as_bytes());
        h.update(b"\n");
    }
    h.finalize().to_hex()[..16].to_string()
}

/// Language identifier used for highlighting and, later, tree-sitter.
pub fn language_of(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    Some(match ext.as_deref()? {
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "json" => "json",
        "yml" | "yaml" => "yaml",
        "xml" => "xml",
        "properties" => "properties",
        "sql" => "sql",
        "md" => "markdown",
        "rs" => "rust",
        "py" => "python",
        "go" => "go",
        "sh" | "bash" | "zsh" => "shell",
        "toml" => "toml",
        "gradle" => "groovy",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> Diff {
        parse(s.as_bytes()).expect("parse")
    }

    #[test]
    fn modified_file_line_numbers() {
        let d = parse_str(
            "diff --git a/src/A.java b/src/A.java\n\
             index 1..2 100644\n\
             --- a/src/A.java\n\
             +++ b/src/A.java\n\
             @@ -10,4 +10,5 @@ class A {\n \
             ctx\n\
             -old\n\
             +new1\n\
             +new2\n \
             ctx2\n \
             ctx3\n",
        );
        assert_eq!(d.files.len(), 1);
        let f = &d.files[0];
        assert_eq!(f.path, "src/A.java");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.language.as_deref(), Some("java"));
        let h = &d.hunks[0];
        assert_eq!(h.section, "class A {");
        let nums: Vec<_> = h.lines.iter().map(|l| (l.old_line, l.new_line)).collect();
        assert_eq!(
            nums,
            vec![
                (Some(10), Some(10)),
                (Some(11), None),
                (None, Some(11)),
                (None, Some(12)),
                (Some(12), Some(13)),
                (Some(13), Some(14)),
            ]
        );
        assert_eq!((h.added(), h.removed()), (2, 1));
    }

    #[test]
    fn added_deleted_renamed_binary() {
        let d = parse_str(
            "diff --git a/new.ts b/new.ts\n\
             new file mode 100644\n\
             index 0000000..1\n\
             --- /dev/null\n\
             +++ b/new.ts\n\
             @@ -0,0 +1,2 @@\n\
             +a\n\
             +b\n\
             \\ No newline at end of file\n\
             diff --git a/gone.kt b/gone.kt\n\
             deleted file mode 100644\n\
             index 1..0000000\n\
             --- a/gone.kt\n\
             +++ /dev/null\n\
             @@ -1 +0,0 @@\n\
             -x\n\
             diff --git a/old dir/X.java b/new dir/X.java\n\
             similarity index 100%\n\
             rename from old dir/X.java\n\
             rename to new dir/X.java\n\
             diff --git a/logo.png b/logo.png\n\
             index 1..2 100644\n\
             Binary files a/logo.png and b/logo.png differ\n",
        );
        let kinds: Vec<_> = d
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("new.ts", FileStatus::Added),
                ("gone.kt", FileStatus::Deleted),
                ("new dir/X.java", FileStatus::Renamed),
                ("logo.png", FileStatus::Modified),
            ]
        );
        assert!(d.hunks[0].lines[1].no_newline);
        assert_eq!(d.hunks[1].lines[0].old_line, Some(1));
        assert_eq!(d.files[2].old_path.as_deref(), Some("old dir/X.java"));
        assert_eq!(d.files[2].similarity, Some(100));
        assert!(d.files[2].hunk_ids.is_empty());
        assert!(d.files[3].binary);
        assert!(d.files[0].old_path.is_none() && d.files[1].old_path.is_none());
    }

    #[test]
    fn hunk_body_looking_like_headers() {
        let d = parse_str(
            "diff --git a/a.sql b/a.sql\n\
             --- a/a.sql\n\
             +++ b/a.sql\n\
             @@ -1,2 +1,2 @@\n\
             --- comment\n\
             ++++ plus\n \
             diff --git in text\n",
        );
        assert_eq!(d.files.len(), 1);
        let texts: Vec<_> = d.hunks[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["-- comment", "+++ plus", "diff --git in text"]);
    }

    #[test]
    fn hash_ignores_position() {
        let a = parse_str("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,1 +1,1 @@\n-x\n+y\n");
        let b = parse_str("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -40,1 +42,1 @@\n-x\n+y\n");
        assert_eq!(a.hunks[0].content_hash, b.hunks[0].content_hash);
    }
}
