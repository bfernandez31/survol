//! Review comments in the TUI: the notes shown under their line (local
//! drafts and GitLab discussions, rendered from markdown and folded when
//! long), the thread popup, the comment editor and the Review panel.
//! Rendering lives in `ui/comments.rs` (and `ui/rows.rs` for inline notes).

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use survol_core::comments::{self, Anchor, CommentStore, Placement};
use survol_core::forge::Discussion;
use survol_core::model::Diff;

use super::Row;
use crate::highlight::Highlighter;
use crate::ui::ACCENT;
use crate::ui::markdown;

/// Width note bodies are wrapped at until the renderer gives the pane's.
const DEFAULT_WIDTH: usize = 96;
/// Columns left of a note body in the diff pane: the line numbers, then `┃ `
/// (and one spare column).
pub const NOTE_INDENT: usize = 15;
/// Lines of text a folded note shows under its head.
const FOLDED_LINES: usize = 4;

/// Colour family of a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Draft,
    Remote,
    Resolved,
}

impl Tone {
    /// Bar left of the note.
    pub fn bar(self) -> Color {
        match self {
            Tone::Draft => Color::Yellow,
            Tone::Remote => Color::Magenta,
            Tone::Resolved => Color::DarkGray,
        }
    }

    fn head(self) -> Style {
        match self {
            Tone::Draft => Style::new().fg(Color::Yellow).bold(),
            Tone::Remote => Style::new().fg(Color::Magenta).bold(),
            Tone::Resolved => Style::new().dim(),
        }
    }

