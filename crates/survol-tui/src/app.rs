//! Application state: what the views share, view switching, global keys and
//! the background grouping. Each view lives in `views/`, rendering in `ui/`.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::config::Config;
use survol_core::graph::{Graph, SymIdx, SymbolKind};
use survol_core::group::{Grouping, Source};
use survol_core::llm::ClaudeCli;
use survol_core::model::FileStatus;
use survol_core::review::{self, Review};
use survol_core::review_state::ReviewState;

use crate::editor;
use crate::highlight::Highlighter;
use crate::views::Layout;
use crate::views::diff::DiffView;
use crate::views::graph::GraphView;
use crate::views::stack::StackView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Diff,
    Stack,
    Graph,
}

impl View {
    /// In `Tab` order.
    pub const ALL: [View; 3] = [View::Diff, View::Stack, View::Graph];

    pub fn name(self) -> &'static str {
        match self {
            View::Diff => "Diff",
            View::Stack => "Stack",
            View::Graph => "Graph",
        }
    }
}

/// What a view asks the application to do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Show this hunk in the Diff view.
    ShowHunk(usize),
    /// Show this file in the Diff view.
    ShowFile(usize),
    /// Recompute the grouping without cache.
    Regroup,
    /// Show this symbol in the Graph view.
    ShowSymbol(SymIdx),
}

/// State shared by all views.
pub struct Shared {
    pub review: Review,
    pub state: ReviewState,
    state_path: PathBuf,
    pub highlighter: Highlighter,
    pub layout: Layout,
    pub worktree_ready: bool,
    /// Code graph, once built in the background.
    pub graph: Option<Graph>,
    /// Set when the terminal must be fully redrawn (after an external editor).
    pub needs_clear: bool,
    message: Option<(String, Instant)>,
}

impl Shared {
    pub fn new(review: Review, state: ReviewState, state_path: PathBuf) -> Self {
        let worktree_ready = review.worktree == review.repo.dir();
        Self {
            review,
            state,
            state_path,
            highlighter: Highlighter::new(),
            layout: Layout::Unified,
            worktree_ready,
            graph: None,
            needs_clear: false,
            message: None,
        }
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

    /// Writes the review state; called after every change.
    pub fn save(&mut self) {
        let head = self.review.head_sha.clone();
        if let Err(e) = self.state.save(&self.state_path, &head, &self.review.diff) {
            self.notify(format!("cannot save review state: {e}"));
        }
    }

    /// File and line to open for a position, on the new side.
    fn editor_target(
        &self,
        file: usize,
        hunk: Option<usize>,
        line: Option<usize>,
    ) -> Result<(PathBuf, u32), &'static str> {
        let d = &self.review.diff;
        let f = &d.files[file];
        if f.status == FileStatus::Deleted {
            return Err("file deleted in this review");
        }
        let line = match (hunk, line) {
            (Some(h), Some(l)) => {
                let hunk = &d.hunks[h];
                // A removed line has no new number: use the next line that has one.
                hunk.lines[l..]
                    .iter()
                    .find_map(|x| x.new_line)
                    .unwrap_or(hunk.new_range.start)
            }
            (Some(h), None) => d.hunks[h].new_range.start,
            _ => 1,
        };
        Ok((self.review.worktree.join(&f.path), line.max(1)))
    }

    /// Opens `file` at the hunk / line in the editor (parent Neovim if any).
    pub fn open_in_editor(&mut self, file: usize, hunk: Option<usize>, line: Option<usize>) {
        match self.editor_target(file, hunk, line) {
            Ok((path, line)) => self.open_editor_at(&path, line),
            Err(e) => self.notify(e),
        }
    }

    /// Opens `path` (relative to the repository, changed or not) at `line`.
    pub fn open_path(&mut self, path: &str, line: u32) {
        let path = self.review.worktree.join(path);
        self.open_editor_at(&path, line.max(1));
    }

    fn open_editor_at(&mut self, path: &std::path::Path, line: u32) {
        if !self.worktree_ready {
            return self.notify("worktree is still being checked out…");
        }
        match editor::open(path, line) {
            Ok(editor::Opened::Plugin | editor::Opened::Parent) => {
                self.notify(format!("opened in nvim: {}:{line}", path.display()))
            }
            Ok(editor::Opened::Foreground) => self.needs_clear = true,
            Err(e) => self.notify(format!("cannot open editor: {e}")),
        }
    }
}

