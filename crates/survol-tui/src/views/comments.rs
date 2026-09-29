//! Review comments in the TUI: the notes shown under their line (local
//! drafts and GitLab discussions), the comment editor and the Review panel.
//! Rendering lives in `ui/comments.rs` (and `ui/rows.rs` for inline notes).

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::comments::{self, Anchor, CommentStore, Placement};
use survol_core::forge::Discussion;
use survol_core::model::Diff;

use super::Row;

/// Width note bodies are wrapped at until the renderer gives the pane's.
const DEFAULT_WIDTH: usize = 96;
/// Columns left of a note body in the diff pane: the line numbers, then `┃ `
/// (and one spare column).
pub const NOTE_INDENT: usize = 15;
/// Body lines shown per note under its line.
const MAX_BODY_LINES: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteStyle {
    DraftHead,
    Draft,
    RemoteHead,
    Remote,
    Resolved,
    Reply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Draft(u64),
    /// Index into the fetched discussions.
    Remote(usize),
}

/// A note as displayed under its line: its raw text, and one entry per
/// screen line once laid out at the pane's width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineNote {
    pub kind: NoteKind,
    head: (NoteStyle, String),
    /// Style of the body lines, and the raw body.
    body: (NoteStyle, String),
    /// Author and raw body of each reply.
    replies: Vec<(String, String)>,
    pub lines: Vec<(NoteStyle, String)>,
}

impl InlineNote {
    fn new(kind: NoteKind, head: (NoteStyle, String), body: (NoteStyle, String)) -> Self {
        Self {
            kind,
            head,
            body,
            replies: Vec::new(),
            lines: Vec::new(),
        }
    }

    /// Wraps the head, body and replies to `width` columns.
    fn layout(&mut self, width: usize) {
        let mut lines = vec![self.head.clone()];
        wrap_into(
            &mut lines,
            self.body.0,
            &self.body.1,
            width,
            "",
            MAX_BODY_LINES,
        );
        for (author, body) in &self.replies {
            lines.push((NoteStyle::Reply, format!("↳ @{author}")));
            wrap_into(
                &mut lines,
                NoteStyle::Reply,
                body,
                width,
                "  ",
                MAX_BODY_LINES,
            );
        }
        self.lines = lines;
    }
}

/// The notes of the review and where they go in the diff.
#[derive(Debug, Clone, Default)]
pub struct NoteIndex {
    pub items: Vec<InlineNote>,
    by_line: HashMap<(usize, usize), Vec<u32>>,
    by_file: HashMap<usize, Vec<u32>>,
    /// Drafts whose line is gone.
    pub stale: usize,
    /// Width the notes are laid out at (0: not yet).
    width: usize,
    /// Changes whenever the notes or their layout change: views built with
    /// another generation must lay out their rows again.
    pub generation: u64,
}

/// Wraps `text` to `width` columns after `indent`, at most `max` lines.
fn wrap_into(
    out: &mut Vec<(NoteStyle, String)>,
    style: NoteStyle,
    text: &str,
    width: usize,
    indent: &str,
    max: usize,
) {
    let mut n = 0;
    for para in text.lines() {
        for l in crate::ui::wrap(para, width.saturating_sub(indent.len())) {
            if n == max {
                out.push((style, format!("{indent}…")));
                return;
            }
            out.push((style, format!("{indent}{l}")));
            n += 1;
        }
    }
}

/// Where a note goes: a line `(hunk, line)`, else under the header of the file.
type Spot = (Option<(usize, usize)>, usize);

