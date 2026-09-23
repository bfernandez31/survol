//! Diff view state and key handling. Rendering lives in `ui.rs`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::model::{FileStatus, LineKind};
use survol_core::review::Review;
use survol_core::review_state::ReviewState;

use crate::editor;
use crate::highlight::Highlighter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Unified,
    Split,
}

/// One screen line of the continuous diff stream.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideItem {
    Dir(String),
    File(usize),
}

/// Where the cursor is, independent of the row layout.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    file: usize,
    hunk: Option<usize>,
    line: Option<usize>,
}

pub struct App {
    pub review: Review,
    pub state: ReviewState,
    state_path: PathBuf,

    pub rows: Vec<Row>,
    /// File of each row.
    row_file: Vec<usize>,
    /// Header row of each file.
    file_row: Vec<usize>,
    pub collapsed: Vec<bool>,
    pub cursor: usize,
    pub scroll: usize,
    pub hscroll: usize,
    /// Height of the diff pane, updated by the renderer.
    pub view_height: usize,

    pub layout: Layout,
    pub focus: Focus,
    pub sidebar: Vec<SideItem>,
    pub side_sel: usize,
    pub sidebar_visible: bool,
    pub filter: String,
    pub filter_editing: bool,

    pub help: bool,
    pub worktree_ready: bool,
    message: Option<(String, Instant)>,
    pending: Option<char>,
    pub highlighter: Highlighter,
    pub quit: bool,
    /// Set when the terminal must be fully redrawn (after an external editor).
    pub needs_clear: bool,
}

impl App {
    pub fn new(review: Review, state: ReviewState, state_path: PathBuf) -> Self {
        let collapsed = (0..review.diff.files.len())
            .map(|f| review.diff.files[f].is_generated || state.is_file_reviewed(&review.diff, f))
            .collect();
        let worktree_ready = review.worktree == review.repo.dir();
        let mut app = Self {
            review,
            state,
            state_path,
            rows: Vec::new(),
            row_file: Vec::new(),
            file_row: Vec::new(),
            collapsed,
            cursor: 0,
            scroll: 0,
            hscroll: 0,
            view_height: 20,
            layout: Layout::Unified,
            focus: Focus::Diff,
            sidebar: Vec::new(),
            side_sel: 0,
            sidebar_visible: true,
            filter: String::new(),
            filter_editing: false,
            help: false,
            worktree_ready,
            message: None,
            pending: None,
            highlighter: Highlighter::new(),
            quit: false,
            needs_clear: false,
        };
        app.build_rows();
        app.build_sidebar();
        app.sync_sidebar();
        app
    }

    // ----- layout -------------------------------------------------------

    fn build_rows(&mut self) {
        let diff = &self.review.diff;
        let mut rows = Vec::with_capacity(
            diff.hunks.iter().map(|h| h.lines.len() + 1).sum::<usize>() + diff.files.len() * 2,
        );
        let mut row_file = Vec::with_capacity(rows.capacity());
        let mut file_row = Vec::with_capacity(diff.files.len());
        for (f, file) in diff.files.iter().enumerate() {
            let start = rows.len();
            file_row.push(start);
            rows.push(Row::File(f));
            if !self.collapsed[f] {
                if file.hunk_ids.is_empty() {
                    rows.push(Row::Note(f));
                }
                for &h in &file.hunk_ids {
                    rows.push(Row::Hunk(h));
                    let lines = &diff.hunks[h].lines;
                    match self.layout {
                        Layout::Unified => {
                            rows.extend((0..lines.len()).map(|line| Row::Line { hunk: h, line }));
                        }
                        Layout::Split => push_pairs(&mut rows, h, lines),
                    }
                }
            }
            rows.push(Row::Spacer);
            row_file.resize(rows.len(), f);
        }
        self.rows = rows;
        self.row_file = row_file;
        self.file_row = file_row;
    }

    fn anchor(&self) -> Option<Anchor> {
        let file = *self.row_file.get(self.cursor)?;
        let (hunk, line) = match self.rows[self.cursor] {
            Row::Hunk(h) => (Some(h), None),
            Row::Line { hunk, line } => (Some(hunk), Some(line)),
            Row::Pair { hunk, left, right } => (Some(hunk), right.or(left)),
            _ => (None, None),
        };
        Some(Anchor { file, hunk, line })
    }

    /// Rebuilds the rows, keeping the cursor on the same content.
    fn relayout(&mut self) {
        let anchor = self.anchor();
        let offset = self.cursor.saturating_sub(self.scroll);
        self.build_rows();
        if let Some(a) = anchor {
            self.cursor = self.locate(a);
            self.scroll = self.cursor.saturating_sub(offset);
        }
        self.clamp();
    }

