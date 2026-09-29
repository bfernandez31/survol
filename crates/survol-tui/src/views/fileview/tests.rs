use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::comments::{self, Anchor, CommentStore};
use survol_core::git::Git;
use survol_core::model::{Diff, LineKind};
use survol_core::review::Review;
use survol_core::review_state::ReviewState;

use super::*;
use crate::views::comments::NoteIndex;

/// `f.rs`: `a`..`j` at the base; `d` becomes `D`, `E` at the head, `j` is
/// removed.
const RAW: &str = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n\
    @@ -3,3 +3,4 @@\n c\n-d\n+D\n+E\n e\n\
    @@ -9,2 +10,1 @@\n i\n-j\n";
const HEAD: &str = "a\nb\nc\nD\nE\ne\nf\ng\nh\ni\n";

fn diff() -> Diff {
    survol_core::diff::parse(RAW.as_bytes()).unwrap()
}

/// `(sign, old, new)` of each code row, `n` for a note line.
fn shape(rows: &[FileRow]) -> Vec<String> {
    let code = |c: &CodeLine| {
        let sign = match c.kind {
            LineKind::Added => '+',
            LineKind::Removed => '-',
            LineKind::Context => ' ',
        };
        let n = |x: Option<u32>| x.map_or("_".into(), |x| x.to_string());
        format!("{sign}{}/{}", n(c.old), n(c.new))
    };
    rows.iter()
        .map(|r| match r {
            FileRow::Code(c) => code(c),
            FileRow::Pair { left, right } => format!(
                "{} | {}",
                left.as_ref().map_or("".into(), code),
                right.as_ref().map_or("".into(), code)
            ),
            FileRow::Note { part, .. } => format!("n{part}"),
        })
        .collect()
}

#[test]
fn whole_file_with_removed_lines_in_place() {
    let d = diff();
    let hunks: Vec<_> = d.file_hunks(0).collect();
    let rows = build_rows(10, &hunks, true, &NoteIndex::default(), Some(0));
    assert_eq!(
        shape(&rows),
        [
            " 1/1", " 2/2", " 3/3", "-4/_", "+_/4", "+_/5", " 5/6", " 6/7", " 7/8", " 8/9",
            " 9/10", "-10/_",
        ]
    );
    // Lines inside the hunks point at the diff, the others do not.
    assert_eq!(rows[2].diff_line(&NoteIndex::default()), Some((0, 0)));
    assert_eq!(rows[7].diff_line(&NoteIndex::default()), None);
    // Without the removed lines: the head only.
    let rows = build_rows(10, &hunks, false, &NoteIndex::default(), Some(0));
    assert_eq!(rows.len(), 10);
    assert!(rows.iter().all(|r| r.new_line().is_some()));
}

#[test]
fn a_pure_removal_goes_after_its_line() {
    let raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -5,1 +4,0 @@\n-e\n";
    let d = survol_core::diff::parse(raw.as_bytes()).unwrap();
    let hunks: Vec<_> = d.file_hunks(0).collect();
    let rows = build_rows(4, &hunks, true, &NoteIndex::default(), Some(0));
    assert_eq!(shape(&rows), [" 1/1", " 2/2", " 3/3", " 4/4", "-5/_"]);
}

#[test]
fn side_by_side_pairs_the_changes_and_keeps_the_notes() {
    let d = diff();
    let mut store = CommentStore::default();
    let line = comments::line_anchor(&d, 0, 2);
    store.add(Anchor::Line { start: None, line }, "why D?");
    let notes = NoteIndex::build(&d, &store, &[]);
    let hunks: Vec<_> = d.file_hunks(0).collect();
    let rows = build_rows(10, &hunks, true, &notes, Some(0));
    assert_eq!(&shape(&rows)[3..8], ["-4/_", "+_/4", "n0", "n1", "+_/5"]);
    let split = split_rows(&rows);
    assert_eq!(
        shape(&split),
        [
            " 1/1 |  1/1",
            " 2/2 |  2/2",
            " 3/3 |  3/3",
            "-4/_ | +_/4",
            " | +_/5",
            "n0",
            "n1",
            " 5/6 |  5/6",
            " 6/7 |  6/7",
            " 7/8 |  7/8",
            " 8/9 |  8/9",
            " 9/10 |  9/10",
            "-10/_ | ",
        ]
    );
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn shared(dir: &std::path::Path) -> Shared {
    std::fs::write(dir.join("f.rs"), HEAD).unwrap();
    let review = Review {
        mr: None,
        base_sha: "base".into(),
        head_sha: "head".into(),
        diff: diff(),
        worktree: dir.to_path_buf(),
        state_key: "test".into(),
        repo: Git::new(dir),
    };
    Shared::new(review, ReviewState::default(), dir.join("state.json"))
}

#[test]
fn keys_walk_changes_threads_and_comment_diff_lines() {
    let dir = tempfile::tempdir().unwrap();
    let mut sh = shared(dir.path());
    let line = comments::line_anchor(&sh.review.diff, 1, 0);
    sh.comments
        .add(Anchor::Line { start: None, line }, "and i?");
    sh.rebuild_notes();
    let mut v = FileView::open(&mut sh, "f.rs", 7).unwrap();
    assert_eq!(v.cursor_line(), 7);
    assert_eq!(v.counts(), (2, 1));
    // n / N: the changes.
    v.on_key(&mut sh, key('N'));
    assert_eq!(v.rows[v.pos.cursor], v.rows[3], "the removed d");
    v.on_key(&mut sh, key('n'));
    assert!(matches!(v.rows[v.pos.cursor], FileRow::Code(c) if c.old == Some(10)));
    // [c: the thread above, on line i.
    v.on_key(&mut sh, key('['));
    v.on_key(&mut sh, key('c'));
    assert!(v.rows[v.pos.cursor].is_note_head());
    // c on it: edit the draft; Enter: its thread.
    assert!(matches!(
        v.on_key(&mut sh, key('c')),
        FileOutcome::Comment(CommentTarget::Draft(_))
    ));
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        v.on_key(&mut sh, enter),
        FileOutcome::Thread(NoteKind::Draft(_))
    ));
    // c on an added line: a comment on that diff line.
    v.on_key(&mut sh, key('g'));
    v.on_key(&mut sh, key('g'));
    v.on_key(&mut sh, key('n'));
    v.on_key(&mut sh, key('j'));
    assert_eq!(
        v.on_key(&mut sh, key('c')),
        FileOutcome::Comment(CommentTarget::Line {
            hunk: 0,
            line: 2,
            start: None
        })
    );
    // Outside the hunks: refused.
    v.on_key(&mut sh, key('g'));
    v.on_key(&mut sh, key('g'));
    assert_eq!(v.on_key(&mut sh, key('c')), FileOutcome::Continue);
    assert!(sh.message().unwrap().contains("only the lines of the diff"));
    // d hides the removed lines, keeping the cursor line; s splits.
    v.on_key(&mut sh, key('G'));
    v.on_key(&mut sh, key('k'));
    let at = v.cursor_line();
    v.on_key(&mut sh, key('d'));
    assert!(
        !v.rows
            .iter()
            .any(|r| matches!(r, FileRow::Code(c) if c.kind == LineKind::Removed))
    );
    assert_eq!(v.cursor_line(), at);
    v.on_key(&mut sh, key('s'));
    assert!(v.rows.iter().all(|r| !matches!(r, FileRow::Code(_))));
    assert_eq!(v.on_key(&mut sh, key('e')), FileOutcome::Edit(at));
    assert_eq!(
        v.on_key(&mut sh, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        FileOutcome::Close
    );
}
