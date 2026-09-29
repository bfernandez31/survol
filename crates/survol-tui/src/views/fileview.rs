//! Whole-file view (`gf` in every view): the file at the head, full screen,
//! the lines the diff adds highlighted and the lines it removes shown in
//! place, with the notes under their line. Rendering lives in
//! `ui/fileview.rs`.

use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::model::{FileStatus, Hunk, LineKind};

use super::comments::{CommentTarget, NoteIndex, NoteKind, row_target};
use super::graph::{Source, load_source};
use super::{Row, Scroll};
use crate::app::Shared;

/// A line of code of the file: at the head, or removed from the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeLine {
    pub old: Option<u32>,
    /// Line at the head (1-based); none for a removed line.
    pub new: Option<u32>,
    pub kind: LineKind,
    /// Hunk and line index, for a line the diff shows.
    pub diff: Option<(usize, usize)>,
}

impl CodeLine {
    fn head(old: u32, new: u32) -> Self {
        Self {
            old: Some(old),
            new: Some(new),
            kind: LineKind::Context,
            diff: None,
        }
    }
}

/// A line of the whole-file view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRow {
    Code(CodeLine),
    /// Side by side: base on the left, head on the right.
    Pair {
        left: Option<CodeLine>,
        right: Option<CodeLine>,
    },
    /// Line `part` of note `note` (see [`NoteIndex`]).
    Note {
        note: u32,
        part: u16,
    },
}

impl FileRow {
    /// The line of the diff the row is on or about.
    pub fn diff_line(self, notes: &NoteIndex) -> Option<(usize, usize)> {
        match self {
            FileRow::Code(c) => c.diff,
            FileRow::Pair { left, right } => {
                right.and_then(|c| c.diff).or(left.and_then(|c| c.diff))
            }
            FileRow::Note { note, .. } => notes.items.get(note as usize).and_then(|n| n.at),
        }
    }

    /// Head line of the row, if any.
    fn new_line(self) -> Option<u32> {
        match self {
            FileRow::Code(c) => c.new,
            FileRow::Pair { left, right } => right.and_then(|c| c.new).or(left.and_then(|c| c.new)),
            FileRow::Note { .. } => None,
        }
    }

    /// Part of a change (added or removed line).
    pub fn is_change(self) -> bool {
        let changed = |c: Option<CodeLine>| c.is_some_and(|c| c.kind != LineKind::Context);
        match self {
            FileRow::Code(c) => c.kind != LineKind::Context,
            FileRow::Pair { left, right } => changed(left) || changed(right),
            FileRow::Note { .. } => false,
        }
    }

    /// First line of a note.
    pub fn is_note_head(self) -> bool {
        matches!(self, FileRow::Note { part: 0, .. })
    }

    /// As a row of the diff, to find what `c` comments on.
    fn as_diff_row(self) -> Row {
        match self {
            FileRow::Note { note, part } => Row::Comment {
                hunk: None,
                line: None,
                note,
                part,
            },
            r => match r.diff_line(&NoteIndex::default()) {
                Some((hunk, line)) => Row::Line { hunk, line },
                None => Row::Spacer,
            },
        }
    }
}