    fn locate(&self, a: Anchor) -> usize {
        let start = self.file_row[a.file];
        let end = self
            .file_row
            .get(a.file + 1)
            .copied()
            .unwrap_or(self.rows.len());
        let mut best = start;
        for (i, row) in self.rows[start..end].iter().enumerate() {
            let hit = match *row {
                Row::Hunk(h) => a.hunk == Some(h) && a.line.is_none(),
                Row::Line { hunk, line } => a.hunk == Some(hunk) && a.line == Some(line),
                Row::Pair { hunk, left, right } => {
                    a.hunk == Some(hunk) && a.line.is_some() && (a.line == left || a.line == right)
                }
                _ => false,
            };
            if hit {
                return start + i;
            }
            if matches!(*row, Row::Hunk(h) if a.hunk == Some(h)) {
                best = start + i;
            }
        }
        best
    }

    pub fn build_sidebar(&mut self) {
        let needle = self.filter.to_lowercase();
        let mut items = Vec::new();
        let mut last_dir: Option<&str> = None;
        for (f, file) in self.review.diff.files.iter().enumerate() {
            if !needle.is_empty() && !file.path.to_lowercase().contains(&needle) {
                continue;
            }
            let dir = file.path.rsplit_once('/').map_or("", |(d, _)| d);
            if last_dir != Some(dir) {
                items.push(SideItem::Dir(dir.to_string()));
                last_dir = Some(dir);
            }
            items.push(SideItem::File(f));
        }
        self.sidebar = items;
    }

    /// Selects the current file in the sidebar.
    fn sync_sidebar(&mut self) {
        let f = self.current_file();
        if let Some(i) = self.sidebar.iter().position(|it| *it == SideItem::File(f)) {
            self.side_sel = i;
        }
    }

    // ----- queries ------------------------------------------------------

    pub fn current_file(&self) -> usize {
        self.row_file.get(self.cursor).copied().unwrap_or(0)
    }

    pub fn current_hunk(&self) -> Option<usize> {
        self.anchor().and_then(|a| a.hunk)
    }

    pub fn message(&self) -> Option<&str> {
        self.message
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(4))
            .map(|(m, _)| m.as_str())
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    // ----- movement -----------------------------------------------------

    pub fn set_view_height(&mut self, h: usize) {
        if h != self.view_height {
            self.view_height = h;
            self.clamp();
        }
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        let h = self.view_height.max(1);
        let margin = (h / 4).min(3);
        if self.cursor < self.scroll + margin {
            self.scroll = self.cursor.saturating_sub(margin);
        } else if self.cursor + margin >= self.scroll + h {
            self.scroll = (self.cursor + margin + 1).saturating_sub(h);
        }
        let max_scroll = self.rows.len().saturating_sub(h.min(self.rows.len()));
        self.scroll = self
            .scroll
            .min(max_scroll.max(self.cursor.saturating_sub(h - 1)));
    }

    fn move_by(&mut self, delta: isize) {
        self.cursor = self.cursor.saturating_add_signed(delta);
        self.clamp();
        self.sync_sidebar();
    }

    fn goto(&mut self, row: usize) {
        self.cursor = row;
        self.clamp();
        self.sync_sidebar();
    }

    /// Puts `row` at the top of the pane.
    fn goto_top(&mut self, row: usize) {
        self.cursor = row;
        self.clamp();
        let max_scroll = self.rows.len().saturating_sub(self.view_height.max(1));
        self.scroll = row.min(max_scroll).min(self.cursor);
        self.sync_sidebar();
    }

    fn find(&self, forward: bool, pred: impl Fn(&Row) -> bool) -> Option<usize> {
        if forward {
            (self.cursor + 1..self.rows.len()).find(|&i| pred(&self.rows[i]))
        } else {
            (0..self.cursor).rev().find(|&i| pred(&self.rows[i]))
        }
    }

    fn next_hunk(&mut self, forward: bool) {
        if let Some(r) = self.find(forward, |r| matches!(r, Row::Hunk(_) | Row::File(_))) {
            self.goto_top(r);
        }
    }

    fn next_file(&mut self, forward: bool) {
        let f = self.current_file();
        let target = if forward {
            f + 1
        } else if self.cursor > self.file_row[f] {
            f
        } else {
            f.saturating_sub(1)
        };
        if let Some(&r) = self.file_row.get(target) {
            self.goto_top(r);
        }
    }