enum GroupEvent {
    Progress(String),
    Done(Result<Grouping, String>),
}

enum GraphEvent {
    Progress(String),
    Done(Box<Result<Graph, String>>),
}

/// State of the background graph build.
pub enum GraphStatus {
    NotStarted,
    Running { since: Instant, progress: String },
    Done,
    Failed(String),
}

/// State of the background grouping.
pub enum GroupStatus {
    Running {
        since: Instant,
        progress: String,
        /// Regenerating: the previous grouping is still displayed.
        regen: bool,
    },
    Done,
    Failed(String),
}

pub struct App {
    pub sh: Shared,
    pub view: View,
    pub diff: DiffView,
    pub stack: StackView,
    pub graph: GraphView,
    pub cfg: Config,
    pub group_status: GroupStatus,
    group_rx: Option<mpsc::Receiver<GroupEvent>>,
    pub graph_status: GraphStatus,
    graph_rx: Option<mpsc::Receiver<GraphEvent>>,
    pub help: bool,
    pub quit: bool,
}

impl App {
    pub fn new(review: Review, state: ReviewState, state_path: PathBuf, cfg: Config) -> Self {
        let sh = Shared::new(review, state, state_path);
        let diff = DiffView::new(&sh);
        Self {
            sh,
            view: View::Diff,
            diff,
            stack: StackView::default(),
            graph: GraphView::default(),
            cfg,
            group_status: GroupStatus::Done,
            group_rx: None,
            graph_status: GraphStatus::NotStarted,
            graph_rx: None,
            help: false,
            quit: false,
        }
    }

    // ----- grouping -----------------------------------------------------

    /// Groups the hunks on a background thread; the views stay usable.
    pub fn start_grouping(&mut self, use_cache: bool) {
        let (tx, rx) = mpsc::channel();
        let review = self.sh.review.clone();
        let cfg = self.cfg.clone();
        std::thread::spawn(move || {
            let llm = ClaudeCli::from_config(&cfg.llm);
            let progress = |m: &str| {
                let _ = tx.send(GroupEvent::Progress(m.to_string()));
            };
            let res = review::group(&review, &cfg, &llm, progress, use_cache);
            let _ = tx.send(GroupEvent::Done(res.map_err(|e| e.to_string())));
        });
        self.group_rx = Some(rx);
        self.group_status = GroupStatus::Running {
            since: Instant::now(),
            progress: String::new(),
            regen: self.stack.grouping.is_some(),
        };
    }

    /// Applies what the grouping thread reported since the last call.
    pub fn poll_grouping(&mut self) {
        let Some(rx) = &self.group_rx else {
            return;
        };
        let mut done = None;
        for ev in rx.try_iter() {
            match ev {
                GroupEvent::Progress(p) => {
                    if let GroupStatus::Running { progress, .. } = &mut self.group_status {
                        *progress = p;
                    }
                }
                GroupEvent::Done(res) => done = Some(res),
            }
        }
        let Some(res) = done else {
            return;
        };
        self.group_rx = None;
        match res {
            Ok(mut g) => {
                if let Some(graph) = &self.sh.graph {
                    g.order_with_graph(graph);
                }
                let n = g.warnings.len();
                self.group_status = GroupStatus::Done;
                self.stack.set_grouping(&self.sh, g);
                if n > 0 {
                    self.sh
                        .notify(format!("grouping: {n} warning(s), `w` in the Stack view"));
                }
            }
            Err(e) => {
                self.sh.notify(format!("grouping failed: {e}"));
                self.group_status = GroupStatus::Failed(e);
            }
        }
    }

    // ----- graph --------------------------------------------------------

    /// Builds the code graph on a background thread (from git objects: no
    /// need to wait for the worktree).
    pub fn start_graph(&mut self) {
        let (tx, rx) = mpsc::channel();
        let review = self.sh.review.clone();
        let cfg = self.cfg.clone();
        std::thread::spawn(move || {
            let progress = |m: &str| {
                let _ = tx.send(GraphEvent::Progress(m.to_string()));
            };
            let res = review::build_graph(&review, &cfg, progress, true);
            let _ = tx.send(GraphEvent::Done(Box::new(res.map_err(|e| e.to_string()))));
        });
        self.graph_rx = Some(rx);
        self.graph_status = GraphStatus::Running {
            since: Instant::now(),
            progress: String::new(),
        };
    }