/// The lines of a file of `head_len` lines at the head, `hunks` in order,
/// removed lines included when `removed`, each diff line followed by its
/// notes (`file`: the file's index in the diff, for its notes).
pub fn build_rows(
    head_len: u32,
    hunks: &[&Hunk],
    removed: bool,
    notes: &NoteIndex,
    file: Option<usize>,
) -> Vec<FileRow> {
    let mut rows = Vec::with_capacity(head_len as usize + 16);
    if let Some(f) = file {
        push_notes(&mut rows, notes, notes.at_file(f));
    }
    // Next head line to show, and old - new outside the hunks.
    let mut next = 1u32;
    let mut delta = 0i64;
    let old_of = |new: u32, delta: i64| (new as i64 + delta).max(1) as u32;
    for h in hunks {
        let first_new = h.lines.iter().find_map(|l| l.new_line);
        // A removal only: its lines go after the head line `start`.
        let until = first_new.unwrap_or(h.new_range.start + 1);
        while next < until && next <= head_len {
            rows.push(FileRow::Code(CodeLine::head(old_of(next, delta), next)));
            next += 1;
        }
        for (i, l) in h.lines.iter().enumerate() {
            if l.kind == LineKind::Removed && !removed {
                continue;
            }
            rows.push(FileRow::Code(CodeLine {
                old: l.old_line,
                new: l.new_line,
                kind: l.kind,
                diff: Some((h.id, i)),
            }));
            if let Some(n) = l.new_line {
                next = next.max(n + 1);
            }
            push_notes(&mut rows, notes, notes.at_line(h.id, i));
        }
        delta = (h.old_range.start + h.old_range.len) as i64
            - (h.new_range.start + h.new_range.len) as i64;
    }
    while next <= head_len {
        rows.push(FileRow::Code(CodeLine::head(old_of(next, delta), next)));
        next += 1;
    }
    rows
}

fn push_notes(rows: &mut Vec<FileRow>, notes: &NoteIndex, ids: &[u32]) {
    for &note in ids {
        let n = notes.items[note as usize].lines.len();
        rows.extend((0..n).map(|part| FileRow::Note {
            note,
            part: part as u16,
        }));
    }
}

/// Side by side: context lines on both sides, the removed and added lines of
/// each change paired.
pub fn split_rows(rows: &[FileRow]) -> Vec<FileRow> {
    let mut out = Vec::with_capacity(rows.len());
    let mut i = 0;
    while i < rows.len() {
        match rows[i] {
            FileRow::Code(c) if c.kind == LineKind::Context => {
                out.push(FileRow::Pair {
                    left: Some(c),
                    right: Some(c),
                });
                i += 1;
            }
            FileRow::Code(_) => {
                // A change: its removed lines, then its added ones, notes
                // after the pairs.
                let (mut del, mut add, mut notes) = (Vec::new(), Vec::new(), Vec::new());
                while let Some(&r) = rows.get(i) {
                    match r {
                        FileRow::Code(c) if c.kind == LineKind::Removed => del.push(c),
                        FileRow::Code(c) if c.kind == LineKind::Added => add.push(c),
                        FileRow::Note { .. } => notes.push(r),
                        _ => break,
                    }
                    i += 1;
                }
                for k in 0..del.len().max(add.len()) {
                    out.push(FileRow::Pair {
                        left: del.get(k).copied(),
                        right: add.get(k).copied(),
                    });
                }
                out.extend(notes);
            }
            r => {
                out.push(r);
                i += 1;
            }
        }
    }
    out
}

/// What the file view asks the application to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOutcome {
    Continue,
    Close,
    Comment(CommentTarget),
    Thread(NoteKind),
    /// Open the editor at this head line.
    Edit(u32),
}

pub struct FileView {
    pub path: String,
    /// Index of the file in the diff.
    pub file: Option<usize>,
    pub src: Rc<Source>,
    pub rows: Vec<FileRow>,
    pub pos: Scroll,
    pub hscroll: usize,
    pub show_removed: bool,
    pub split: bool,
    /// Other end of a `V` selection.
    pub visual: Option<usize>,
    /// Generation of the notes the rows were built with.
    pub notes_gen: u64,
    pending: Option<char>,
}

impl FileView {
    /// The file `path` at the head, the cursor on head line `line`.
    pub fn open(sh: &mut Shared, path: &str, line: u32) -> Result<Self, String> {
        let file = sh.review.diff.files.iter().position(|f| f.path == path);
        let deleted = file.is_some_and(|f| sh.review.diff.files[f].status == FileStatus::Deleted);
        let src = match load_source(sh, path, false) {
            Some(s) => s,
            // Deleted: nothing at the head, every line shows as removed.
            None if deleted => Source {
                lines: Vec::new(),
                spans: Vec::new(),
                changed: Default::default(),
            },
            None => return Err(format!("cannot read {path}")),
        };
        let mut v = Self {
            path: path.to_string(),
            file,
            src: Rc::new(src),
            rows: Vec::new(),
            pos: Scroll {
                height: 20,
                ..Scroll::default()
            },
            hscroll: 0,
            show_removed: true,
            split: false,
            visual: None,
            notes_gen: 0,
            pending: None,
        };
        v.build(sh);
        let row = v.row_of_line(line);
        v.pos.cursor = row;
        v.pos.scroll = row.saturating_sub(v.pos.height / 3);
        v.pos.clamp(v.rows.len());
        Ok(v)
    }