    /// Base style of the body text.
    pub fn body(self) -> Style {
        match self {
            Tone::Draft => Style::new().fg(Color::Yellow),
            Tone::Remote => Style::new(),
            Tone::Resolved => Style::new().dim(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoteKind {
    Draft(u64),
    /// Index into the fetched discussions.
    Remote(usize),
}

/// A reply shown under a discussion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub author: String,
    pub date: String,
    pub body: String,
}

/// A note as displayed under its line: its raw text, and its screen lines
/// once laid out at the pane's width.
#[derive(Debug, Clone, PartialEq)]
pub struct InlineNote {
    pub kind: NoteKind,
    pub tone: Tone,
    head: String,
    /// Raw markdown body.
    body: String,
    replies: Vec<Reply>,
    /// The line `(hunk, line)` it is about, else under the header of `file`.
    pub at: Option<(usize, usize)>,
    pub file: usize,
    pub lines: Vec<Line<'static>>,
    /// Longer than a folded note: `o` shows it whole.
    pub foldable: bool,
}

impl InlineNote {
    fn new(kind: NoteKind, tone: Tone, head: String, body: String) -> Self {
        Self {
            kind,
            tone,
            head,
            body,
            replies: Vec::new(),
            at: None,
            file: 0,
            lines: Vec::new(),
            foldable: false,
        }
    }

    /// Renders the head, the body and the replies at `width` columns, folded
    /// to a few lines unless `expanded`.
    fn layout(&mut self, width: usize, expanded: bool, hl: Option<&Highlighter>) {
        let body_style = self.tone.body();
        let mut text = markdown::render(&self.body, width, body_style, hl);
        for r in &self.replies {
            let mut head = format!("↳ @{}", r.author);
            if !r.date.is_empty() {
                head.push_str(&format!(" · {}", r.date));
            }
            text.push(Line::from(Span::styled(
                head,
                Style::new().fg(Color::Magenta).dim(),
            )));
            for mut l in markdown::render(&r.body, width.saturating_sub(2), body_style, hl) {
                l.spans.insert(0, Span::raw("  "));
                text.push(l);
            }
        }
        let mut lines = vec![Line::from(Span::styled(
            self.head.clone(),
            self.tone.head(),
        ))];
        // Folding a single line away saves nothing.
        self.foldable = text.len() > FOLDED_LINES + 1;
        let hint = Style::new().fg(ACCENT).dim();
        if self.foldable && !expanded {
            let more = text.len() - FOLDED_LINES;
            lines.extend(text.into_iter().take(FOLDED_LINES));
            lines.push(Line::from(Span::styled(
                format!("▸ {more} more line(s) · o expand · Enter thread"),
                hint,
            )));
        } else {
            lines.extend(text);
            if self.foldable {
                lines.push(Line::from(Span::styled("▾ o fold · Enter thread", hint)));
            }
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
    /// Notes shown whole (`o`), kept across rebuilds.
    pub expanded: HashSet<NoteKind>,
    /// Width the notes are laid out at.
    width: usize,
    /// Code blocks are highlighted (laid out by the renderer).
    highlighted: bool,
    /// Changes whenever the notes or their layout change: views built with
    /// another generation must lay out their rows again.
    pub generation: u64,
}

/// `2026-09-29T10:42:00.000Z` → `2026-09-29 10:42`.
fn short_date(s: &str) -> String {
    s.get(..16).unwrap_or(s).replace('T', " ")
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
            if !first.created_at.is_empty() {
                head.push_str(&format!(" · {}", short_date(&first.created_at)));
            }
            if at.is_none() {
                let line = pos.new_line.or(pos.old_line);
                match line {
                    Some(n) => head.push_str(&format!(" · line {n}, outside the diff shown")),
                    None => head.push_str(" · on the file"),
                }
            }
            let tone = if resolved {
                Tone::Resolved
            } else {
                Tone::Remote
            };
            let mut note = InlineNote::new(NoteKind::Remote(di), tone, head, first.body.clone());
            note.replies = d
                .notes
                .iter()
                .skip(1)
                .filter(|n| !n.system)
                .map(|n| Reply {
                    author: n.author.username.clone(),
                    date: short_date(&n.created_at),
                    body: n.body.clone(),
                })
                .collect();
            ix.push(note, at, file);
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
                    Some((Some((hunk, line)), diff.hunks[hunk].file))
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
            let note = InlineNote::new(NoteKind::Draft(d.id), Tone::Draft, head, d.body.clone());
            ix.push(note, at, file);
        }
        ix.width = DEFAULT_WIDTH;
        ix.layout(None);
        ix
    }

    /// Adds `note` at its place.
    fn push(&mut self, mut note: InlineNote, at: Option<(usize, usize)>, file: usize) -> u32 {
        note.at = at;
        note.file = file;
        self.items.push(note);
        let idx = (self.items.len() - 1) as u32;
        match at {
            Some(hl) => self.by_line.entry(hl).or_default().push(idx),
            None => self.by_file.entry(file).or_default().push(idx),
        }
        idx
    }

    fn layout(&mut self, hl: Option<&Highlighter>) {
        for n in &mut self.items {
            let expanded = self.expanded.contains(&n.kind);
            n.layout(self.width, expanded, hl);
        }
        self.generation += 1;
    }

    /// Lays the notes out at `width` columns, code blocks highlighted.
    pub fn set_width(&mut self, width: usize, hl: &Highlighter) {
        if width == self.width && self.highlighted {
            return;
        }
        self.width = width;
        self.highlighted = true;
        self.layout(Some(hl));
    }

    /// `o`: shows note `note` whole, or folds it back.
    pub fn toggle_fold(&mut self, note: u32, hl: &Highlighter) {
        let Some(n) = self.items.get_mut(note as usize) else {
            return;
        };
        if !self.expanded.remove(&n.kind) {
            self.expanded.insert(n.kind);
        }
        n.layout(self.width, self.expanded.contains(&n.kind), Some(hl));
        self.generation += 1;
    }

    /// Index of the note `kind`, if placed in the diff.
    pub fn find(&self, kind: NoteKind) -> Option<u32> {
        self.items
            .iter()
            .position(|n| n.kind == kind)
            .map(|i| i as u32)
    }

    /// The placed notes in the order of the diff.
    pub fn ordered(&self, diff: &Diff) -> Vec<u32> {
        let mut out = Vec::new();
        for (f, file) in diff.files.iter().enumerate() {
            out.extend_from_slice(self.at_file(f));
            for &h in &file.hunk_ids {
                for l in 0..diff.hunks[h].lines.len() {
                    out.extend_from_slice(self.at_line(h, l));
                }
            }
        }
        out
    }

    pub fn width(&self) -> usize {
        self.width
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

// ----- thread ---------------------------------------------------------------

/// The whole thread of a note (`Enter` on it): the code it is about, the
/// note and its replies, rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadView {
    pub kind: NoteKind,
    pub scroll: usize,
    /// Updated by the renderer.
    pub height: usize,
    pub total: usize,
    /// Back to the Review panel when closed.
    pub from_panel: bool,
    /// Back to the whole-file view when closed.
    pub from_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadOutcome {
    Continue,
    Close,
    /// Reply to the discussion, or edit the draft.
    Write(CommentTarget),
    /// The next (`true`) or previous thread of the diff.
    Next(bool),
}

impl ThreadView {
    pub fn new(kind: NoteKind) -> Self {
        Self {
            kind,
            scroll: 0,
            height: 10,
            total: 0,
            from_panel: false,
            from_file: false,
        }
    }

    pub fn target(&self) -> CommentTarget {
        match self.kind {
            NoteKind::Draft(id) => CommentTarget::Draft(id),
            NoteKind::Remote(d) => CommentTarget::Reply(d),
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.total.saturating_sub(self.height);
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> ThreadOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.height / 2).max(1) as isize;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ThreadOutcome::Close,
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(half * 2),
            KeyCode::PageUp => self.scroll_by(-half * 2),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll_by(isize::MAX / 2),
            KeyCode::Char('c' | 'e') if !ctrl => return ThreadOutcome::Write(self.target()),
            KeyCode::Char('n') | KeyCode::Tab => return ThreadOutcome::Next(true),
            KeyCode::Char('N') | KeyCode::BackTab => return ThreadOutcome::Next(false),
            _ => {}
        }
        ThreadOutcome::Continue
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
    /// Back to this thread when done.
    pub from_thread: Option<NoteKind>,
    /// Back to the whole-file view when done.
    pub from_file: bool,
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
            from_thread: None,
            from_file: false,
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
    /// The whole thread of a draft or discussion.
    Thread(NoteKind),
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
            KeyCode::Char('t') => match row {
                Some(PanelRow::Draft(id)) => return PanelOutcome::Thread(NoteKind::Draft(id)),
                Some(PanelRow::Discussion(d)) => {
                    return PanelOutcome::Thread(NoteKind::Remote(d));
                }
                _ => {}
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

    fn text(n: &InlineNote) -> Vec<String> {
        n.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
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
            text(remote),
            [
                "◆ @alice · unresolved",
                "Why B?",
                "It was b.",
                "↳ @bob",
                "  Renamed.",
            ]
        );
        assert_eq!(remote.tone, Tone::Remote);
        assert_eq!(remote.at, Some((0, 2)));
        assert_eq!(ix.items[at[1] as usize].kind, NoteKind::Draft(id));
        assert_eq!(
            text(&ix.items[at[2] as usize])[0],
            "✎ draft · reply to @alice"
        );
        assert_eq!(ix.at_file(0).len(), 1);

        // A resolved discussion outside the diff goes under the file header.
        let ix = NoteIndex::build(&d, &CommentStore::default(), &[discussion(Some(40), true)]);
        let f = ix.at_file(0);
        let note = &ix.items[f[0] as usize];
        assert_eq!(note.tone, Tone::Resolved);
        assert_eq!(
            text(note)[0],
            "◆ @alice · resolved · line 40, outside the diff shown"
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
        let hl = Highlighter::new();
        let mut ix = NoteIndex::build(&d, &CommentStore::default(), &[disc]);
        let gen0 = ix.generation;
        ix.set_width(20, &hl);
        assert!(ix.generation > gen0, "a new layout is a new generation");
        // Long: folded to its first lines.
        assert!(ix.items[0].foldable);
        assert_eq!(
            text(&ix.items[0]),
            [
                "◆ @alice · unresolved",
                "The lock is taken",
                "after the read: move",
                "it before.",
                "↳ @bob",
                "▸ 4 more line(s) · o expand · Enter thread",
            ]
        );
        // `o`: whole, replies included.
        ix.toggle_fold(0, &hl);
        let whole = text(&ix.items[0]);
        assert_eq!(
            whole[1..whole.len() - 1],
            [
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
        assert!(whole[whole.len() - 1].starts_with("▾ o fold"));
        // Nothing is lost: every line fits.
        assert!(
            whole[1..whole.len() - 1]
                .iter()
                .all(|l| l.chars().count() <= 20)
        );
        let gen1 = ix.generation;
        ix.set_width(20, &hl);
        assert_eq!(ix.generation, gen1, "same width: nothing to redo");
        // The expanded state survives a rebuild.
        assert!(ix.expanded.contains(&NoteKind::Remote(0)));
    }

    #[test]
    fn markdown_bodies_are_rendered() {
        let d = survol_core::diff::parse(RAW.as_bytes()).unwrap();
        let mut store = CommentStore::default();
        let line = comments::line_anchor(&d, 0, 2);
        store.add(
            Anchor::Line { start: None, line },
            "**Why** `B`?\n\n- one\n- two",
        );
        let ix = NoteIndex::build(&d, &store, &[]);
        assert_eq!(
            text(&ix.items[0]),
            ["✎ draft", "Why B?", "", "• one", "• two"]
        );
        let why = &ix.items[0].lines[1].spans[0];
        assert!(
            why.style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(ix.ordered(&d), [0]);
        assert_eq!(ix.find(NoteKind::Draft(1)), Some(0));
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
    fn thread_scrolls_writes_and_moves_on() {
        let mut t = ThreadView::new(NoteKind::Remote(2));
        t.height = 5;
        t.total = 12;
        t.on_key(key(KeyCode::Char('G')));
        assert_eq!(t.scroll, 7);
        t.on_key(key(KeyCode::Char('k')));
        assert_eq!(t.scroll, 6);
        assert_eq!(
            t.on_key(key(KeyCode::Char('c'))),
            ThreadOutcome::Write(CommentTarget::Reply(2))
        );
        assert_eq!(
            t.on_key(key(KeyCode::Char('N'))),
            ThreadOutcome::Next(false)
        );
        assert_eq!(t.on_key(key(KeyCode::Esc)), ThreadOutcome::Close);
        let mut d = ThreadView::new(NoteKind::Draft(4));
        assert_eq!(
            d.on_key(key(KeyCode::Char('e'))),
            ThreadOutcome::Write(CommentTarget::Draft(4))
        );
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
            p.on_key(key(KeyCode::Char('t')), &rows),
            PanelOutcome::Thread(NoteKind::Draft(7))
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