    fn is_unreviewed(&self, row: &Row) -> bool {
        let d = &self.review.diff;
        match *row {
            Row::Hunk(h) => !self.state.is_hunk_reviewed(d, h),
            Row::File(f) => d.files[f].hunk_ids.is_empty() && !self.state.is_file_reviewed(d, f),
            _ => false,
        }
    }

    fn next_unreviewed(&mut self) {
        match self.find(true, |r| self.is_unreviewed(r)) {
            Some(r) => self.goto_top(r),
            None => {
                let (done, total) = self.state.progress(&self.review.diff);
                if done == total {
                    self.notify("everything is reviewed");
                } else if let Some(r) =
                    (0..self.cursor).find(|&i| self.is_unreviewed(&self.rows[i]))
                {
                    self.goto_top(r);
                }
            }
        }
    }

    // ----- review actions -----------------------------------------------

    fn save(&mut self) {
        let head = self.review.head_sha.clone();
        if let Err(e) = self.state.save(&self.state_path, &head, &self.review.diff) {
            self.notify(format!("cannot save review state: {e}"));
        }
    }

    fn toggle_hunk(&mut self) {
        let Some(h) = self.current_hunk() else {
            return self.toggle_file();
        };
        let d = &self.review.diff;
        let reviewed = !self.state.is_hunk_reviewed(d, h);
        self.state.set_hunk(d, h, reviewed);
        let f = d.hunks[h].file;
        let file_done = self.state.is_file_reviewed(d, f);
        self.save();
        if reviewed && file_done {
            self.collapse(f, true);
            self.goto(self.file_row[f]);
        }
        if reviewed {
            self.next_unreviewed();
        }
    }

    fn toggle_file(&mut self) {
        let f = self.current_file();
        let d = &self.review.diff;
        let reviewed = !self.state.is_file_reviewed(d, f);
        self.state.set_file(d, f, reviewed);
        self.save();
        self.collapse(f, reviewed);
        self.goto(self.file_row[f]);
        if reviewed && let Some(&r) = self.file_row.get(f + 1) {
            self.goto_top(r);
        }
    }

    fn collapse(&mut self, f: usize, collapsed: bool) {
        if self.collapsed[f] != collapsed {
            self.collapsed[f] = collapsed;
            // Keep the cursor on the file header if its content disappears.
            if collapsed && self.current_file() == f {
                self.cursor = self.file_row[f];
            }
            self.relayout();
        }
    }

    fn toggle_collapse(&mut self) {
        let f = self.current_file();
        self.collapse(f, !self.collapsed[f]);
    }

    fn set_all_collapsed(&mut self, collapsed: bool) {
        self.collapsed.iter_mut().for_each(|c| *c = collapsed);
        let f = self.current_file();
        self.cursor = self.file_row[f];
        self.relayout();
    }

    fn toggle_layout(&mut self) {
        self.layout = match self.layout {
            Layout::Unified => Layout::Split,
            Layout::Split => Layout::Unified,
        };
        self.relayout();
    }

