//! State and key handling of each view. Rendering lives in `ui/`.

pub mod diff;
pub mod graph;
pub mod stack;

use survol_core::model::{DiffLine, LineKind};

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
}

impl Row {
    /// Hunk and line index the row shows, if any.
    pub fn hunk_line(self) -> Option<(usize, Option<usize>)> {
        match self {
            Row::Hunk(h) => Some((h, None)),
            Row::Line { hunk, line } => Some((hunk, Some(line))),
            Row::Pair { hunk, left, right } => Some((hunk, right.or(left))),
            _ => None,
        }
    }
}

/// Header row then lines of `hunk`, in `layout`.
pub fn push_hunk_rows(rows: &mut Vec<Row>, hunk: usize, lines: &[DiffLine], layout: Layout) {
    rows.push(Row::Hunk(hunk));
    match layout {
        Layout::Unified => rows.extend((0..lines.len()).map(|line| Row::Line { hunk, line })),
        Layout::Split => push_pairs(rows, hunk, lines),
    }
}

/// Pairs removed and added lines of each change block for the split layout.
fn push_pairs(rows: &mut Vec<Row>, hunk: usize, lines: &[DiffLine]) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == LineKind::Context {
            rows.push(Row::Pair {
                hunk,
                left: Some(i),
                right: Some(i),
            });
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
            rows.push(Row::Pair {
                hunk,
                left: removed.get(k).copied(),
                right: added.get(k).copied(),
            });
        }
    }
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
