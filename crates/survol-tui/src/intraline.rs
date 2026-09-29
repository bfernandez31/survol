//! Intraline changes: the words that differ between a removed line and the
//! added line paired with it, to highlight them on top of the line colour.
//!
//! Lines are split into tokens (identifier runs, whitespace runs, single
//! punctuation characters) and compared with a longest common subsequence.
//! Pairs that differ too much get no highlight: it would cover most of the
//! line and add noise, not information.

use std::ops::Range;

use survol_core::model::LineKind;

/// Changed character ranges of one line (character indices, not bytes).
pub type Words = Vec<Range<usize>>;

/// Above this share of changed non-blank characters (both lines together),
/// a pair is not highlighted.
const MAX_CHANGED: f32 = 0.7;
/// Token-table size limit, to keep the LCS cheap on long lines.
const MAX_CELLS: usize = 250_000;

fn class(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        0
    } else if c.is_whitespace() {
        1
    } else {
        2
    }
}

/// Token ranges of `chars`: runs of word characters or of blanks, and single
/// punctuation characters.
fn tokens(chars: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let k = class(chars[i]);
        let mut j = i + 1;
        if k != 2 {
            while j < chars.len() && class(chars[j]) == k {
                j += 1;
            }
        }
        out.push(i..j);
        i = j;
    }
    out
}

/// Which tokens of `a` and `b` belong to a longest common subsequence.
fn common(
    a: &[char],
    at: &[Range<usize>],
    b: &[char],
    bt: &[Range<usize>],
) -> (Vec<bool>, Vec<bool>) {
    let (n, m) = (at.len(), bt.len());
    let eq = |i: usize, j: usize| a[at[i].clone()] == b[bt[j].clone()];
    // len[i][j]: LCS length of the suffixes at[i..] and bt[j..].
    let mut len = vec![0u32; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            len[idx(i, j)] = if eq(i, j) {
                len[idx(i + 1, j + 1)] + 1
            } else {
                len[idx(i + 1, j)].max(len[idx(i, j + 1)])
            };
        }
    }
    let (mut ka, mut kb) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if eq(i, j) {
            ka[i] = true;
            kb[j] = true;
            i += 1;
            j += 1;
        } else if len[idx(i + 1, j)] >= len[idx(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (ka, kb)
}

/// Merged ranges of the tokens not kept, blanks alone left out; and the
/// number of changed and of all non-blank characters.
fn changed(chars: &[char], toks: &[Range<usize>], kept: &[bool]) -> (Words, usize, usize) {
    let mut out: Words = Vec::new();
    let mut changed = 0;
    for (t, &k) in toks.iter().zip(kept) {
        let blank = chars[t.clone()].iter().all(|c| c.is_whitespace());
        if k {
            continue;
        }
        if !blank {
            changed += t.len();
        }
        match out.last_mut() {
            Some(last) if last.end == t.start => last.end = t.end,
            _ if blank => {}
            _ => out.push(t.clone()),
        }
    }
    // A run may end with blanks swallowed from the middle: trim them.
    for r in &mut out {
        while r.end > r.start && chars[r.end - 1].is_whitespace() {
            r.end -= 1;
        }
    }
    let total = chars.iter().filter(|c| !c.is_whitespace()).count();
    (out, changed, total)
}

/// Changed words of a removed line `old` and its added counterpart `new`, or
/// `None` when the lines differ too much (or not at all) to be worth it.
pub fn pair(old: &str, new: &str) -> Option<(Words, Words)> {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let (at, bt) = (tokens(&a), tokens(&b));
    if at.len().saturating_mul(bt.len()) > MAX_CELLS {
        return None;
    }
    let (ka, kb) = common(&a, &at, &b, &bt);
    let (wa, ca, ta) = changed(&a, &at, &ka);
    let (wb, cb, tb) = changed(&b, &bt, &kb);
    let share = (ca + cb) as f32 / (ta + tb).max(1) as f32;
    if (wa.is_empty() && wb.is_empty()) || share > MAX_CHANGED {
        return None;
    }
    Some((wa, wb))
}

/// Changed words of every line of a hunk: in each run of removed lines
/// followed by added lines, the n-th removed line is paired with the n-th
/// added one. Other lines get none.
pub fn hunk<'a>(lines: impl IntoIterator<Item = (LineKind, &'a str)>) -> Vec<Words> {
    let lines: Vec<_> = lines.into_iter().collect();
    let mut out = vec![Words::new(); lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if lines[i].0 != LineKind::Removed {
            i += 1;
            continue;
        }
        let del = i;
        while i < lines.len() && lines[i].0 == LineKind::Removed {
            i += 1;
        }
        let add = i;
        while i < lines.len() && lines[i].0 == LineKind::Added {
            i += 1;
        }
        for k in 0..(add - del).min(i - add) {
            if let Some((a, b)) = pair(lines[del + k].1, lines[add + k].1) {
                out[del + k] = a;
                out[add + k] = b;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use LineKind::{Added, Context, Removed};

    #[test]
    fn highlights_an_added_trailing_comma() {
        let (old, new) = pair(
            r#"    "regime-supprimer.spec.ts""#,
            r#"    "regime-supprimer.spec.ts","#,
        )
        .unwrap();
        assert!(old.is_empty());
        assert_eq!(new, vec![30..31]);
    }

    #[test]
    fn highlights_a_changed_argument() {
        let (old, new) = pair(
            "    await page.waitForTimeout(500);",
            "    await page.waitForTimeout(1000);",
        )
        .unwrap();
        assert_eq!(old, vec![30..33]);
        assert_eq!(new, vec![30..34]);
    }

    #[test]
    fn merges_neighbouring_tokens() {
        let (old, new) = pair("x = a.getCode();", "x = a.getCodeDroit(y);").unwrap();
        assert_eq!(old, vec![6..13]);
        assert_eq!(new, vec![6..18, 19..20]);
    }

    #[test]
    fn skips_rewritten_and_identical_lines() {
        assert_eq!(
            pair("  ancien: number;", "  readonly valeur: string | null;"),
            None
        );
        assert_eq!(pair("same();", "same();"), None);
        // Indentation only: nothing visible to highlight.
        assert_eq!(pair("  x();", "    x();"), None);
    }

    #[test]
    fn counts_characters_not_bytes() {
        let (old, new) = pair("délai = 500;", "délai = 800;").unwrap();
        assert_eq!(old, vec![8..11]);
        assert_eq!(new, vec![8..11]);
    }

    #[test]
    fn pairs_removed_and_added_runs_in_order() {
        let words = hunk([
            (Context, "{"),
            (Removed, "a = 1;"),
            (Removed, "b = 2;"),
            (Added, "a = 10;"),
            (Context, "}"),
            (Added, "c = 3;"),
        ]);
        assert_eq!(words[1], vec![4..5]);
        assert_eq!(words[3], vec![4..6]);
        // b has no counterpart; c follows a context line.
        assert!(words[2].is_empty() && words[5].is_empty());
        assert!(words[0].is_empty() && words[4].is_empty());
    }
}