    /// File and line to open for the cursor, on the new side.
    fn editor_target(&self) -> Result<(PathBuf, u32), &'static str> {
        let d = &self.review.diff;
        let f = self.current_file();
        let file = &d.files[f];
        if file.status == FileStatus::Deleted {
            return Err("file deleted in this review");
        }
        let line = match self.anchor().and_then(|a| a.hunk.map(|h| (h, a.line))) {
            Some((h, Some(l))) => {
                let hunk = &d.hunks[h];
                // A removed line has no new number: use the next line that has one.
                hunk.lines[l..]
                    .iter()
                    .find_map(|x| x.new_line)
                    .unwrap_or(hunk.new_range.start)
            }
            Some((h, None)) => d.hunks[h].new_range.start,
            None => 1,
        };
        Ok((self.review.worktree.join(&file.path), line.max(1)))
    }

    fn open_in_editor(&mut self) {
        if !self.worktree_ready {
            return self.notify("worktree is still being checked out…");
        }
        match self.editor_target() {
            Ok((path, line)) => match editor::open(&path, line) {
                Ok(editor::Opened::Parent) => {
                    self.notify(format!("opened in nvim: {}:{line}", path.display()))
                }
                Ok(editor::Opened::Foreground) => self.needs_clear = true,
                Err(e) => self.notify(format!("cannot open editor: {e}")),
            },
            Err(e) => self.notify(e),
        }
    }

    // ----- keys ---------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.help {
            self.help = false;
            return;
        }
        if self.filter_editing {
            return self.on_filter_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.view_height / 2).max(1) as isize;
        let page = self.view_height.max(1) as isize;

        if let Some(p) = self.pending.take() {
            match (p, key.code) {
                ('g', KeyCode::Char('g')) => {
                    self.goto(0);
                    self.side_sel = 0;
                }
                ('z', KeyCode::Char('a' | 'o' | 'c')) => self.toggle_collapse(),
                ('z', KeyCode::Char('M')) => self.set_all_collapsed(true),
                ('z', KeyCode::Char('R')) => self.set_all_collapsed(false),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('g') | KeyCode::Char('z') if !ctrl => {
                if let KeyCode::Char(c) = key.code {
                    self.pending = Some(c);
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Diff,
                    Focus::Diff => {
                        self.sidebar_visible = true;
                        Focus::Sidebar
                    }
                };
            }
            KeyCode::Char('B') => {
                self.sidebar_visible = !self.sidebar_visible;
                if !self.sidebar_visible {
                    self.focus = Focus::Diff;
                }
            }
            KeyCode::Char('/') => {
                self.sidebar_visible = true;
                self.focus = Focus::Sidebar;
                self.filter_editing = true;
            }
            KeyCode::Char('s') => self.toggle_layout(),
            KeyCode::Char('d') if ctrl => self.move_by(half),
            KeyCode::Char('u') if ctrl => self.move_by(-half),
            KeyCode::Char('f') if ctrl => self.move_by(page),
            KeyCode::Char('b') if ctrl => self.move_by(-page),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::Char('G') | KeyCode::End => self.goto(self.rows.len().saturating_sub(1)),
            KeyCode::Home => self.goto(0),
            KeyCode::Char('n') | KeyCode::Char('}') => self.next_hunk(true),
            KeyCode::Char('N') | KeyCode::Char('{') => self.next_hunk(false),
            KeyCode::Char('J') | KeyCode::Char(']') => self.next_file(true),
            KeyCode::Char('K') | KeyCode::Char('[') => self.next_file(false),
            KeyCode::Char('u') => self.next_unreviewed(),
            KeyCode::Char(' ') => self.toggle_hunk(),
            KeyCode::Char('r') | KeyCode::Char('v') => self.toggle_file(),
            KeyCode::Char('o') => self.toggle_collapse(),
            KeyCode::Char('e') => self.open_in_editor(),
            KeyCode::Char('h') | KeyCode::Left if self.focus == Focus::Diff => {
                self.hscroll = self.hscroll.saturating_sub(8);
            }
            KeyCode::Char('l') | KeyCode::Right if self.focus == Focus::Diff => self.hscroll += 8,
            KeyCode::Char('0') => self.hscroll = 0,
            _ => match self.focus {
                Focus::Diff => self.on_diff_key(key),
                Focus::Sidebar => self.on_sidebar_key(key),
            },
        }
    }

    fn on_diff_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Enter => {
                if matches!(
                    self.rows.get(self.cursor),
                    Some(Row::File(_) | Row::Note(_))
                ) {
                    self.toggle_collapse();
                } else {
                    self.open_in_editor();
                }
            }
            KeyCode::Esc if !self.filter.is_empty() => self.set_filter(String::new()),
            _ => {}
        }
    }

    fn on_sidebar_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.side_move(true),
            KeyCode::Char('k') | KeyCode::Up => self.side_move(false),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Diff,
            KeyCode::Esc if !self.filter.is_empty() => self.set_filter(String::new()),
            KeyCode::Esc => self.focus = Focus::Diff,
            _ => {}
        }
    }

    fn side_move(&mut self, forward: bool) {
        let n = self.sidebar.len();
        let mut i = self.side_sel;
        loop {
            i = match (forward, i) {
                (true, i) if i + 1 < n => i + 1,
                (false, i) if i > 0 => i - 1,
                _ => return,
            };
            if let SideItem::File(f) = self.sidebar[i] {
                self.side_sel = i;
                let r = self.file_row[f];
                self.goto_top(r);
                self.side_sel = i;
                return;
            }
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.filter_editing = false,
            KeyCode::Esc => {
                self.filter_editing = false;
                self.set_filter(String::new());
            }
            KeyCode::Backspace => {
                let mut f = self.filter.clone();
                f.pop();
                self.set_filter(f);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let f = format!("{}{c}", self.filter);
                self.set_filter(f);
            }
            _ => {}
        }
    }

    fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.build_sidebar();
        match self.sidebar.iter().find_map(|it| match it {
            SideItem::File(f) => Some(*f),
            SideItem::Dir(_) => None,
        }) {
            Some(f) => self.goto_top(self.file_row[f]),
            None => self.side_sel = 0,
        }
    }
}

/// Pairs removed and added lines of each change block for the split layout.
fn push_pairs(rows: &mut Vec<Row>, hunk: usize, lines: &[survol_core::model::DiffLine]) {
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