impl NoteIndex {
    pub fn build(diff: &Diff, store: &CommentStore, discussions: &[Discussion]) -> Self {
        let mut ix = NoteIndex::default();
        // Discussions first: replies drafted locally follow them.
        let mut disc_at: HashMap<&str, Spot> = HashMap::new();
        for (di, d) in discussions.iter().enumerate() {
            if d.is_system() {
                continue;
            }
            let Some(pos) = d.position() else {
                continue;
            };
            let Some((file, at)) = comments::place_position(pos, diff) else {
                continue;
            };
            let resolved = d.is_resolved();
            let first = &d.notes[0];
            let mut head = format!("◆ @{}", first.author.username);
            if d.is_resolvable() {
                head.push_str(if resolved {
                    " · resolved"
                } else {
                    " · unresolved"
                });
            }
            if at.is_none() {
                let line = pos.new_line.or(pos.old_line);
                match line {
                    Some(n) => head.push_str(&format!(" · line {n}, outside the diff shown")),
                    None => head.push_str(" · on the file"),
                }
            }
            let style = if resolved {
                NoteStyle::Resolved
            } else {
                NoteStyle::RemoteHead
            };
            let body = if resolved {
                NoteStyle::Resolved
            } else {
                NoteStyle::Remote
            };
            let mut note = InlineNote::new(
                NoteKind::Remote(di),
                (style, head),
                (body, first.body.clone()),
            );
            note.replies = d
                .notes
                .iter()
                .skip(1)
                .filter(|n| !n.system)
                .map(|n| (n.author.username.clone(), n.body.clone()))
                .collect();
            let idx = ix.push(note);
            match at {
                Some(hl) => ix.by_line.entry(hl).or_default().push(idx),
                None => ix.by_file.entry(file).or_default().push(idx),
            }
            disc_at.insert(d.id.as_str(), (at, file));
        }
        for d in &store.drafts {
            let p = comments::place(&d.anchor, diff);
            let mut head = "✎ draft".to_string();
            let target = match p {
                Placement::Line {
                    hunk,
                    line,
                    start,
                    moved,
                    ..
                } => {
                    if start.is_some() {
                        let at = comments::describe(diff, p);
                        let path = &diff.files[diff.hunks[hunk].file].path;
                        let lines = at.strip_prefix(path.as_str()).unwrap_or(&at);
                        head.push_str(&format!(" · lines {}", lines.trim_start_matches(':')));
                    }
                    if moved {
                        head.push_str(" · moved: the code around it changed");
                    }
                    Some((Some((hunk, line)), 0))
                }
                Placement::File { file } => {
                    head.push_str(" · on the file");
                    Some((None, file))
                }
                Placement::Reply => {
                    let Anchor::Reply { discussion, author } = &d.anchor else {
                        continue;
                    };
                    head.push_str(&format!(" · reply to @{author}"));
                    disc_at.get(discussion.as_str()).copied()
                }
                Placement::Stale => {
                    ix.stale += 1;
                    None
                }
            };
            let Some((at, file)) = target else {
                continue;
            };
            let idx = ix.push(InlineNote::new(
                NoteKind::Draft(d.id),
                (NoteStyle::DraftHead, head),
                (NoteStyle::Draft, d.body.clone()),
            ));
            match at {
                Some(hl) => ix.by_line.entry(hl).or_default().push(idx),
                None => ix.by_file.entry(file).or_default().push(idx),
            }
        }
        ix.set_width(DEFAULT_WIDTH);
        ix
    }

    fn push(&mut self, note: InlineNote) -> u32 {
        self.items.push(note);
        (self.items.len() - 1) as u32
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// Lays the notes out at `width` columns (a new generation if it changed).
    pub fn set_width(&mut self, width: usize) {
        if width == self.width {
            return;
        }
        self.width = width;
        for n in &mut self.items {
            n.layout(width);
        }
        self.generation += 1;
    }

    pub fn at_line(&self, hunk: usize, line: usize) -> &[u32] {
        self.by_line.get(&(hunk, line)).map_or(&[], Vec::as_slice)
    }

    pub fn at_file(&self, file: usize) -> &[u32] {
        self.by_file.get(&file).map_or(&[], Vec::as_slice)
    }

    /// Rows of the notes `ids`, attached to `hunk` / `line` (none for a file).
    pub fn push_rows(
        &self,
        rows: &mut Vec<Row>,
        ids: &[u32],
        hunk: Option<usize>,
        line: Option<usize>,
    ) {
        for &note in ids {
            let n = self.items[note as usize].lines.len();
            rows.extend((0..n).map(|part| Row::Comment {
                hunk,
                line,
                note,
                part: part as u16,
            }));
        }
    }
}

/// What `c`, `C` or the Review panel want to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentTarget {
    /// A line (index in the hunk), or the range `start..=line`.
    Line {
        hunk: usize,
        line: usize,
        start: Option<usize>,
    },
    File(usize),
    /// An existing draft.
    Draft(u64),
    /// A reply to a discussion (index into the fetched discussions).
    Reply(usize),
    /// The overall comment of the review.
    Summary,
}

