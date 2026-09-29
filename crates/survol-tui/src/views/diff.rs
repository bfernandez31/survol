//! Diff view: every file in one continuous stream, with a file sidebar.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Focus, Row, Scroll, push_hunk_rows};
use crate::app::{Action, Shared};
use crate::views::comments::{CommentTarget, row_target};
use crate::views::graph::symbol_at_position;

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

pub struct DiffView {
    pub rows: Vec<Row>,
    /// File of each row.
    row_file: Vec<usize>,
    /// Header row of each file.
    file_row: Vec<usize>,
    pub collapsed: Vec<bool>,
    pub pos: Scroll,
    pub hscroll: usize,

    pub focus: Focus,
    pub sidebar: Vec<SideItem>,
    pub side_sel: usize,
    pub sidebar_visible: bool,
    pub filter: String,
    pub filter_editing: bool,
    pending: Option<char>,
    /// Other end of a `V` selection (row index).
    pub visual: Option<usize>,
    /// [`NoteIndex::generation`](crate::views::comments::NoteIndex) the rows were built with.
    pub notes_gen: u64,
}

impl DiffView {
    pub fn new(sh: &Shared) -> Self {
        let (review, state) = (&sh.review, &sh.state);
        let collapsed = (0..review.diff.files.len())
            .map(|f| review.diff.files[f].is_generated || state.is_file_reviewed(&review.diff, f))
            .collect();
        let mut v = Self {
            rows: Vec::new(),
            row_file: Vec::new(),
            file_row: Vec::new(),
            collapsed,
            pos: Scroll {
                height: 20,
                ..Scroll::default()
            },
            hscroll: 0,
            focus: Focus::Content,
            sidebar: Vec::new(),
            side_sel: 0,
            sidebar_visible: true,
            filter: String::new(),
            filter_editing: false,
            pending: None,
            visual: None,
            notes_gen: 0,
        };
        v.build_rows(sh);
        v.build_sidebar(sh);
        v.sync_sidebar();
        v
    }

    /// Keys this view handles before the global ones (filter input, `g`/`z` prefixes).
    pub fn captures_keys(&self) -> bool {
        self.filter_editing || self.pending.is_some()
    }

    // ----- layout -------------------------------------------------------