    /// Applies what the graph thread reported since the last call.
    pub fn poll_graph(&mut self) {
        let Some(rx) = &self.graph_rx else {
            return;
        };
        let mut done = None;
        for ev in rx.try_iter() {
            match ev {
                GraphEvent::Progress(p) => {
                    if let GraphStatus::Running { progress, .. } = &mut self.graph_status {
                        *progress = p;
                    }
                }
                GraphEvent::Done(res) => done = Some(*res),
            }
        }
        let Some(res) = done else {
            return;
        };
        self.graph_rx = None;
        match res {
            Ok(g) => self.set_graph(g),
            Err(e) => {
                self.sh.notify(format!("graph failed: {e}"));
                self.graph_status = GraphStatus::Failed(e);
            }
        }
    }

    /// Installs a built graph: the Graph view shows it and the Stack groups
    /// follow its dependency order.
    pub fn set_graph(&mut self, g: Graph) {
        self.graph_status = GraphStatus::Done;
        self.stack.reorder_with_graph(&self.sh, &g);
        self.sh.graph = Some(g);
        self.graph.on_graph_ready(&self.sh);
    }

    /// Short description of the graph for the header.
    pub fn graph_label(&self) -> String {
        match &self.graph_status {
            GraphStatus::NotStarted => String::new(),
            GraphStatus::Running { since, .. } => {
                format!("⟳ graph… {}s", since.elapsed().as_secs())
            }
            GraphStatus::Failed(_) => "graph failed".into(),
            GraphStatus::Done => match &self.sh.graph {
                Some(g) => {
                    let n = g
                        .changed_symbols()
                        .into_iter()
                        .filter(|&s| g.symbol(s).kind != SymbolKind::File)
                        .count();
                    let cached = if g.from_cache { " · cached" } else { "" };
                    format!("graph: {n} changed symbols{cached}")
                }
                None => String::new(),
            },
        }
    }

    pub fn grouping_running(&self) -> bool {
        matches!(self.group_status, GroupStatus::Running { .. })
    }

    /// Short description of the grouping for the header.
    pub fn grouping_label(&self) -> String {
        match &self.group_status {
            GroupStatus::Running { since, regen, .. } => format!(
                "⟳ {} {}s",
                if *regen {
                    "regrouping…"
                } else {
                    "grouping…"
                },
                since.elapsed().as_secs()
            ),
            GroupStatus::Failed(_) => "grouping failed".into(),
            GroupStatus::Done => match &self.stack.grouping {
                None => String::new(),
                Some(g) => {
                    let source = match g.source {
                        Source::Llm => "LLM",
                        Source::Partial => "partial",
                        Source::Fallback if !self.cfg.llm.enabled => "no LLM",
                        Source::Fallback => "fallback",
                        Source::Mechanical => "mechanical",
                    };
                    let mut s = format!("{} groups · {source}", g.groups.len());
                    if g.from_cache {
                        s.push_str(" · cached");
                    }
                    if !g.warnings.is_empty() {
                        s.push_str(&format!(" · ⚠ {}", g.warnings.len()));
                    }
                    s
                }
            },
        }
    }

    // ----- views --------------------------------------------------------

    fn switch(&mut self, view: View) {
        self.view = view;
    }