/// The comment target of the row under the cursor; `visual`: the other end
/// of a `V` selection.
pub fn row_target(
    notes: &NoteIndex,
    row: Row,
    visual: Option<Row>,
) -> Result<CommentTarget, &'static str> {
    let line_of = |r: Row| match r {
        Row::Line { hunk, line } => Some((hunk, line)),
        Row::Pair { hunk, left, right } => right.or(left).map(|l| (hunk, l)),
        Row::Comment {
            hunk: Some(h),
            line: Some(l),
            ..
        } => Some((h, l)),
        _ => None,
    };
    if let Some(v) = visual {
        let (Some((h1, a)), Some((h2, b))) = (line_of(v), line_of(row)) else {
            return Err("a range goes from one diff line to another");
        };
        if h1 != h2 {
            return Err("a range stays inside one hunk");
        }
        let (start, end) = (a.min(b), a.max(b));
        return Ok(CommentTarget::Line {
            hunk: h1,
            line: end,
            start: (start < end).then_some(start),
        });
    }
    match row {
        Row::Comment { note, .. } => match notes.items[note as usize].kind {
            NoteKind::Draft(id) => Ok(CommentTarget::Draft(id)),
            NoteKind::Remote(d) => Ok(CommentTarget::Reply(d)),
        },
        Row::File(f) | Row::Note(f) => Ok(CommentTarget::File(f)),
        Row::Hunk(_) => Err("move to a line of the hunk (C comments on the file)"),
        Row::Spacer => Err("move to a diff line"),
        r => line_of(r)
            .map(|(hunk, line)| CommentTarget::Line {
                hunk,
                line,
                start: None,
            })
            .ok_or("move to a diff line"),
    }
}

// ----- editor ---------------------------------------------------------------

/// A comment being written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub target: CommentTarget,
    pub title: String,
    /// Code or note the comment is about, shown above the text.
    pub context: Vec<String>,
    pub text: String,
    original: String,
    /// Esc pressed once on a modified text.
    pub confirm_discard: bool,
    /// Back to the Review panel when done.
    pub from_panel: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorOutcome {
    Continue,
    Cancel,
    Save(String),
}

impl Editor {
    pub fn new(target: CommentTarget, title: String, context: Vec<String>, text: String) -> Self {
        Self {
            target,
            title,
            context,
            original: text.clone(),
            text,
            confirm_discard: false,
            from_panel: false,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> EditorOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if key.code != KeyCode::Esc {
            self.confirm_discard = false;
        }
        match key.code {
            KeyCode::Esc => {
                if self.text == self.original || self.confirm_discard {
                    return EditorOutcome::Cancel;
                }
                self.confirm_discard = true;
            }
            KeyCode::Char('s') if ctrl => return EditorOutcome::Save(self.text.clone()),
            KeyCode::Enter if alt || ctrl => return EditorOutcome::Save(self.text.clone()),
            KeyCode::Enter => self.text.push('\n'),
            KeyCode::Tab => self.text.push_str("    "),
            KeyCode::Backspace => {
                self.text.pop();
            }
            KeyCode::Char('u') if ctrl => {
                let keep = self.text.rfind('\n').map_or(0, |i| i + 1);
                self.text.truncate(keep);
            }
            KeyCode::Char(c) if !ctrl => self.text.push(c),
            _ => {}
        }
        EditorOutcome::Continue
    }
}

// ----- Review panel ---------------------------------------------------------

/// A line of the Review panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelRow {
    Summary,
    Draft(u64),
    /// Index into the fetched discussions.
    Discussion(usize),
}