    fn build_rows(&mut self, sh: &Shared) {
        let diff = &sh.review.diff;
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
                sh.notes
                    .push_rows(&mut rows, sh.notes.at_file(f), None, None);
                if file.hunk_ids.is_empty() {
                    rows.push(Row::Note(f));
                }
                for &h in &file.hunk_ids {
                    push_hunk_rows(&mut rows, h, &diff.hunks[h].lines, sh.layout, &sh.notes);
                }
            }
            rows.push(Row::Spacer);
            row_file.resize(rows.len(), f);
        }
        self.rows = rows;
        self.row_file = row_file;
        self.file_row = file_row;
        self.notes_gen = sh.notes.generation;
    }

    fn anchor(&self) -> Option<Anchor> {
        let file = *self.row_file.get(self.pos.cursor)?;
        let (hunk, line) = match self.rows[self.pos.cursor].hunk_line() {
            Some((h, l)) => (Some(h), l),
            None => (None, None),
        };
        Some(Anchor { file, hunk, line })
    }

    /// Rebuilds the rows, keeping the cursor on the same content.
    pub fn relayout(&mut self, sh: &Shared) {
        self.visual = None;
        let anchor = self.anchor();
        let offset = self.pos.cursor.saturating_sub(self.pos.scroll);
        self.build_rows(sh);
        if let Some(a) = anchor {
            self.pos.cursor = self.locate(a);
            self.pos.scroll = self.pos.cursor.saturating_sub(offset);
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

    pub fn build_sidebar(&mut self, sh: &Shared) {
        let needle = self.filter.to_lowercase();
        let mut items = Vec::new();
        let mut last_dir: Option<&str> = None;
        for (f, file) in sh.review.diff.files.iter().enumerate() {
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

    /// Shows `hunk` at the top of the pane, unfolding its file.
    pub fn reveal_hunk(&mut self, sh: &Shared, hunk: usize) {
        let file = sh.review.diff.hunks[hunk].file;
        self.unfold(sh, file);
        let row = self.locate(Anchor {
            file,
            hunk: Some(hunk),
            line: None,
        });
        self.focus = Focus::Content;
        self.goto_top(row);
    }

    /// Puts the cursor on line `line` of `hunk`, unfolding its file.
    pub fn reveal_line(&mut self, sh: &Shared, hunk: usize, line: usize) {
        let file = sh.review.diff.hunks[hunk].file;
        self.unfold(sh, file);
        let row = self.locate(Anchor {
            file,
            hunk: Some(hunk),
            line: Some(line),
        });
        self.focus = Focus::Content;
        self.pos.cursor = row;
        self.pos.scroll = row.saturating_sub(self.pos.height / 3);
        self.clamp();
        self.sync_sidebar();
    }

    pub fn reveal_file(&mut self, sh: &Shared, file: usize) {
        self.unfold(sh, file);
        self.focus = Focus::Content;
        self.goto_top(self.file_row[file]);
    }

    fn unfold(&mut self, sh: &Shared, file: usize) {
        if self.collapsed[file] {
            self.collapsed[file] = false;
            self.relayout(sh);
        }
    }

    // ----- queries ------------------------------------------------------

    pub fn current_file(&self) -> usize {
        self.row_file.get(self.pos.cursor).copied().unwrap_or(0)
    }

    // ----- movement -----------------------------------------------------

    pub fn set_view_height(&mut self, h: usize) {
        if h != self.pos.height {
            self.pos.height = h;
            self.clamp();
        }
    }

    fn clamp(&mut self) {
        self.pos.clamp(self.rows.len());
    }

    fn move_by(&mut self, delta: isize) {
        self.pos.cursor = self.pos.cursor.saturating_add_signed(delta);
        self.clamp();
        self.sync_sidebar();
    }

    fn goto(&mut self, row: usize) {
        self.pos.cursor = row;
        self.clamp();
        self.sync_sidebar();
    }

    /// Puts `row` at the top of the pane.
    fn goto_top(&mut self, row: usize) {
        self.pos.goto_top(row, self.rows.len());
        self.sync_sidebar();
    }

    fn find(&self, forward: bool, pred: impl Fn(&Row) -> bool) -> Option<usize> {
        let cursor = self.pos.cursor;
        if forward {
            (cursor + 1..self.rows.len()).find(|&i| pred(&self.rows[i]))
        } else {
            (0..cursor).rev().find(|&i| pred(&self.rows[i]))
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
        } else if self.pos.cursor > self.file_row[f] {
            f
        } else {
            f.saturating_sub(1)
        };
        if let Some(&r) = self.file_row.get(target) {
            self.goto_top(r);
        }
    }

    fn is_unreviewed(sh: &Shared, row: &Row) -> bool {
        let d = &sh.review.diff;
        match *row {
            Row::Hunk(h) => !sh.state.is_hunk_reviewed(d, h),
            Row::File(f) => d.files[f].hunk_ids.is_empty() && !sh.state.is_file_reviewed(d, f),
            _ => false,
        }
    }

    fn next_unreviewed(&mut self, sh: &mut Shared) {
        match self.find(true, |r| Self::is_unreviewed(sh, r)) {
            Some(r) => self.goto_top(r),
            None => {
                let (done, total) = sh.state.progress(&sh.review.diff);
                if done == total {
                    sh.notify("everything is reviewed");
                } else if let Some(r) =
                    (0..self.pos.cursor).find(|&i| Self::is_unreviewed(sh, &self.rows[i]))
                {
                    self.goto_top(r);
                }
            }
        }
    }

    // ----- review actions -----------------------------------------------

    fn toggle_hunk(&mut self, sh: &mut Shared) {
        let Some(h) = self.anchor().and_then(|a| a.hunk) else {
            return self.toggle_file(sh);
        };
        let d = &sh.review.diff;
        let reviewed = !sh.state.is_hunk_reviewed(d, h);
        sh.state.set_hunk(d, h, reviewed);
        let f = d.hunks[h].file;
        let file_done = sh.state.is_file_reviewed(d, f);
        sh.save();
        if reviewed && file_done {
            self.collapse(sh, f, true);
            self.goto(self.file_row[f]);
        }
        if reviewed {
            self.next_unreviewed(sh);
        }
    }

    fn toggle_file(&mut self, sh: &mut Shared) {
        let f = self.current_file();
        let d = &sh.review.diff;
        let reviewed = !sh.state.is_file_reviewed(d, f);
        sh.state.set_file(d, f, reviewed);
        sh.save();
        self.collapse(sh, f, reviewed);
        self.goto(self.file_row[f]);
        if reviewed && let Some(&r) = self.file_row.get(f + 1) {
            self.goto_top(r);
        }
    }

    fn collapse(&mut self, sh: &Shared, f: usize, collapsed: bool) {
        if self.collapsed[f] != collapsed {
            self.collapsed[f] = collapsed;
            // Keep the cursor on the file header if its content disappears.
            if collapsed && self.current_file() == f {
                self.pos.cursor = self.file_row[f];
            }
            self.relayout(sh);
        }
    }

    fn toggle_collapse(&mut self, sh: &Shared) {
        let f = self.current_file();
        self.collapse(sh, f, !self.collapsed[f]);
    }

    fn set_all_collapsed(&mut self, sh: &Shared, collapsed: bool) {
        self.collapsed.iter_mut().for_each(|c| *c = collapsed);
        let f = self.current_file();
        self.pos.cursor = self.file_row[f];
        self.relayout(sh);
    }

    fn open_in_editor(&self, sh: &mut Shared) {
        let (hunk, line) = self
            .anchor()
            .and_then(|a| a.hunk.map(|h| (Some(h), a.line)))
            .unwrap_or((None, None));
        sh.open_in_editor(self.current_file(), hunk, line);
    }

    /// `a`: the hunk under the cursor, for a question to the LLM.
    pub fn ask_subject(&self, sh: &mut Shared) -> Option<(survol_core::ask::Subject, String)> {
        let Some(h) = self.anchor().and_then(|a| a.hunk) else {
            sh.notify("move to a hunk to ask about it (or use the Graph / Stack views)");
            return None;
        };
        let d = &sh.review.diff;
        let hunk = &d.hunks[h];
        Some((
            survol_core::ask::Subject::Hunk(hunk.content_hash.clone()),
            format!("hunk {}:{}", d.files[hunk.file].path, hunk.new_range.start),
        ))
    }

    /// `c`: comment on the line (or the `V` range, the file header, the
    /// draft) under the cursor.
    fn comment(&mut self, sh: &mut Shared) -> Action {
        let Some(&row) = self.rows.get(self.pos.cursor) else {
            return Action::None;
        };
        let visual = self.visual.take().and_then(|v| self.rows.get(v).copied());
        match row_target(&sh.notes, row, visual) {
            Ok(t) => Action::Comment(t),
            Err(e) => {
                sh.notify(e);
                Action::None
            }
        }
    }

    /// `gs`: the Graph view of the symbol under the cursor.
    fn show_symbol(&self, sh: &mut Shared) -> Action {
        let Some(a) = self.anchor() else {
            return Action::None;
        };
        let Some(g) = &sh.graph else {
            sh.notify("the code graph is still being built…");
            return Action::None;
        };
        match symbol_at_position(g, &sh.review.diff, a.file, a.hunk, a.line) {
            Some(s) => Action::ShowSymbol(s),
            None => {
                sh.notify("no symbol here (language not indexed?)");
                Action::None
            }
        }
    }

    // ----- keys ---------------------------------------------------------

    pub fn on_key(&mut self, sh: &mut Shared, key: KeyEvent) -> Action {
        if self.filter_editing {
            self.on_filter_key(sh, key);
            return Action::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.pos.height / 2).max(1) as isize;
        let page = self.pos.height.max(1) as isize;

        if let Some(p) = self.pending.take() {
            match (p, key.code) {
                ('g', KeyCode::Char('g')) => {
                    self.goto(0);
                    self.side_sel = 0;
                }
                ('g', KeyCode::Char('s')) => return self.show_symbol(sh),
                ('z', KeyCode::Char('a' | 'o' | 'c')) => self.toggle_collapse(sh),
                ('z', KeyCode::Char('M')) => self.set_all_collapsed(sh, true),
                ('z', KeyCode::Char('R')) => self.set_all_collapsed(sh, false),
                _ => {}
            }
            return Action::None;
        }

        match key.code {
            KeyCode::Char('g') | KeyCode::Char('z') if !ctrl => {
                if let KeyCode::Char(c) = key.code {
                    self.pending = Some(c);
                }
            }
            KeyCode::Char('h') if ctrl => {
                self.sidebar_visible = true;
                self.focus = Focus::List;
            }
            KeyCode::Char('l') if ctrl => self.focus = Focus::Content,
            KeyCode::Char('B') => {
                self.sidebar_visible = !self.sidebar_visible;
                if !self.sidebar_visible {
                    self.focus = Focus::Content;
                }
            }
            KeyCode::Char('/') => {
                self.sidebar_visible = true;
                self.focus = Focus::List;
                self.filter_editing = true;
            }
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
            KeyCode::Char('u') => self.next_unreviewed(sh),
            KeyCode::Char(' ') => self.toggle_hunk(sh),
            KeyCode::Char('r') | KeyCode::Char('v') => self.toggle_file(sh),
            KeyCode::Char('o') => self.toggle_collapse(sh),
            KeyCode::Char('e') => self.open_in_editor(sh),
            KeyCode::Char('C') => return Action::Comment(CommentTarget::File(self.current_file())),
            KeyCode::Char('c') if self.focus == Focus::Content => return self.comment(sh),
            KeyCode::Char('V') if self.focus == Focus::Content => {
                self.visual = match self.visual {
                    Some(_) => None,
                    None => Some(self.pos.cursor),
                };
            }
            KeyCode::Esc if self.visual.is_some() => self.visual = None,
            KeyCode::Char('h') | KeyCode::Left if self.focus == Focus::Content => {
                self.hscroll = self.hscroll.saturating_sub(8);
            }
            KeyCode::Char('l') | KeyCode::Right if self.focus == Focus::Content => {
                self.hscroll += 8
            }
            KeyCode::Char('0') => self.hscroll = 0,
            _ => match self.focus {
                Focus::Content => self.on_content_key(sh, key),
                Focus::List => self.on_sidebar_key(sh, key),
            },
        }
        Action::None
    }

    fn on_content_key(&mut self, sh: &mut Shared, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Enter => {
                if matches!(
                    self.rows.get(self.pos.cursor),
                    Some(Row::File(_) | Row::Note(_))
                ) {
                    self.toggle_collapse(sh);
                } else {
                    self.open_in_editor(sh);
                }
            }
            KeyCode::Esc if !self.filter.is_empty() => self.set_filter(sh, String::new()),
            _ => {}
        }
    }

    fn on_sidebar_key(&mut self, sh: &Shared, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.side_move(true),
            KeyCode::Char('k') | KeyCode::Up => self.side_move(false),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Content,
            KeyCode::Esc if !self.filter.is_empty() => self.set_filter(sh, String::new()),
            KeyCode::Esc => self.focus = Focus::Content,
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

    fn on_filter_key(&mut self, sh: &Shared, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.filter_editing = false,
            KeyCode::Esc => {
                self.filter_editing = false;
                self.set_filter(sh, String::new());
            }
            KeyCode::Backspace => {
                let mut f = self.filter.clone();
                f.pop();
                self.set_filter(sh, f);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let f = format!("{}{c}", self.filter);
                self.set_filter(sh, f);
            }
            _ => {}
        }
    }

    fn set_filter(&mut self, sh: &Shared, filter: String) {
        self.filter = filter;
        self.build_sidebar(sh);
        match self.sidebar.iter().find_map(|it| match it {
            SideItem::File(f) => Some(*f),
            SideItem::Dir(_) => None,
        }) {
            Some(f) => self.goto_top(self.file_row[f]),
            None => self.side_sel = 0,
        }
    }
}
