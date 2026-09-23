//! Stack view: hunks grouped by functional capability, then by layer, in
//! reading order. Left: the groups as a foldable tree. Right: the summary of
//! the selected group and the diff of the selected node.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::graph::Graph;
use survol_core::group::Grouping;
use survol_core::model::Diff;
use survol_core::review_state::ReviewState;

use super::{Focus, Row, Scroll, push_hunk_rows};
use crate::app::{Action, Shared};
use crate::views::graph::symbol_at_position;

/// A line of the groups tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    Group(usize),
    Layer {
        group: usize,
        layer: usize,
    },
    Hunk {
        group: usize,
        layer: usize,
        hunk: usize,
    },
    /// A file without hunks (mechanical group only).
    File {
        group: usize,
        file: usize,
    },
}

impl Node {
    pub fn group(self) -> usize {
        match self {
            Node::Group(g) => g,
            Node::Layer { group, .. } | Node::Hunk { group, .. } | Node::File { group, .. } => {
                group
            }
        }
    }

    pub fn depth(self) -> usize {
        match self {
            Node::Group(_) => 0,
            Node::Layer { .. } | Node::File { .. } => 1,
            Node::Hunk { .. } => 2,
        }
    }

    fn parent(self) -> Option<Node> {
        match self {
            Node::Group(_) => None,
            Node::Layer { group, .. } | Node::File { group, .. } => Some(Node::Group(group)),
            Node::Hunk { group, layer, .. } => Some(Node::Layer { group, layer }),
        }
    }
}

/// A line of the content pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackRow {
    Layer { group: usize, layer: usize },
    File(usize),
    Diff(Row),
}

/// Visible nodes of the tree, depth first.
pub fn flatten(g: &Grouping, folded: &[bool], open_layers: &HashSet<(usize, usize)>) -> Vec<Node> {
    let mut out = Vec::new();
    for (gi, group) in g.groups.iter().enumerate() {
        out.push(Node::Group(gi));
        if folded.get(gi).copied().unwrap_or(false) {
            continue;
        }
        for (li, layer) in group.layers.iter().enumerate() {
            out.push(Node::Layer {
                group: gi,
                layer: li,
            });
            if open_layers.contains(&(gi, li)) {
                out.extend(layer.hunk_ids.iter().map(|&h| Node::Hunk {
                    group: gi,
                    layer: li,
                    hunk: h,
                }));
            }
        }
        out.extend(
            group
                .file_ids
                .iter()
                .map(|&f| Node::File { group: gi, file: f }),
        );
    }
    out
}

/// Hunks and hunkless files under `node`.
pub fn node_items(g: &Grouping, node: Node) -> (Vec<usize>, Vec<usize>) {
    match node {
        Node::Group(gi) => (g.groups[gi].hunk_ids.clone(), g.groups[gi].file_ids.clone()),
        Node::Layer { group, layer } => {
            (g.groups[group].layers[layer].hunk_ids.clone(), Vec::new())
        }
        Node::Hunk { hunk, .. } => (vec![hunk], Vec::new()),
        Node::File { file, .. } => (Vec::new(), vec![file]),
    }
}

/// Reviewed items under `node`, and total.
pub fn node_progress(g: &Grouping, node: Node, diff: &Diff, state: &ReviewState) -> (usize, usize) {
    if let Node::Group(gi) = node {
        return g.groups[gi].progress(diff, state);
    }
    let (hunks, files) = node_items(g, node);
    let done = hunks
        .iter()
        .filter(|&&h| state.is_hunk_reviewed(diff, h))
        .count()
        + files
            .iter()
            .filter(|&&f| state.is_file_reviewed(diff, f))
            .count();
    (done, hunks.len() + files.len())
}

#[derive(Default)]
pub struct StackView {
    pub grouping: Option<Grouping>,
    pub tree: Vec<Node>,
    /// Selected index in `tree`.
    pub sel: usize,
    /// Per group.
    folded: Vec<bool>,
    open_layers: HashSet<(usize, usize)>,

    pub rows: Vec<StackRow>,
    pub pos: Scroll,
    pub hscroll: usize,
    pub focus: Focus,
    pub list_hidden: bool,
    /// Waiting for y/n before regrouping with the LLM.
    pub confirm_regroup: bool,
    pub show_warnings: bool,
    pending: Option<char>,
}

