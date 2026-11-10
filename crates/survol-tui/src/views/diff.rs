//! Diff view: every file in one continuous stream, with a file sidebar
//! (see [`super::explorer`]).

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::graph::EdgeKind;

use super::explorer::{self, SideItem};
use super::{Focus, Fold, Row, Scroll, push_hunk_rows};
use crate::app::{Action, Shared};
use crate::views::comments::{CommentTarget, row_target};
use crate::views::graph::symbol_at_position;

/// Where the cursor is, independent of the row layout.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    file: usize,
    hunk: Option<usize>,
    line: Option<usize>,
    /// On a note under the line: the note and its line.
    note: Option<(u32, u16)>,
}

/// Where the cursor of the Diff view stands, to come back to it (`Ctrl-o`).
#[derive(Debug, Clone, Copy)]
pub struct Place {
    anchor: Anchor,
    /// Cursor row minus scroll.
    offset: usize,
    focus: Focus,
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
    /// How the sidebar lists the files (`m`).
    pub explorer: explorer::Mode,
    /// Folded directories of the sidebar, by key.
    folded_dirs: HashSet<String>,
    /// First key of a prefix (`g`, `z`, `]`, `[`) waiting for the second.
    pub pending: Option<char>,
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
            explorer: sh
                .state
                .explorer
                .as_deref()
                .and_then(explorer::Mode::parse)
                .unwrap_or_default(),
            folded_dirs: HashSet::new(),
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
        let row = self.rows[self.pos.cursor];
        let (hunk, line) = match row.hunk_line() {
            Some((h, l)) => (Some(h), l),
            None => (None, None),
        };
        let note = match row {
            Row::Comment { note, part, .. } => Some((note, part)),
            _ => None,
        };
        Some(Anchor {
            file,
            hunk,
            line,
            note,
        })
    }

    /// Where the cursor stands, independent of folds.
    pub fn place(&self) -> Option<Place> {
        Some(Place {
            anchor: self.anchor()?,
            offset: self.pos.cursor.saturating_sub(self.pos.scroll),
            focus: self.focus,
        })
    }

    /// Back to `p`, unfolding its file if needed.
    pub fn restore(&mut self, sh: &Shared, p: Place) {
        if p.anchor.hunk.is_some() {
            self.unfold(sh, p.anchor.file);
        }
        self.visual = None;
        self.focus = p.focus;
        self.pos.cursor = self.locate(p.anchor);
        self.pos.scroll = self.pos.cursor.saturating_sub(p.offset);
        self.clamp();
        self.sync_sidebar();
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
        let row = self.locate_line(a);
        match a.note {
            Some(note) => super::note_row(&self.rows, row, note, |r| *r),
            None => row,
        }
    }

    fn locate_line(&self, a: Anchor) -> usize {
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

    /// The files passing the filter, with their path.
    fn listed<'a>(&self, sh: &'a Shared) -> Vec<(usize, &'a str)> {
        let needle = self.filter.to_lowercase();
        sh.review
            .diff
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| needle.is_empty() || f.path.to_lowercase().contains(&needle))
            .map(|(i, f)| (i, f.path.as_str()))
            .collect()
    }

    pub fn build_sidebar(&mut self, sh: &Shared) {
        let files = self.listed(sh);
        self.sidebar = match self.explorer {
            explorer::Mode::Tree => explorer::tree(&files, &self.folded_dirs),
            explorer::Mode::Pairs => {
                let items = explorer::pairs(&files, |t| graph_subject(sh, t));
                explorer::fold_flat(items, &self.folded_dirs)
            }
            explorer::Mode::Flat => explorer::flat(&files),
        };
        self.side_sel = self.side_sel.min(self.sidebar.len().saturating_sub(1));
    }

    /// The pairs mode can match tests by the graph: list again once built.
    pub fn on_graph_ready(&mut self, sh: &Shared) {
        if self.explorer == explorer::Mode::Pairs {
            self.build_sidebar(sh);
            self.sync_sidebar();
        }
    }

    /// Selects the current file in the sidebar (or the folded directory
    /// holding it).
    fn sync_sidebar(&mut self) {
        let f = self.current_file();
        let mut dir = None;
        for (i, it) in self.sidebar.iter().enumerate() {
            match it {
                SideItem::File { file, .. } if *file == f => {
                    self.side_sel = i;
                    return;
                }
                SideItem::Dir { files, .. } if files.contains(&f) => dir = Some(i),
                _ => {}
            }
        }
        if let Some(i) = dir {
            self.side_sel = i;
        }
    }

    /// `m`: tree → pairs → flat, remembered in the review state.
    fn cycle_explorer(&mut self, sh: &mut Shared) {
        self.explorer = self.explorer.next();
        self.build_sidebar(sh);
        self.sync_sidebar();
        sh.state.explorer = Some(self.explorer.name().to_string());
        sh.save();
        sh.notify(format!("files: {}", self.explorer.name()));
    }

    pub fn is_dir_folded(&self, key: &str) -> bool {
        self.folded_dirs.contains(key)
    }

    fn selected_dir(&self) -> Option<(&str, &[usize])> {
        match self.sidebar.get(self.side_sel)? {
            SideItem::Dir { key, files, .. } => Some((key, files)),
            SideItem::File { .. } => None,
        }
    }

    fn set_dir_folded(&mut self, sh: &Shared, key: String, folded: bool) {
        if folded {
            self.folded_dirs.insert(key.clone());
        } else {
            self.folded_dirs.remove(&key);
        }
        self.build_sidebar(sh);
        if let Some(i) = self
            .sidebar
            .iter()
            .position(|it| matches!(it, SideItem::Dir { key: k, .. } if *k == key))
        {
            self.side_sel = i;
        }
    }

    /// `zM` / `zR` in the sidebar: every directory folded / unfolded.
    fn fold_all_dirs(&mut self, sh: &Shared, folded: bool) {
        self.folded_dirs = if folded {
            match self.explorer {
                explorer::Mode::Tree => explorer::dir_keys(&self.listed(sh)),
                _ => self
                    .sidebar
                    .iter()
                    .filter_map(|it| match it {
                        SideItem::Dir { key, .. } => Some(key.clone()),
                        SideItem::File { .. } => None,
                    })
                    .chain(self.folded_dirs.iter().cloned())
                    .collect(),
            }
        } else {
            HashSet::new()
        };
        self.build_sidebar(sh);
        self.sync_sidebar();
    }

    /// `space` on a directory: all its files reviewed (or none, if they are).
    fn review_dir(&mut self, sh: &mut Shared, files: Vec<usize>) {
        let d = &sh.review.diff;
        let reviewed = !files.iter().all(|&f| sh.state.is_file_reviewed(d, f));
        for &f in &files {
            sh.state.set_file(d, f, reviewed);
            self.collapsed[f] = reviewed;
        }
        sh.save();
        let sel = self.side_sel;
        self.relayout(sh);
        self.side_sel = sel;
        sh.notify(format!(
            "{} file(s) {}",
            files.len(),
            if reviewed { "reviewed" } else { "unreviewed" }
        ));
    }

    /// Shows `hunk` at the top of the pane, unfolding its file.
    pub fn reveal_hunk(&mut self, sh: &Shared, hunk: usize) {
        let file = sh.review.diff.hunks[hunk].file;
        self.unfold(sh, file);
        let row = self.locate(Anchor {
            file,
            hunk: Some(hunk),
            line: None,
            note: None,
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
            note: None,
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

    /// `[u`: the unreviewed hunk before the cursor, going round.
    fn prev_unreviewed(&mut self, sh: &mut Shared) {
        let r = self
            .find(false, |r| Self::is_unreviewed(sh, r))
            .or_else(|| {
                (self.pos.cursor + 1..self.rows.len())
                    .rev()
                    .find(|&i| Self::is_unreviewed(sh, &self.rows[i]))
            });
        match r {
            Some(r) => self.goto_top(r),
            None => sh.notify("everything is reviewed"),
        }
    }

    /// `]t` / `[t`: the next / previous note (draft or discussion).
    fn next_thread(&mut self, sh: &mut Shared, forward: bool) {
        match self.find(forward, |r| matches!(r, Row::Comment { part: 0, .. })) {
            Some(r) => self.goto(r),
            None => sh.notify(if forward {
                "no thread below"
            } else {
                "no thread above"
            }),
        }
    }

    /// `]` / `[` then `c` hunk, `f` file, `t` thread, `u` unreviewed.
    fn bracket(&mut self, sh: &mut Shared, forward: bool, c: char) {
        match c {
            'c' => self.next_hunk(forward),
            'f' => self.next_file(forward),
            't' => self.next_thread(sh, forward),
            'u' if forward => self.next_unreviewed(sh),
            'u' => self.prev_unreviewed(sh),
            _ => {
                let p = if forward { ']' } else { '[' };
                sh.notify(format!(
                    "{p}c hunk · {p}f file · {p}t thread · {p}u unreviewed ({p} alone is a prefix now)"
                ))
            }
        }
    }

    /// `gG`: the Stack group of the hunk under the cursor (or of the file's
    /// first hunk).
    fn show_group(&self, sh: &mut Shared) -> Action {
        let d = &sh.review.diff;
        let hunk = self
            .anchor()
            .and_then(|a| a.hunk)
            .or_else(|| d.files[self.current_file()].hunk_ids.first().copied());
        match hunk {
            Some(h) => Action::ShowGroup(h),
            None => {
                sh.notify("this file has no hunk to find in a group");
                Action::None
            }
        }
    }

    // ----- review actions -----------------------------------------------

    /// `space` (`advance`: then the next unreviewed hunk) and `x`: the hunk
    /// under the cursor reviewed or not; on a file header, the file.
    fn toggle_hunk(&mut self, sh: &mut Shared, advance: bool) {
        let Some(h) = self.anchor().and_then(|a| a.hunk) else {
            return self.toggle_file(sh);
        };
        if !advance {
            let d = &sh.review.diff;
            let reviewed = !sh.state.is_hunk_reviewed(d, h);
            sh.state.set_hunk(d, h, reviewed);
            sh.save();
            return;
        }
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

    /// `o` / `za` / `zo` / `zc`: the note under the cursor whole or folded,
    /// else the file.
    fn fold_here(&mut self, sh: &mut Shared, fold: Fold) {
        match self.rows.get(self.pos.cursor) {
            Some(&Row::Comment { note, .. }) => {
                let folded = !sh.notes.is_expanded(note);
                if fold.folded(folded) != folded {
                    self.toggle_note(sh, note);
                }
            }
            _ => {
                let f = self.current_file();
                self.collapse(sh, f, fold.folded(self.collapsed[f]));
            }
        }
    }

    fn toggle_note(&mut self, sh: &mut Shared, note: u32) {
        sh.toggle_note(note);
        self.relayout(sh);
        // Back on the head of the note.
        let head = |r: &Row| matches!(*r, Row::Comment { note: n, part: 0, .. } if n == note);
        if let Some(r) = self.rows.iter().position(head) {
            self.pos.cursor = r;
            self.clamp();
        }
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
        if super::stray_ctrl(&key, &['d', 'u', 'f', 'b', 'h', 'l']) {
            self.pending = None;
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
                ('g', KeyCode::Char('G')) => return self.show_group(sh),
                ('g', KeyCode::Char('d')) => {
                    sh.notify("already in the Diff view (gs: Graph, gG: Stack group)")
                }
                ('g', KeyCode::Char('f')) => {
                    let (hunk, line) = self.anchor().map_or((None, None), |a| (a.hunk, a.line));
                    return sh.whole_file(self.current_file(), hunk, line);
                }
                ('z', KeyCode::Char(c)) if Fold::of(c).is_some() => {
                    self.fold_here(sh, Fold::of(c).unwrap_or(Fold::Toggle))
                }
                (']' | '[', KeyCode::Char(c)) => self.bracket(sh, p == ']', c),
                ('z', KeyCode::Char('M')) if self.focus == Focus::List => {
                    self.fold_all_dirs(sh, true)
                }
                ('z', KeyCode::Char('R')) if self.focus == Focus::List => {
                    self.fold_all_dirs(sh, false)
                }
                ('z', KeyCode::Char('M')) => self.set_all_collapsed(sh, true),
                ('z', KeyCode::Char('R')) => self.set_all_collapsed(sh, false),
                _ => {}
            }
            return Action::None;
        }

        match key.code {
            KeyCode::Char(c @ ('g' | 'z' | ']' | '[')) if !ctrl => self.pending = Some(c),
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
            KeyCode::Char('n') => self.next_hunk(true),
            KeyCode::Char('N') => self.next_hunk(false),
            KeyCode::Char(c @ ('}' | '{')) => {
                sh.once(
                    "}",
                    "} / { are now ]c / [c (next / previous hunk); they stay aliases until 0.4",
                );
                self.next_hunk(c == '}');
            }
            KeyCode::Char('J') => self.next_file(true),
            KeyCode::Char('K') => self.next_file(false),
            KeyCode::Char('u') => self.next_unreviewed(sh),
            KeyCode::Char('m') => self.cycle_explorer(sh),
            KeyCode::Char(' ' | 'x' | 'X')
                if self.focus == Focus::List && self.selected_dir().is_some() =>
            {
                let files = self
                    .selected_dir()
                    .map(|(_, f)| f.to_vec())
                    .unwrap_or_default();
                self.review_dir(sh, files);
            }
            KeyCode::Char(' ') => self.toggle_hunk(sh, true),
            KeyCode::Char('x') if self.focus == Focus::List => self.toggle_file(sh),
            KeyCode::Char('x') => self.toggle_hunk(sh, false),
            KeyCode::Char('X') => self.toggle_file(sh),
            KeyCode::Char('r') => {
                sh.once(
                    "r",
                    "r is now X (file reviewed); r stays an alias until 0.4",
                );
                self.toggle_file(sh);
            }
            KeyCode::Char('v') => {
                sh.once(
                    "v",
                    "v now selects lines, like V (X marks the file reviewed)",
                );
                return self.on_key(sh, KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));
            }
            KeyCode::Char('o') => self.fold_here(sh, Fold::Toggle),
            KeyCode::Char('e') => self.open_in_editor(sh),
            // The list shows files: `c` and `C` both comment the selected one.
            KeyCode::Char('c' | 'C') if self.focus == Focus::List => {
                match self.sidebar.get(self.side_sel) {
                    Some(SideItem::Dir { .. }) => {
                        sh.notify("select a file to comment on it (not a directory)")
                    }
                    Some(&SideItem::File { file, .. }) => {
                        return Action::Comment(CommentTarget::File(file));
                    }
                    None => return Action::Comment(CommentTarget::File(self.current_file())),
                }
            }
            KeyCode::Char('V') if self.focus == Focus::List => {
                sh.notify("V: select lines in the diff pane (l / Enter / Ctrl-l)")
            }
            KeyCode::Char('C') => return Action::Comment(CommentTarget::File(self.current_file())),
            KeyCode::Char('c') => return self.comment(sh),
            KeyCode::Char('V') => {
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
                Focus::Content => return self.on_content_key(sh, key),
                Focus::List => self.on_sidebar_key(sh, key),
            },
        }
        Action::None
    }

    fn on_content_key(&mut self, sh: &mut Shared, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Enter => match self.rows.get(self.pos.cursor) {
                Some(Row::File(_) | Row::Note(_)) => self.toggle_collapse(sh),
                Some(&Row::Comment { note, .. }) => {
                    return Action::Thread(sh.notes.items[note as usize].kind);
                }
                // Down a level: the whole file (`e` opens the editor).
                _ => {
                    sh.once(
                        "enter-line",
                        "Enter on a line opens the whole file now (e: the editor)",
                    );
                    let (hunk, line) = self.anchor().map_or((None, None), |a| (a.hunk, a.line));
                    return sh.whole_file(self.current_file(), hunk, line);
                }
            },
            KeyCode::Esc if !self.filter.is_empty() => self.set_filter(sh, String::new()),
            _ => {}
        }
        Action::None
    }

    fn on_sidebar_key(&mut self, sh: &mut Shared, key: KeyEvent) {
        let dir = self
            .selected_dir()
            .map(|(k, _)| (k.to_string(), self.folded_dirs.contains(k)));
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.side_move(true),
            KeyCode::Char('k') | KeyCode::Up => self.side_move(false),
            KeyCode::Char('h') | KeyCode::Left => match dir {
                Some((key, false)) => self.set_dir_folded(sh, key, true),
                _ => self.side_parent(),
            },
            KeyCode::Char('l') | KeyCode::Right => match dir {
                Some((key, true)) => self.set_dir_folded(sh, key, false),
                Some((_, false)) => self.side_move(true),
                None => self.focus = Focus::Content,
            },
            KeyCode::Enter => match dir {
                Some((key, folded)) => self.set_dir_folded(sh, key, !folded),
                None => self.focus = Focus::Content,
            },
            KeyCode::Esc if !self.filter.is_empty() => self.set_filter(sh, String::new()),
            KeyCode::Esc => {
                sh.once(
                    "esc-list",
                    "Esc stays in the list now: l, Enter or Ctrl-l go to the diff",
                );
            }
            _ => {}
        }
    }

    /// Selects sidebar item `i`, showing its file (a directory's first).
    fn side_select(&mut self, i: usize) {
        let file = match &self.sidebar[i] {
            SideItem::File { file, .. } => Some(*file),
            SideItem::Dir { files, .. } => files.first().copied(),
        };
        if let Some(f) = file {
            self.goto_top(self.file_row[f]);
        }
        self.side_sel = i;
    }

    fn side_move(&mut self, forward: bool) {
        let n = self.sidebar.len();
        let i = match (forward, self.side_sel) {
            (true, i) if i + 1 < n => i + 1,
            (false, i) if i > 0 => i - 1,
            _ => return,
        };
        self.side_select(i);
    }

    /// `h` on a file or a folded directory: its parent directory.
    fn side_parent(&mut self) {
        let Some(depth) = self.sidebar.get(self.side_sel).map(SideItem::depth) else {
            return;
        };
        if let Some(i) = self.sidebar[..self.side_sel]
            .iter()
            .rposition(|it| matches!(it, SideItem::Dir { .. }) && it.depth() < depth)
        {
            self.side_select(i);
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
            SideItem::File { file, .. } => Some(*file),
            SideItem::Dir { .. } => None,
        }) {
            Some(f) => self.goto_top(self.file_row[f]),
            None => self.side_sel = 0,
        }
    }
}

/// The changed file a test file exercises most, by the test and call edges
/// of the code graph.
fn graph_subject(sh: &Shared, test: usize) -> Option<usize> {
    let g = sh.graph.as_ref()?;
    let d = &sh.review.diff;
    let mut hits: HashMap<usize, usize> = HashMap::new();
    for &s in g.symbols_in_file(&d.files[test].path) {
        for e in g.edges_from(s) {
            if !matches!(e.kind, EdgeKind::Tests | EdgeKind::Calls) {
                continue;
            }
            let path = &g.symbol(e.to).file;
            if let Some(f) = d.files.iter().position(|x| &x.path == path)
                && f != test
            {
                *hits.entry(f).or_default() += 1;
            }
        }
    }
    hits.into_iter()
        .max_by_key(|&(f, n)| (n, std::cmp::Reverse(f)))
        .map(|(f, _)| f)
}
