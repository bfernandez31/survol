//! State and key handling of each view. Rendering lives in `ui/`.

pub mod ask;
pub mod comments;
pub mod diff;
pub mod explorer;
pub mod export;
pub mod fileview;
pub mod flows;
pub mod graph;
pub mod stack;

use survol_core::model::{DiffLine, LineKind};

use self::comments::NoteIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Unified,
    Split,
}

/// Which pane of a two-pane view has the keyboard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Focus {
    /// Files (Diff view) or groups tree (Stack view).
    #[default]
    List,
    Content,
}

/// One screen line of diff content, shared by the Diff and Stack views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    File(usize),
    /// Under a file without hunks (binary, pure rename, mode change).
    Note(usize),
    Hunk(usize),
    Line {
        hunk: usize,
        line: usize,
    },
    /// Split layout: old line on the left, new line on the right.
    Pair {
        hunk: usize,
        left: Option<usize>,
        right: Option<usize>,
    },
    Spacer,
    /// Line `part` of a note (draft or discussion) shown under the line it
    /// is about (`line` of `hunk`), or under a file header.
    Comment {
        hunk: Option<usize>,
        line: Option<usize>,
        /// Index into [`NoteIndex::items`].
        note: u32,
        part: u16,
    },
}

impl Row {
    /// Hunk and line index the row shows (for a note, the line it is about).
    pub fn hunk_line(self) -> Option<(usize, Option<usize>)> {
        match self {
            Row::Hunk(h) => Some((h, None)),
            Row::Line { hunk, line } => Some((hunk, Some(line))),
            Row::Pair { hunk, left, right } => Some((hunk, right.or(left))),
            Row::Comment {
                hunk: Some(h),
                line,
                ..
            } => Some((h, line)),
            _ => None,
        }
    }
}

/// Header row then lines of `hunk`, in `layout`, each line followed by its
/// notes.
pub fn push_hunk_rows(
    rows: &mut Vec<Row>,
    hunk: usize,
    lines: &[DiffLine],
    layout: Layout,
    notes: &NoteIndex,
) {
    rows.push(Row::Hunk(hunk));
    match layout {
        Layout::Unified => {
            for line in 0..lines.len() {
                rows.push(Row::Line { hunk, line });
                notes.push_rows(rows, notes.at_line(hunk, line), Some(hunk), Some(line));
            }
        }
        Layout::Split => push_pairs(rows, hunk, lines, notes),
    }
}

/// Pairs removed and added lines of each change block for the split layout.
fn push_pairs(rows: &mut Vec<Row>, hunk: usize, lines: &[DiffLine], notes: &NoteIndex) {
    let pair = |rows: &mut Vec<Row>, left: Option<usize>, right: Option<usize>| {
        rows.push(Row::Pair { hunk, left, right });
        if let Some(l) = left {
            notes.push_rows(rows, notes.at_line(hunk, l), Some(hunk), Some(l));
        }
        if let Some(r) = right
            && right != left
        {
            notes.push_rows(rows, notes.at_line(hunk, r), Some(hunk), Some(r));
        }
    };
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == LineKind::Context {
            pair(rows, Some(i), Some(i));
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].kind == LineKind::Removed {
            i += 1;
        }
        let removed: Vec<usize> = (start..i).collect();
        let start = i;
        while i < lines.len() && lines[i].kind == LineKind::Added {
            i += 1;
        }
        let added: Vec<usize> = (start..i).collect();
        for k in 0..removed.len().max(added.len()) {
            pair(rows, removed.get(k).copied(), added.get(k).copied());
        }
    }
}

/// Row of line `part` of note `note` among the note rows following `from`
/// (the line or file header the note is under); `from` if it is gone.
pub fn note_row<T>(
    rows: &[T],
    from: usize,
    (note, part): (u32, u16),
    row: impl Fn(&T) -> Row,
) -> usize {
    let mut best = from;
    for (i, r) in rows.iter().enumerate().skip(from + 1) {
        match row(r) {
            Row::Comment {
                note: n, part: p, ..
            } if n == note => {
                best = i;
                if p >= part {
                    break;
                }
            }
            Row::Comment { .. } => {}
            _ => break,
        }
    }
    best
}

/// Cursor and scroll of a scrollable pane, keeping a small margin.
#[derive(Debug, Clone, Copy, Default)]
pub struct Scroll {
    pub cursor: usize,
    pub scroll: usize,
    /// Height of the pane, updated by the renderer.
    pub height: usize,
}

impl Scroll {
    pub fn clamp(&mut self, len: usize) {
        self.cursor = self.cursor.min(len.saturating_sub(1));
        let h = self.height.max(1);
        let margin = (h / 4).min(3);
        if self.cursor < self.scroll + margin {
            self.scroll = self.cursor.saturating_sub(margin);
        } else if self.cursor + margin >= self.scroll + h {
            self.scroll = (self.cursor + margin + 1).saturating_sub(h);
        }
        let max_scroll = len.saturating_sub(h.min(len));
        self.scroll = self
            .scroll
            .min(max_scroll.max(self.cursor.saturating_sub(h - 1)));
    }

    /// Puts `row` at the top of the pane.
    pub fn goto_top(&mut self, row: usize, len: usize) {
        self.cursor = row;
        self.clamp(len);
        let max_scroll = len.saturating_sub(self.height.max(1));
        self.scroll = row.min(max_scroll).min(self.cursor);
    }
}