impl StackView {
    /// Keys this view handles before the global ones (prompts, popups, prefixes).
    pub fn captures_keys(&self) -> bool {
        self.confirm_regroup || self.show_warnings || self.pending.is_some()
    }

    /// Displays a new grouping: mechanical and reviewed groups folded, the
    /// first unreviewed group selected.
    pub fn set_grouping(&mut self, sh: &Shared, g: Grouping) {
        let d = &sh.review.diff;
        self.folded = g
            .groups
            .iter()
            .map(|gr| gr.mechanical || gr.is_reviewed(d, &sh.state))
            .collect();
        self.open_layers.clear();
        let first = g
            .groups
            .iter()
            .position(|gr| !gr.is_reviewed(d, &sh.state))
            .unwrap_or(0);
        self.grouping = Some(g);
        self.tree = self.flatten();
        self.select_node(sh, Node::Group(first));
    }

    /// Reorders the groups along the code graph, keeping folds, the
    /// selected node and the content scroll.
    pub fn reorder_with_graph(&mut self, sh: &Shared, graph: &Graph) {
        let Some(g) = &mut self.grouping else {
            return;
        };
        let before: Vec<usize> = g.groups.iter().map(|gr| gr.id).collect();
        g.order_with_graph(graph);
        // Old index → new index, by group id (in order, should ids repeat).
        let mut slots: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, gr) in g.groups.iter().enumerate().rev() {
            slots.entry(gr.id).or_default().push(i);
        }
        let to_new: Vec<usize> = before
            .iter()
            .enumerate()
            .map(|(old, id)| slots.get_mut(id).and_then(Vec::pop).unwrap_or(old))
            .collect();
        let mut folded = vec![false; self.folded.len()];
        for (old, &f) in self.folded.iter().enumerate() {
            if let Some(&n) = to_new.get(old) {
                folded[n] = f;
            }
        }
        self.folded = folded;
        self.open_layers = self
            .open_layers
            .iter()
            .map(|&(gi, li)| (to_new.get(gi).copied().unwrap_or(gi), li))
            .collect();
        let remap = |gi: usize| to_new.get(gi).copied().unwrap_or(gi);
        let selected = self.selected().map(|n| match n {
            Node::Group(gi) => Node::Group(remap(gi)),
            Node::Layer { group, layer } => Node::Layer {
                group: remap(group),
                layer,
            },
            Node::Hunk { group, layer, hunk } => Node::Hunk {
                group: remap(group),
                layer,
                hunk,
            },
            Node::File { group, file } => Node::File {
                group: remap(group),
                file,
            },
        });
        self.tree = self.flatten();
        let pos = self.pos;
        if let Some(i) = selected.and_then(|n| self.tree.iter().position(|x| *x == n)) {
            self.sel = i;
        }
        self.build_content(sh);
        self.pos = pos;
        self.pos.clamp(self.rows.len());
    }

    fn flatten(&self) -> Vec<Node> {
        match &self.grouping {
            Some(g) => flatten(g, &self.folded, &self.open_layers),
            None => Vec::new(),
        }
    }

    pub fn is_group_folded(&self, group: usize) -> bool {
        self.folded.get(group).copied().unwrap_or(false)
    }

    pub fn is_layer_open(&self, group: usize, layer: usize) -> bool {
        self.open_layers.contains(&(group, layer))
    }

    pub fn selected(&self) -> Option<Node> {
        self.tree.get(self.sel).copied()
    }

    fn select(&mut self, sh: &Shared, i: usize) {
        let i = i.min(self.tree.len().saturating_sub(1));
        let changed = i != self.sel || self.rows.is_empty();
        self.sel = i;
        if changed {
            self.build_content(sh);
        }
    }

    fn select_node(&mut self, sh: &Shared, node: Node) {
        if let Some(i) = self.tree.iter().position(|n| *n == node) {
            self.sel = usize::MAX;
            self.select(sh, i);
        }
    }

    /// Recomputes the visible tree, keeping the selection on the same node
    /// (or its closest visible ancestor).
    fn refresh_tree(&mut self, sh: &Shared) {
        let current = self.selected();
        self.tree = self.flatten();
        let mut node = current;
        while let Some(n) = node {
            if let Some(i) = self.tree.iter().position(|x| *x == n) {
                if Some(n) == current {
                    self.sel = i;
                } else {
                    self.sel = usize::MAX;
                    self.select(sh, i);
                }
                return;
            }
            node = n.parent();
        }
        self.sel = usize::MAX;
        self.select(sh, 0);
    }

    // ----- content ------------------------------------------------------

    fn build_content(&mut self, sh: &Shared) {
        self.rows.clear();
        self.pos = Scroll {
            height: self.pos.height,
            ..Scroll::default()
        };
        let (Some(g), Some(node)) = (&self.grouping, self.selected()) else {
            return;
        };
        let diff = &sh.review.diff;
        let mut rows = Vec::new();
        let push_hunks = |rows: &mut Vec<StackRow>, ids: &[usize]| {
            let mut last_file = None;
            let mut buf = Vec::new();
            for &h in ids {
                let f = diff.hunks[h].file;
                if last_file != Some(f) {
                    if last_file.is_some() {
                        rows.push(StackRow::Diff(Row::Spacer));
                    }
                    rows.push(StackRow::File(f));
                    last_file = Some(f);
                }
                buf.clear();
                push_hunk_rows(&mut buf, h, &diff.hunks[h].lines, sh.layout);
                rows.extend(buf.iter().map(|r| StackRow::Diff(*r)));
            }
        };
        let push_files = |rows: &mut Vec<StackRow>, ids: &[usize]| {
            for &f in ids {
                rows.push(StackRow::File(f));
                rows.push(StackRow::Diff(Row::Note(f)));
            }
        };
        match node {
            Node::Group(gi) => {
                let group = &g.groups[gi];
                for (li, layer) in group.layers.iter().enumerate() {
                    if li > 0 {
                        rows.push(StackRow::Diff(Row::Spacer));
                    }
                    rows.push(StackRow::Layer {
                        group: gi,
                        layer: li,
                    });
                    push_hunks(&mut rows, &layer.hunk_ids);
                }
                if !group.file_ids.is_empty() && !group.layers.is_empty() {
                    rows.push(StackRow::Diff(Row::Spacer));
                }
                push_files(&mut rows, &group.file_ids);
            }
            Node::Layer { group, layer } => {
                rows.push(StackRow::Layer { group, layer });
                push_hunks(&mut rows, &g.groups[group].layers[layer].hunk_ids);
            }
            Node::Hunk { hunk, .. } => push_hunks(&mut rows, &[hunk]),
            Node::File { file, .. } => push_files(&mut rows, &[file]),
        }
        self.rows = rows;
    }

    /// Rebuilds the content (after a layout change), keeping the cursor on
    /// the same hunk.
    pub fn relayout(&mut self, sh: &Shared) {
        let hunk = self.cursor_hunk();
        self.build_content(sh);
        if let Some((h, _)) = hunk
            && let Some(r) = self
                .rows
                .iter()
                .position(|r| *r == StackRow::Diff(Row::Hunk(h)))
        {
            self.pos.goto_top(r, self.rows.len());
        }
    }

    pub fn set_view_height(&mut self, h: usize) {
        if h != self.pos.height {
            self.pos.height = h;
            self.pos.clamp(self.rows.len());
        }
    }

    fn cursor_hunk(&self) -> Option<(usize, Option<usize>)> {
        match self.rows.get(self.pos.cursor)? {
            StackRow::Diff(r) => r.hunk_line(),
            _ => None,
        }
    }

    fn move_content(&mut self, delta: isize) {
        self.pos.cursor = self.pos.cursor.saturating_add_signed(delta);
        self.pos.clamp(self.rows.len());
    }

    fn next_content_hunk(&mut self, forward: bool) {
        let is_head = |r: &StackRow| {
            matches!(
                r,
                StackRow::Diff(Row::Hunk(_)) | StackRow::File(_) | StackRow::Layer { .. }
            )
        };
        let c = self.pos.cursor;
        let found = if forward {
            (c + 1..self.rows.len()).find(|&i| is_head(&self.rows[i]))
        } else {
            (0..c).rev().find(|&i| is_head(&self.rows[i]))
        };
        if let Some(r) = found {
            self.pos.goto_top(r, self.rows.len());
        }
    }

    // ----- tree navigation ----------------------------------------------

    fn move_sel(&mut self, sh: &Shared, delta: isize) {
        let i = self.sel.saturating_add_signed(delta);
        self.select(sh, i);
    }

    fn next_group(&mut self, sh: &Shared, forward: bool) {
        let Some(node) = self.selected() else {
            return;
        };
        let g = node.group();
        let target = if forward {
            g + 1
        } else if node.depth() > 0 {
            g
        } else {
            g.saturating_sub(1)
        };
        self.select_node(sh, Node::Group(target));
    }

    /// Selects the next group not fully reviewed after the current one,
    /// wrapping around.
    pub fn next_unreviewed_group(&mut self, sh: &mut Shared) {
        let Some(g) = &self.grouping else {
            return;
        };
        let n = g.groups.len();
        let start = self.selected().map_or(0, |s| s.group() + 1);
        let d = &sh.review.diff;
        let next = (0..n)
            .map(|k| (start + k) % n)
            .find(|&i| !g.groups[i].is_reviewed(d, &sh.state));
        match next {
            Some(i) => self.select_node(sh, Node::Group(i)),
            None => sh.notify("every group is reviewed"),
        }
    }

    fn toggle_fold(&mut self, sh: &Shared) {
        let Some(node) = self.selected() else {
            return;
        };
        match node {
            Node::Group(g) => self.folded[g] = !self.folded[g],
            Node::Layer { group, layer } => {
                if !self.open_layers.remove(&(group, layer)) {
                    self.open_layers.insert((group, layer));
                }
            }
            Node::Hunk { group, layer, .. } => {
                self.open_layers.remove(&(group, layer));
            }
            Node::File { group, .. } => self.folded[group] = true,
        }
        self.refresh_tree(sh);
    }

    fn set_all_folded(&mut self, sh: &Shared, folded: bool) {
        self.folded.iter_mut().for_each(|f| *f = folded);
        if folded {
            self.open_layers.clear();
        } else if let Some(g) = &self.grouping {
            for (gi, group) in g.groups.iter().enumerate() {
                self.open_layers
                    .extend((0..group.layers.len()).map(|li| (gi, li)));
            }
        }
        self.refresh_tree(sh);
    }

    /// `h`: fold the node, or go to its parent.
    fn tree_left(&mut self, sh: &Shared) {
        let Some(node) = self.selected() else {
            return;
        };
        match node {
            Node::Group(g) if !self.folded[g] => self.toggle_fold(sh),
            Node::Layer { group, layer } if self.open_layers.contains(&(group, layer)) => {
                self.toggle_fold(sh)
            }
            _ => {
                if let Some(p) = node.parent() {
                    self.select_node(sh, p);
                }
            }
        }
    }

    /// `l`: unfold the node, or go to its first child, or to the content.
    fn tree_right(&mut self, sh: &Shared) {
        let Some(node) = self.selected() else {
            return;
        };
        let folded = match node {
            Node::Group(g) => self.folded[g],
            Node::Layer { group, layer } => !self.open_layers.contains(&(group, layer)),
            _ => {
                self.focus = Focus::Content;
                return;
            }
        };
        if folded {
            self.toggle_fold(sh);
        } else if self
            .tree
            .get(self.sel + 1)
            .is_some_and(|n| n.depth() > node.depth())
        {
            self.move_sel(sh, 1);
        }
    }

    // ----- review -------------------------------------------------------

    /// Toggles "reviewed" on everything under the selected node. When marked,
    /// moves on: to the next unreviewed group if this one is done, else to the
    /// next unreviewed node of the group.
    pub fn toggle_reviewed(&mut self, sh: &mut Shared) {
        let (Some(g), Some(node)) = (&self.grouping, self.selected()) else {
            return;
        };
        let d = &sh.review.diff;
        let (done, total) = node_progress(g, node, d, &sh.state);
        let reviewed = done < total;
        let (hunks, files) = node_items(g, node);
        for h in hunks {
            sh.state.set_hunk(d, h, reviewed);
        }
        for f in files {
            sh.state.set_file(d, f, reviewed);
        }
        sh.save();
        if !reviewed {
            return;
        }

        let d = &sh.review.diff;
        let gi = node.group();
        if g.groups[gi].is_reviewed(d, &sh.state) {
            self.folded[gi] = true;
            self.refresh_tree(sh);
            self.next_unreviewed_group(sh);
            return;
        }
        // Skip the node's own subtree, then find an unreviewed node of the group.
        let mut i = self.sel + 1;
        while self.tree.get(i).is_some_and(|n| n.depth() > node.depth()) {
            i += 1;
        }
        let next = (i..self.tree.len())
            .take_while(|&k| self.tree[k].group() == gi)
            .find(|&k| {
                let (a, b) = node_progress(g, self.tree[k], d, &sh.state);
                a < b
            });
        if let Some(k) = next {
            self.select(sh, k);
        }
    }

    // ----- actions ------------------------------------------------------

    /// The Diff view position matching the cursor (content) or the node (tree).
    fn jump_target(&self) -> Action {
        let (Some(g), Some(node)) = (&self.grouping, self.selected()) else {
            return Action::None;
        };
        if self.focus == Focus::Content {
            match self.rows.get(self.pos.cursor) {
                Some(StackRow::Diff(r)) => {
                    if let Some((h, _)) = r.hunk_line() {
                        return Action::ShowHunk(h);
                    }
                    if let Row::Note(f) = r {
                        return Action::ShowFile(*f);
                    }
                }
                Some(StackRow::File(f)) => {
                    // The first hunk of this file shown below, if any.
                    let first = self.rows[self.pos.cursor..].iter().find_map(|r| match r {
                        StackRow::Diff(Row::Hunk(h)) => Some(*h),
                        _ => None,
                    });
                    return match first {
                        Some(h) => Action::ShowHunk(h),
                        None => Action::ShowFile(*f),
                    };
                }
                _ => {}
            }
        }
        let (hunks, files) = node_items(g, node);
        match (hunks.first(), files.first()) {
            (Some(&h), _) => Action::ShowHunk(h),
            (None, Some(&f)) => Action::ShowFile(f),
            _ => Action::None,
        }
    }

    /// `gs`: the Graph view of the symbol changed by the hunk under the
    /// cursor (content) or the first hunk of the node (tree).
    fn show_symbol(&self, sh: &mut Shared) -> Action {
        let Action::ShowHunk(h) = self.jump_target() else {
            return Action::None;
        };
        let line = match (self.focus, self.cursor_hunk()) {
            (Focus::Content, Some((ch, l))) if ch == h => l,
            _ => None,
        };
        let Some(g) = &sh.graph else {
            sh.notify("the code graph is still being built…");
            return Action::None;
        };
        let file = sh.review.diff.hunks[h].file;
        match symbol_at_position(g, &sh.review.diff, file, Some(h), line) {
            Some(s) => Action::ShowSymbol(s),
            None => {
                sh.notify("no symbol here (language not indexed?)");
                Action::None
            }
        }
    }

    fn open_in_editor(&self, sh: &mut Shared) {
        if let Action::ShowHunk(h) = self.jump_target() {
            let line = match (self.focus, self.cursor_hunk()) {
                (Focus::Content, Some((ch, l))) if ch == h => l,
                _ => None,
            };
            let file = sh.review.diff.hunks[h].file;
            sh.open_in_editor(file, Some(h), line);
        } else if let Action::ShowFile(f) = self.jump_target() {
            sh.open_in_editor(f, None, None);
        }
    }

    // ----- keys ---------------------------------------------------------

    pub fn on_key(&mut self, sh: &mut Shared, key: KeyEvent, llm_enabled: bool) -> Action {
        if self.show_warnings {
            self.show_warnings = false;
            return Action::None;
        }
        if self.confirm_regroup {
            self.confirm_regroup = false;
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                return Action::Regroup;
            }
            sh.notify("regrouping cancelled");
            return Action::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(p) = self.pending.take() {
            match (p, key.code) {
                ('g', KeyCode::Char('g')) => match self.focus {
                    Focus::List => self.select(sh, 0),
                    Focus::Content => self.pos.goto_top(0, self.rows.len()),
                },
                ('g', KeyCode::Char('d')) => return self.jump_target(),
                ('g', KeyCode::Char('s')) => return self.show_symbol(sh),
                ('z', KeyCode::Char('a' | 'o' | 'c')) => self.toggle_fold(sh),
                ('z', KeyCode::Char('M')) => self.set_all_folded(sh, true),
                ('z', KeyCode::Char('R')) => self.set_all_folded(sh, false),
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
                self.list_hidden = false;
                self.focus = Focus::List;
            }
            KeyCode::Char('l') if ctrl => self.focus = Focus::Content,
            KeyCode::Char('B') => {
                self.list_hidden = !self.list_hidden;
                if self.list_hidden {
                    self.focus = Focus::Content;
                }
            }
            KeyCode::Char('R') => {
                if llm_enabled {
                    self.confirm_regroup = true;
                } else {
                    return Action::Regroup;
                }
            }
            KeyCode::Char('w') => match &self.grouping {
                Some(g) if !g.warnings.is_empty() => self.show_warnings = true,
                _ => sh.notify("no grouping warning"),
            },
            _ if self.grouping.is_none() => {}
            KeyCode::Char(' ') => self.toggle_reviewed(sh),
            KeyCode::Char('u') if !ctrl => self.next_unreviewed_group(sh),
            KeyCode::Char('J') | KeyCode::Char(']') => self.next_group(sh, true),
            KeyCode::Char('K') | KeyCode::Char('[') => self.next_group(sh, false),
            KeyCode::Char('o') => self.toggle_fold(sh),
            KeyCode::Char('e') => self.open_in_editor(sh),
            KeyCode::Char('0') => self.hscroll = 0,
            _ => match self.focus {
                Focus::List => return self.on_tree_key(sh, key),
                Focus::Content => return self.on_content_key(key),
            },
        }
        Action::None
    }

    fn on_tree_key(&mut self, sh: &Shared, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.pos.height / 2).max(1) as isize;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(sh, 1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(sh, -1),
            KeyCode::Char('d') if ctrl => self.move_sel(sh, half),
            KeyCode::Char('u') if ctrl => self.move_sel(sh, -half),
            KeyCode::PageDown => self.move_sel(sh, half * 2),
            KeyCode::PageUp => self.move_sel(sh, -half * 2),
            KeyCode::Char('G') | KeyCode::End => self.select(sh, usize::MAX),
            KeyCode::Home => self.select(sh, 0),
            KeyCode::Char('h') | KeyCode::Left => self.tree_left(sh),
            KeyCode::Char('l') | KeyCode::Right => self.tree_right(sh),
            KeyCode::Enter => match self.selected() {
                Some(Node::Group(_) | Node::Layer { .. }) => self.toggle_fold(sh),
                Some(_) => return self.jump_target(),
                None => {}
            },
            _ => {}
        }
        Action::None
    }

    fn on_content_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.pos.height / 2).max(1) as isize;
        let page = self.pos.height.max(1) as isize;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_content(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_content(-1),
            KeyCode::Char('d') if ctrl => self.move_content(half),
            KeyCode::Char('u') if ctrl => self.move_content(-half),
            KeyCode::Char('f') if ctrl => self.move_content(page),
            KeyCode::Char('b') if ctrl => self.move_content(-page),
            KeyCode::PageDown => self.move_content(page),
            KeyCode::PageUp => self.move_content(-page),
            KeyCode::Char('G') | KeyCode::End => {
                self.pos.cursor = self.rows.len().saturating_sub(1);
                self.pos.clamp(self.rows.len());
            }
            KeyCode::Home => self.pos.goto_top(0, self.rows.len()),
            KeyCode::Char('n') | KeyCode::Char('}') => self.next_content_hunk(true),
            KeyCode::Char('N') | KeyCode::Char('{') => self.next_content_hunk(false),
            KeyCode::Char('h') | KeyCode::Left => self.hscroll = self.hscroll.saturating_sub(8),
            KeyCode::Char('l') | KeyCode::Right => self.hscroll += 8,
            KeyCode::Enter => return self.jump_target(),
            KeyCode::Esc => self.focus = Focus::List,
            _ => {}
        }
        Action::None
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use survol_core::git::Git;
    use survol_core::group::{Group, Layer, Source};
    use survol_core::review::Review;

    use super::*;
    use crate::views::Layout;

    const RAW: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
        @@ -1,1 +1,1 @@\n-a\n+b\n@@ -10,1 +10,1 @@\n-c\n+d\n\
        diff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n\
        @@ -1,1 +1,1 @@\n-e\n+f\n\
        diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n\
        @@ -1,1 +1,1 @@\n-g\n+h\n\
        diff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n";

    fn group(id: usize, layers: &[(&str, &[usize])], files: &[usize], mechanical: bool) -> Group {
        let layers: Vec<Layer> = layers
            .iter()
            .map(|(n, ids)| Layer {
                name: n.to_string(),
                hunk_ids: ids.to_vec(),
            })
            .collect();
        let mut hunk_ids: Vec<usize> = layers.iter().flat_map(|l| l.hunk_ids.clone()).collect();
        hunk_ids.sort_unstable();
        Group {
            id,
            title: format!("group {id}"),
            summary: "summary".into(),
            layers,
            order: id,
            hunk_ids,
            file_ids: files.to_vec(),
            mechanical,
        }
    }

    /// Hunks 0, 1 in src/a.rs, 2 in src/b.rs, 3 in Cargo.lock; file `y` is a pure rename.
    /// A review and a grouping of it, the review state saved under `dir`.
    pub(crate) fn fixture(dir: &std::path::Path) -> (Review, Grouping) {
        let diff = survol_core::diff::parse(RAW.as_bytes()).unwrap();
        let rename = diff.files.iter().position(|f| f.path == "y").unwrap();
        let review = Review {
            mr: None,
            base_sha: "base".into(),
            head_sha: "head".into(),
            diff,
            worktree: dir.to_path_buf(),
            state_key: "test".into(),
            repo: Git::new(dir),
        };
        let grouping = Grouping {
            groups: vec![
                group(0, &[("model", &[0]), ("api", &[1])], &[], false),
                group(1, &[("tests", &[2])], &[], false),
                group(2, &[("generated", &[3])], &[rename], true),
            ],
            source: Source::Llm,
            model: None,
            prompt_version: 0,
            key: String::new(),
            from_cache: false,
            llm_calls: 1,
            warnings: Vec::new(),
        };
        (review, grouping)
    }

    fn setup(dir: &std::path::Path) -> (Shared, StackView) {
        let (review, grouping) = fixture(dir);
        let sh = Shared::new(review, ReviewState::default(), dir.join("state.json"));
        let mut v = StackView::default();
        v.set_grouping(&sh, grouping);
        (sh, v)
    }

    pub(crate) fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn flattens_with_folds() {
        let dir = tempfile::tempdir().unwrap();
        let (_, v) = setup(dir.path());
        // Mechanical group folded, layers folded.
        assert_eq!(
            v.tree,
            [
                Node::Group(0),
                Node::Layer { group: 0, layer: 0 },
                Node::Layer { group: 0, layer: 1 },
                Node::Group(1),
                Node::Layer { group: 1, layer: 0 },
                Node::Group(2),
            ]
        );
        let g = v.grouping.as_ref().unwrap();
        let open: HashSet<_> = [(0, 1), (2, 0)].into();
        let all = flatten(g, &[false, true, false], &open);
        assert_eq!(
            all,
            [
                Node::Group(0),
                Node::Layer { group: 0, layer: 0 },
                Node::Layer { group: 0, layer: 1 },
                Node::Hunk {
                    group: 0,
                    layer: 1,
                    hunk: 1
                },
                Node::Group(1),
                Node::Group(2),
                Node::Layer { group: 2, layer: 0 },
                Node::Hunk {
                    group: 2,
                    layer: 0,
                    hunk: 3
                },
                Node::File { group: 2, file: 3 },
            ]
        );
    }

    #[test]
    fn content_of_a_group_is_its_hunks_by_layer_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        assert_eq!(v.selected(), Some(Node::Group(0)));
        let heads: Vec<StackRow> = v
            .rows
            .iter()
            .copied()
            .filter(|r| !matches!(r, StackRow::Diff(Row::Line { .. } | Row::Spacer)))
            .collect();
        assert_eq!(
            heads,
            [
                StackRow::Layer { group: 0, layer: 0 },
                StackRow::File(0),
                StackRow::Diff(Row::Hunk(0)),
                StackRow::Layer { group: 0, layer: 1 },
                StackRow::File(0),
                StackRow::Diff(Row::Hunk(1)),
            ]
        );
        // Split layout pairs the lines, cursor stays on the same hunk.
        v.focus = Focus::Content;
        v.next_content_hunk(true);
        v.next_content_hunk(true);
        assert_eq!(v.cursor_hunk(), Some((0, None)));
        sh.layout = Layout::Split;
        v.relayout(&sh);
        assert_eq!(v.cursor_hunk(), Some((0, None)));
        assert!(
            v.rows
                .iter()
                .any(|r| matches!(r, StackRow::Diff(Row::Pair { .. })))
        );
        assert_eq!(v.jump_target(), Action::ShowHunk(0));
    }

    #[test]
    fn space_marks_the_group_and_moves_to_the_next_unreviewed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        v.on_key(&mut sh, key(' '), true);
        let d = &sh.review.diff;
        assert!((0..2).all(|h| sh.state.is_hunk_reviewed(d, h)));
        assert!(!sh.state.is_hunk_reviewed(d, 2));
        // Saved at once.
        assert!(dir.path().join("state.json").exists());
        assert_eq!(v.selected(), Some(Node::Group(1)));
        assert_eq!(v.tree[1], Node::Group(1), "reviewed group 0 is folded");
        v.on_key(&mut sh, key(' '), true);
        // The mechanical group: its hunk and hunkless file in one key.
        assert_eq!(v.selected(), Some(Node::Group(2)));
        v.on_key(&mut sh, key(' '), true);
        let (done, total) = v
            .grouping
            .as_ref()
            .unwrap()
            .progress(&sh.review.diff, &sh.state);
        assert_eq!(done, total);
        assert_eq!(total, 5);
        // Toggling again un-reviews.
        v.on_key(&mut sh, key(' '), true);
        assert!(!sh.state.is_hunk_reviewed(&sh.review.diff, 3));
    }

    #[test]
    fn marking_a_layer_moves_within_the_group() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        v.on_key(&mut sh, key('j'), true);
        assert_eq!(v.selected(), Some(Node::Layer { group: 0, layer: 0 }));
        v.on_key(&mut sh, key(' '), true);
        assert!(sh.state.is_hunk_reviewed(&sh.review.diff, 0));
        assert_eq!(v.selected(), Some(Node::Layer { group: 0, layer: 1 }));
    }

    #[test]
    fn tree_navigation() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        // l on an unfolded group goes to its first child, then unfolds the layer.
        v.on_key(&mut sh, key('l'), true);
        assert_eq!(v.selected(), Some(Node::Layer { group: 0, layer: 0 }));
        v.on_key(&mut sh, key('l'), true);
        v.on_key(&mut sh, key('l'), true);
        assert_eq!(
            v.selected(),
            Some(Node::Hunk {
                group: 0,
                layer: 0,
                hunk: 0
            })
        );
        assert_eq!(v.rows.first(), Some(&StackRow::File(0)));
        // Enter on a hunk jumps to the Diff view.
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(v.on_key(&mut sh, enter, true), Action::ShowHunk(0));
        // h goes up to the layer, then folds it.
        v.on_key(&mut sh, key('h'), true);
        assert_eq!(v.selected(), Some(Node::Layer { group: 0, layer: 0 }));
        v.on_key(&mut sh, key('h'), true);
        assert!(!v.tree.iter().any(|n| matches!(n, Node::Hunk { .. })));
        // J / K by group, u to the next unreviewed group.
        v.on_key(&mut sh, key('J'), true);
        assert_eq!(v.selected(), Some(Node::Group(1)));
        v.on_key(&mut sh, key('u'), true);
        assert_eq!(v.selected(), Some(Node::Group(2)));
        v.on_key(&mut sh, key('u'), true);
        assert_eq!(v.selected(), Some(Node::Group(0)));
        // gd from a group: its first hunk.
        v.on_key(&mut sh, key('g'), true);
        assert_eq!(v.on_key(&mut sh, key('d'), true), Action::ShowHunk(0));
    }

    #[test]
    fn regroup_asks_for_confirmation_when_it_costs_an_llm_call() {
        let dir = tempfile::tempdir().unwrap();
        let (mut sh, mut v) = setup(dir.path());
        assert_eq!(v.on_key(&mut sh, key('R'), true), Action::None);
        assert!(v.captures_keys());
        assert_eq!(v.on_key(&mut sh, key('n'), true), Action::None);
        v.on_key(&mut sh, key('R'), true);
        assert_eq!(v.on_key(&mut sh, key('y'), true), Action::Regroup);
        assert_eq!(v.on_key(&mut sh, key('R'), false), Action::Regroup);
    }
}