/// Rows of the panel: the summary, the drafts in diff order (stale last),
/// then the merge request's discussions.
pub fn panel_rows(diff: &Diff, store: &CommentStore, discussions: &[Discussion]) -> Vec<PanelRow> {
    let mut rows = vec![PanelRow::Summary];
    let mut drafts: Vec<(usize, usize, usize, u64)> = store
        .drafts
        .iter()
        .map(|d| {
            let key = match comments::place(&d.anchor, diff) {
                Placement::Line {
                    file, hunk, line, ..
                } => (0, file, hunk * 100_000 + line),
                Placement::File { file } => (0, file, 0),
                Placement::Reply => (1, 0, 0),
                Placement::Stale => (2, 0, 0),
            };
            (key.0, key.1, key.2, d.id)
        })
        .collect();
    drafts.sort();
    rows.extend(drafts.into_iter().map(|d| PanelRow::Draft(d.3)));
    rows.extend(
        discussions
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.is_system())
            .map(|(i, _)| PanelRow::Discussion(i)),
    );
    rows
}

/// What the Review panel is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    Delete(u64),
    Publish {
        plan: comments::Plan,
        /// Show the JSON of the requests.
        json: bool,
        scroll: usize,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewPanel {
    pub sel: usize,
    pub confirm: Option<Confirm>,
    /// Updated by the renderer.
    pub height: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelOutcome {
    Continue,
    Close,
    Edit(CommentTarget),
    /// Show where this row is (draft or discussion).
    Jump(PanelRow),
    Delete(u64),
    /// Prepare the publication (then confirm).
    AskPublish,
    Publish(comments::Plan),
    Refresh,
}

impl ReviewPanel {
    pub fn on_key(&mut self, key: KeyEvent, rows: &[PanelRow]) -> PanelOutcome {
        if let Some(c) = self.confirm.take() {
            return match c {
                Confirm::Delete(id) => match key.code {
                    KeyCode::Char('y' | 'Y') => PanelOutcome::Delete(id),
                    _ => PanelOutcome::Continue,
                },
                Confirm::Publish { plan, json, scroll } => match key.code {
                    KeyCode::Char('y' | 'Y') => PanelOutcome::Publish(plan),
                    KeyCode::Char('J') => {
                        self.confirm = Some(Confirm::Publish {
                            plan,
                            json: !json,
                            scroll: 0,
                        });
                        PanelOutcome::Continue
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        self.confirm = Some(Confirm::Publish {
                            plan,
                            json,
                            scroll: scroll + 1,
                        });
                        PanelOutcome::Continue
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.confirm = Some(Confirm::Publish {
                            plan,
                            json,
                            scroll: scroll.saturating_sub(1),
                        });
                        PanelOutcome::Continue
                    }
                    _ => PanelOutcome::Continue,
                },
            };
        }
        let row = rows.get(self.sel).copied();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('P') => return PanelOutcome::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                self.sel = (self.sel + 1).min(rows.len().saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => self.sel = 0,
            KeyCode::Char('G') | KeyCode::End => self.sel = rows.len().saturating_sub(1),
            KeyCode::Char('p') => return PanelOutcome::AskPublish,
            KeyCode::Char('r') => return PanelOutcome::Refresh,
            KeyCode::Char('S') => return PanelOutcome::Edit(CommentTarget::Summary),
            KeyCode::Char('e' | 'c') => match row {
                Some(PanelRow::Summary) => return PanelOutcome::Edit(CommentTarget::Summary),
                Some(PanelRow::Draft(id)) => return PanelOutcome::Edit(CommentTarget::Draft(id)),
                Some(PanelRow::Discussion(d)) => {
                    return PanelOutcome::Edit(CommentTarget::Reply(d));
                }
                None => {}
            },
            KeyCode::Char('d' | 'x') => {
                if let Some(PanelRow::Draft(id)) = row {
                    self.confirm = Some(Confirm::Delete(id));
                }
            }
            KeyCode::Enter => match row {
                Some(PanelRow::Summary) => return PanelOutcome::Edit(CommentTarget::Summary),
                Some(r) => return PanelOutcome::Jump(r),
                None => {}
            },
            _ => {}
        }
        PanelOutcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use survol_core::forge::{Author, Note, Position};

    const RAW: &str = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n\
        @@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn discussion(new_line: Option<u32>, resolved: bool) -> Discussion {
        let note = |id, user: &str, body: &str| Note {
            id,
            body: body.into(),
            author: Author {
                username: user.into(),
                name: String::new(),
            },
            created_at: String::new(),
            system: false,
            resolvable: true,
            resolved,
            position: None,
        };
        let mut first = note(1, "alice", "Why B?\nIt was b.");
        first.position = Some(Position {
            position_type: "text".into(),
            base_sha: "b".into(),
            start_sha: "s".into(),
            head_sha: "h".into(),
            old_path: "f.rs".into(),
            new_path: "f.rs".into(),
            old_line: None,
            new_line,
            line_range: None,
        });
        Discussion {
            id: "d1".into(),
            individual_note: false,
            notes: vec![first, note(2, "bob", "Renamed.")],
        }
    }

    #[test]
    fn indexes_drafts_and_discussions_under_their_line() {
        let d = survol_core::diff::parse(RAW.as_bytes()).unwrap();
        let mut store = CommentStore::default();
        let line = comments::line_anchor(&d, 0, 2);
        let id = store.add(Anchor::Line { start: None, line }, "draft body");
        store.add(comments::file_anchor(&d, 0), "about the file");
        store.add(
            Anchor::Reply {
                discussion: "d1".into(),
                author: "alice".into(),
            },
            "ok",
        );
        let ix = NoteIndex::build(&d, &store, &[discussion(Some(2), false)]);
        let at = ix.at_line(0, 2);
        assert_eq!(at.len(), 3, "discussion, draft, reply on the + line");
        let remote = &ix.items[at[0] as usize];
        assert_eq!(remote.kind, NoteKind::Remote(0));
        assert_eq!(
            remote.lines,
            [
                (NoteStyle::RemoteHead, "◆ @alice · unresolved".to_string()),
                (NoteStyle::Remote, "Why B?".to_string()),
                (NoteStyle::Remote, "It was b.".to_string()),
                (NoteStyle::Reply, "↳ @bob".to_string()),
                (NoteStyle::Reply, "  Renamed.".to_string()),
            ]
        );
        assert_eq!(ix.items[at[1] as usize].kind, NoteKind::Draft(id));
        assert_eq!(
            ix.items[at[2] as usize].lines[0].1,
            "✎ draft · reply to @alice"
        );
        assert_eq!(ix.at_file(0).len(), 1);

        // A resolved discussion outside the diff goes under the file header.
        let ix = NoteIndex::build(&d, &CommentStore::default(), &[discussion(Some(40), true)]);
        let f = ix.at_file(0);
        assert_eq!(
            ix.items[f[0] as usize].lines[0],
            (
                NoteStyle::Resolved,
                "◆ @alice · resolved · line 40, outside the diff shown".to_string()
            )
        );

        let rows = panel_rows(&d, &store, &[discussion(Some(2), false)]);
        assert_eq!(rows[0], PanelRow::Summary);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[4], PanelRow::Discussion(0));
    }

    #[test]
    fn notes_wrap_at_the_pane_width_and_show_replies_fully() {
        let d = survol_core::diff::parse(RAW.as_bytes()).unwrap();
        let mut disc = discussion(Some(2), false);
        disc.notes[0].body = "The lock is taken after the read: move it before.".into();
        disc.notes[1].body = "Fixed.\nThe read now happens under the lock.".into();
        let mut ix = NoteIndex::build(&d, &CommentStore::default(), &[disc]);
        let gen0 = ix.generation;
        ix.set_width(20);
        assert!(ix.generation > gen0, "a new layout is a new generation");
        let text: Vec<&str> = ix.items[0].lines.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            text,
            [
                "◆ @alice · unresolved",
                "The lock is taken",
                "after the read: move",
                "it before.",
                "↳ @bob",
                "  Fixed.",
                "  The read now",
                "  happens under the",
                "  lock.",
            ]
        );
        // Nothing is lost: every word of the bodies is on some line.
        assert!(text.iter().all(|l| l.chars().count() <= 21));
        let gen1 = ix.generation;
        ix.set_width(20);
        assert_eq!(ix.generation, gen1, "same width: nothing to redo");
    }

    #[test]
    fn targets_of_rows_and_ranges() {
        let notes = NoteIndex::default();
        let line = |line| Row::Line { hunk: 3, line };
        assert_eq!(
            row_target(&notes, line(2), None),
            Ok(CommentTarget::Line {
                hunk: 3,
                line: 2,
                start: None
            })
        );
        assert_eq!(
            row_target(&notes, line(1), Some(line(4))),
            Ok(CommentTarget::Line {
                hunk: 3,
                line: 4,
                start: Some(1)
            })
        );
        assert!(row_target(&notes, line(1), Some(Row::Line { hunk: 4, line: 0 })).is_err());
        assert_eq!(
            row_target(
                &notes,
                Row::Pair {
                    hunk: 3,
                    left: Some(1),
                    right: None
                },
                None
            ),
            Ok(CommentTarget::Line {
                hunk: 3,
                line: 1,
                start: None
            })
        );
        assert_eq!(
            row_target(&notes, Row::File(2), None),
            Ok(CommentTarget::File(2))
        );
        assert!(row_target(&notes, Row::Hunk(3), None).is_err());
    }

    #[test]
    fn editor_saves_and_guards_against_losing_text() {
        let mut e = Editor::new(CommentTarget::Summary, "t".into(), vec![], String::new());
        assert_eq!(e.on_key(key(KeyCode::Esc)), EditorOutcome::Cancel);
        for c in "ok".chars() {
            e.on_key(key(KeyCode::Char(c)));
        }
        e.on_key(key(KeyCode::Enter));
        e.on_key(key(KeyCode::Char('!')));
        assert_eq!(e.on_key(key(KeyCode::Esc)), EditorOutcome::Continue);
        assert!(e.confirm_discard);
        e.on_key(key(KeyCode::Backspace));
        assert!(!e.confirm_discard);
        assert_eq!(
            e.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            EditorOutcome::Save("ok\n".into())
        );
    }

    #[test]
    fn panel_confirms_deletion_and_publication() {
        let rows = [PanelRow::Summary, PanelRow::Draft(7)];
        let mut p = ReviewPanel::default();
        assert_eq!(
            p.on_key(key(KeyCode::Enter), &rows),
            PanelOutcome::Edit(CommentTarget::Summary)
        );
        p.on_key(key(KeyCode::Char('j')), &rows);
        assert_eq!(
            p.on_key(key(KeyCode::Char('d')), &rows),
            PanelOutcome::Continue
        );
        assert_eq!(p.confirm, Some(Confirm::Delete(7)));
        assert_eq!(
            p.on_key(key(KeyCode::Char('n')), &rows),
            PanelOutcome::Continue
        );
        assert_eq!(p.confirm, None);
        p.on_key(key(KeyCode::Char('x')), &rows);
        assert_eq!(
            p.on_key(key(KeyCode::Char('y')), &rows),
            PanelOutcome::Delete(7)
        );
        assert_eq!(
            p.on_key(key(KeyCode::Enter), &rows),
            PanelOutcome::Jump(PanelRow::Draft(7))
        );
        assert_eq!(
            p.on_key(key(KeyCode::Char('p')), &rows),
            PanelOutcome::AskPublish
        );
        let plan = comments::Plan {
            mode: comments::Mode::Drafts,
            comments: vec![],
            skipped: vec![],
        };
        p.confirm = Some(Confirm::Publish {
            plan: plan.clone(),
            json: false,
            scroll: 0,
        });
        p.on_key(key(KeyCode::Char('J')), &rows);
        assert!(matches!(
            p.confirm,
            Some(Confirm::Publish { json: true, .. })
        ));
        assert_eq!(
            p.on_key(key(KeyCode::Char('y')), &rows),
            PanelOutcome::Publish(plan)
        );
    }
}