    fn cycle(&mut self, forward: bool) {
        let n = View::ALL.len();
        let i = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
        let next = if forward { i + 1 } else { i + n - 1 } % n;
        self.switch(View::ALL[next]);
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::ShowHunk(h) => {
                self.diff.reveal_hunk(&self.sh, h);
                self.switch(View::Diff);
            }
            Action::ShowFile(f) => {
                self.diff.reveal_file(&self.sh, f);
                self.switch(View::Diff);
            }
            Action::ShowSymbol(s) => {
                if self.sh.graph.is_some() {
                    self.graph.show_symbol(&self.sh, s);
                    self.switch(View::Graph);
                }
            }
            Action::Regroup => {
                if self.grouping_running() {
                    self.sh.notify("grouping is already running");
                } else {
                    self.start_grouping(false);
                }
            }
        }
    }

    fn toggle_layout(&mut self) {
        self.sh.layout = match self.sh.layout {
            Layout::Unified => Layout::Split,
            Layout::Split => Layout::Unified,
        };
        self.diff.relayout(&self.sh);
        self.stack.relayout(&self.sh);
    }

    // ----- keys ---------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.help {
            self.help = false;
            return;
        }
        let captured = match self.view {
            View::Diff => self.diff.captures_keys(),
            View::Stack => self.stack.captures_keys(),
            View::Graph => self.graph.captures_keys(),
        };
        if !captured && self.on_global_key(key) {
            return;
        }
        let action = match self.view {
            View::Diff => self.diff.on_key(&mut self.sh, key),
            View::Stack => self.stack.on_key(&mut self.sh, key, self.cfg.llm.enabled),
            View::Graph => self.graph.on_key(&mut self.sh, key),
        };
        self.apply(action);
    }

    /// Keys valid in every view. Returns whether the key was used.
    fn on_global_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab => self.cycle(true),
            KeyCode::BackTab => self.cycle(false),
            KeyCode::Char('1') => self.switch(View::Diff),
            KeyCode::Char('2') => self.switch(View::Stack),
            KeyCode::Char('3') => self.switch(View::Graph),
            KeyCode::Char('s') if !ctrl => self.toggle_layout(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::Row;
    use crate::views::stack::tests::{fixture, key};

    #[test]
    fn graph_reorders_the_stack_keeping_the_selection() {
        let dir = tempfile::tempdir().unwrap();
        let (review, mut grouping) = fixture(dir.path());
        // Put the tests group first: the graph order brings it back last.
        grouping.groups.swap(0, 1);
        let mut app = App::new(
            review,
            ReviewState::default(),
            dir.path().join("state.json"),
            Config::default(),
        );
        app.stack.set_grouping(&app.sh, grouping);
        app.on_key(key('2'));
        app.on_key(key('J'));
        let selected = |app: &App| {
            let g = app.stack.grouping.as_ref().unwrap();
            g.groups[app.stack.selected().unwrap().group()].id
        };
        assert_eq!(selected(&app), 0);
        app.set_graph(Graph::default());
        let ids: Vec<usize> = app
            .stack
            .grouping
            .as_ref()
            .unwrap()
            .groups
            .iter()
            .map(|g| g.id)
            .collect();
        assert_eq!(ids, [0, 1, 2]);
        assert_eq!(selected(&app), 0);
        assert!(app.graph_label().starts_with("graph: 0 changed"));
        // `3` jumps to the Graph view.
        app.on_key(key('3'));
        assert_eq!(app.view, View::Graph);
    }

    #[test]
    fn views_share_the_review_state_and_jump_to_the_diff() {
        let dir = tempfile::tempdir().unwrap();
        let (review, grouping) = fixture(dir.path());
        let mut app = App::new(
            review,
            ReviewState::default(),
            dir.path().join("state.json"),
            Config::default(),
        );
        app.stack.set_grouping(&app.sh, grouping);

        app.on_key(key('2'));
        assert_eq!(app.view, View::Stack);
        // Validate the first group from the Stack view: the Diff view sees it.
        app.on_key(key(' '));
        assert!(app.sh.state.is_hunk_reviewed(&app.sh.review.diff, 0));
        assert!(app.sh.state.is_file_reviewed(&app.sh.review.diff, 0));

        // gd on the next group (src/b.rs) opens its hunk in the Diff view.
        app.on_key(key('g'));
        app.on_key(key('d'));
        assert_eq!(app.view, View::Diff);
        assert_eq!(app.diff.rows[app.diff.pos.cursor], Row::Hunk(2));

        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.view, View::Stack);
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.view, View::Diff);
        // `s` switches both views to split.
        app.on_key(key('s'));
        assert_eq!(app.sh.layout, Layout::Split);
        assert!(
            app.stack
                .rows
                .iter()
                .any(|r| matches!(r, crate::views::stack::StackRow::Diff(Row::Pair { .. })))
        );
    }
}