    pub fn build(&mut self, sh: &Shared) {
        let d = &sh.review.diff;
        let hunks: Vec<&Hunk> = self
            .file
            .map(|f| d.file_hunks(f).collect())
            .unwrap_or_default();
        let rows = build_rows(
            self.src.lines.len() as u32,
            &hunks,
            self.show_removed,
            &sh.notes,
            self.file,
        );
        self.rows = if self.split { split_rows(&rows) } else { rows };
        self.notes_gen = sh.notes.generation;
    }

    /// Rebuilds the rows, keeping the cursor on the same head line.
    pub fn relayout(&mut self, sh: &Shared) {
        let line = self.cursor_line();
        let offset = self.pos.cursor.saturating_sub(self.pos.scroll);
        self.visual = None;
        self.build(sh);
        self.pos.cursor = self.row_of_line(line);
        self.pos.scroll = self.pos.cursor.saturating_sub(offset);
        self.pos.clamp(self.rows.len());
    }

    fn row_of_line(&self, line: u32) -> usize {
        self.rows
            .iter()
            .position(|r| r.new_line().is_some_and(|n| n >= line))
            .unwrap_or(self.rows.len().saturating_sub(1))
    }

    /// Head line at or after the cursor.
    pub fn cursor_line(&self) -> u32 {
        self.rows[self.pos.cursor.min(self.rows.len())..]
            .iter()
            .find_map(|r| r.new_line())
            .or_else(|| self.rows.iter().rev().find_map(|r| r.new_line()))
            .unwrap_or(1)
    }

    /// Changes (blocks of added / removed lines) and threads.
    pub fn counts(&self) -> (usize, usize) {
        let changes = (0..self.rows.len())
            .filter(|&i| self.change_start(i))
            .count();
        let threads = self.rows.iter().filter(|r| r.is_note_head()).count();
        (changes, threads)
    }

    fn change_start(&self, i: usize) -> bool {
        self.rows[i].is_change()
            && (i == 0
                || !self.rows[..i]
                    .iter()
                    .rev()
                    .find(|r| !matches!(r, FileRow::Note { .. }))
                    .is_some_and(|r| r.is_change()))
    }

    pub fn set_view_height(&mut self, h: usize) {
        if h != self.pos.height {
            self.pos.height = h;
            self.pos.clamp(self.rows.len());
        }
    }

    fn move_by(&mut self, delta: isize) {
        self.pos.cursor = self.pos.cursor.saturating_add_signed(delta);
        self.pos.clamp(self.rows.len());
    }

    fn jump(&mut self, forward: bool, pred: impl Fn(&Self, usize) -> bool) -> bool {
        let c = self.pos.cursor;
        let found = if forward {
            (c + 1..self.rows.len()).find(|&i| pred(self, i))
        } else {
            (0..c).rev().find(|&i| pred(self, i))
        };
        match found {
            Some(i) => {
                self.pos.cursor = i;
                self.pos.scroll = i.saturating_sub(self.pos.height / 3);
                self.pos.clamp(self.rows.len());
                true
            }
            None => false,
        }
    }

    /// `c`: comment the diff line (or `V` range) under the cursor, or write
    /// on the note there.
    fn comment(&mut self, sh: &mut Shared) -> FileOutcome {
        let Some(&row) = self.rows.get(self.pos.cursor) else {
            return FileOutcome::Continue;
        };
        let visual = self
            .visual
            .take()
            .and_then(|v| self.rows.get(v))
            .map(|r| r.as_diff_row());
        let target = match row.as_diff_row() {
            Row::Spacer => Err(
                "only the lines of the diff can be commented here (C in the Diff view for the file)",
            ),
            r => row_target(&sh.notes, r, visual),
        };
        match target {
            Ok(t) => FileOutcome::Comment(t),
            Err(e) => {
                sh.notify(e);
                FileOutcome::Continue
            }
        }
    }

    pub fn on_key(&mut self, sh: &mut Shared, key: KeyEvent) -> FileOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.pos.height / 2).max(1) as isize;
        let page = self.pos.height.max(1) as isize;
        if let Some(p) = self.pending.take() {
            match (p, key.code) {
                ('g', KeyCode::Char('g')) => self.pos.goto_top(0, self.rows.len()),
                (']', KeyCode::Char('c')) => {
                    if !self.jump(true, |v, i| v.rows[i].is_note_head()) {
                        sh.notify("no thread below");
                    }
                }
                ('[', KeyCode::Char('c')) => {
                    if !self.jump(false, |v, i| v.rows[i].is_note_head()) {
                        sh.notify("no thread above");
                    }
                }
                ('z', KeyCode::Char('a' | 'o' | 'c')) => self.toggle_note(sh),
                _ => {}
            }
            return FileOutcome::Continue;
        }
        match key.code {
            KeyCode::Esc if self.visual.is_some() => self.visual = None,
            KeyCode::Esc | KeyCode::Char('q') => return FileOutcome::Close,
            KeyCode::Char(c @ ('g' | ']' | '[' | 'z')) if !ctrl => self.pending = Some(c),
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('d') if ctrl => self.move_by(half),
            KeyCode::Char('u') if ctrl => self.move_by(-half),
            KeyCode::Char('f') if ctrl => self.move_by(page),
            KeyCode::Char('b') if ctrl => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::Char('G') | KeyCode::End => {
                self.pos.cursor = self.rows.len().saturating_sub(1);
                self.pos.clamp(self.rows.len());
            }
            KeyCode::Home => self.pos.goto_top(0, self.rows.len()),
            KeyCode::Char('h') | KeyCode::Left => self.hscroll = self.hscroll.saturating_sub(8),
            KeyCode::Char('l') | KeyCode::Right => self.hscroll += 8,
            KeyCode::Char('0') => self.hscroll = 0,
            KeyCode::Char('n') => {
                if !self.jump(true, Self::change_start) {
                    sh.notify("no change below");
                }
            }
            KeyCode::Char('N') => {
                if !self.jump(false, Self::change_start) {
                    sh.notify("no change above");
                }
            }
            KeyCode::Char('d') => {
                self.show_removed = !self.show_removed;
                self.relayout(sh);
            }
            KeyCode::Char('s') => {
                self.split = !self.split;
                self.relayout(sh);
            }
            KeyCode::Char('V') => {
                self.visual = match self.visual {
                    Some(_) => None,
                    None => Some(self.pos.cursor),
                };
            }
            KeyCode::Char('c') => return self.comment(sh),
            KeyCode::Char('o') => self.toggle_note(sh),
            KeyCode::Char('e') => return FileOutcome::Edit(self.cursor_line()),
            KeyCode::Enter => match self.rows.get(self.pos.cursor) {
                Some(&FileRow::Note { note, .. }) => {
                    return FileOutcome::Thread(sh.notes.items[note as usize].kind);
                }
                _ => return FileOutcome::Edit(self.cursor_line()),
            },
            _ => {}
        }
        FileOutcome::Continue
    }

    /// `o` on a note: whole or folded.
    fn toggle_note(&mut self, sh: &mut Shared) {
        let Some(&FileRow::Note { note, .. }) = self.rows.get(self.pos.cursor) else {
            return;
        };
        sh.toggle_note(note);
        let at = self.pos.cursor;
        self.build(sh);
        let head = self
            .rows
            .iter()
            .position(|r| *r == FileRow::Note { note, part: 0 });
        self.pos.cursor = head.unwrap_or(at);
        self.pos.clamp(self.rows.len());
    }
}

#[cfg(test)]
mod tests;
