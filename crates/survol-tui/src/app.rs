//! Application state: what the views share, view switching, global keys and
//! the background grouping. Each view lives in `views/`, rendering in `ui/`.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::ask::{self, Answer, CodeRef};
use survol_core::config::Config;
use survol_core::graph::{Graph, SymIdx, SymbolKind};
use survol_core::group::{Grouping, Source};
use survol_core::llm::{ClaudeCli, LlmProvider};
use survol_core::model::{FileStatus, Side};
use survol_core::review::{self, Review};
use survol_core::review_state::ReviewState;

use crate::editor;
use crate::highlight::Highlighter;
use crate::views::Layout;
use crate::views::ask::{
    AnswerOutcome, AnswerView, AskInput, HistoryOutcome, HistoryView, InputOutcome, Pending,
};
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
    /// Show this line (index in the hunk) in the Diff view.
    ShowLine {
        hunk: usize,
        line: usize,
    },
}

/// A modal window over the views.
pub enum Popup {
    AskInput(AskInput),
    /// Waiting for the LLM.
    Pending(Pending),
    Answer(AnswerView),
    History(HistoryView),
}

/// Something that answers questions; `Send` to run on a background thread.
pub type Llm = Arc<dyn LlmProvider + Send>;

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
            Ok(editor::Opened::Parent) => {
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

    pub popup: Option<Popup>,
    /// Answers questions (the Claude CLI; a fake in tests).
    pub llm: Llm,
    /// Question being answered in the background.
    pub ask_pending: Option<Pending>,
    ask_rx: Option<mpsc::Receiver<Result<Answer, String>>>,
    /// Questions of this review, oldest first.
    pub history: Vec<Answer>,
    /// Last answer shown: `A` reopens it.
    last_answer: Option<AnswerView>,
}

impl App {
    pub fn new(review: Review, state: ReviewState, state_path: PathBuf, cfg: Config) -> Self {
        let sh = Shared::new(review, state, state_path);
        let diff = DiffView::new(&sh);
        let history = review::questions_path(&sh.review)
            .ok()
            .and_then(|p| ask::load_history(&p).ok())
            .unwrap_or_default();
        let llm: Llm = Arc::new(ClaudeCli::from_config(&cfg.llm));
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
            popup: None,
            llm,
            ask_pending: None,
            ask_rx: None,
            history,
            last_answer: None,
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
            Action::ShowLine { hunk, line } => {
                self.diff.reveal_line(&self.sh, hunk, line);
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
        if self.popup.is_some() {
            return self.on_popup_key(key);
        }
        let captured = match self.view {
            View::Diff => self.diff.captures_keys(),
            View::Stack => self.stack.captures_keys(),
            View::Graph => self.graph.captures_keys(),
        };
        if !captured && self.on_global_key(key) {
            return;
        }
        if !captured && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            match key.code {
                KeyCode::Char('a') => return self.ask_here(),
                KeyCode::Char('A') => return self.reopen_answer(),
                _ => {}
            }
        }
        let action = match self.view {
            View::Diff => self.diff.on_key(&mut self.sh, key),
            View::Stack => self.stack.on_key(&mut self.sh, key, self.cfg.llm.enabled),
            View::Graph => self.graph.on_key(&mut self.sh, key),
        };
        self.apply(action);
    }

    // ----- questions to the LLM -----------------------------------------

    /// `a`: asks about the node under the cursor of the current view.
    fn ask_here(&mut self) {
        if !self.cfg.llm.enabled {
            return self
                .sh
                .notify("the LLM is disabled (--no-llm): questions are unavailable");
        }
        if let Some(p) = &self.ask_pending {
            self.popup = Some(Popup::Pending(p.clone()));
            return;
        }
        let subject = match self.view {
            View::Diff => self.diff.ask_subject(&mut self.sh),
            View::Stack => self.stack.ask_subject(&mut self.sh),
            View::Graph => self.graph.ask_subject(&mut self.sh),
        };
        if let Some((subject, label)) = subject {
            self.popup = Some(Popup::AskInput(AskInput::new(subject, label)));
        }
    }

    /// Sends the question in the background; the answer shows when ready.
    pub fn submit_question(&mut self, input: &AskInput, question: &str) {
        let prompt = match review::ask_prompt(
            &self.sh.review,
            &self.cfg,
            self.sh.graph.as_ref(),
            self.stack.grouping.as_ref(),
            &input.subject,
            question,
        ) {
            Ok(p) => p,
            Err(e) => return self.sh.notify(format!("cannot ask: {e}")),
        };
        let params = match review::ask_params(&self.sh.review, &self.cfg, true) {
            Ok(p) => p,
            Err(e) => return self.sh.notify(format!("cannot ask: {e}")),
        };
        let pending = Pending {
            label: prompt.label.clone(),
            question: question.to_string(),
            since: Instant::now(),
        };
        let (tx, rx) = mpsc::channel();
        let llm = self.llm.clone();
        std::thread::spawn(move || {
            let res = ask::ask(&prompt, llm.as_ref(), &params).map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
        self.ask_rx = Some(rx);
        self.ask_pending = Some(pending.clone());
        self.popup = Some(Popup::Pending(pending));
    }

    /// Applies the answer of the background question, if it arrived.
    pub fn poll_ask(&mut self) {
        let Some(rx) = &self.ask_rx else {
            return;
        };
        let res = match rx.try_recv() {
            Ok(res) => res,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the question was lost".into()),
        };
        self.ask_rx = None;
        self.ask_pending = None;
        let waiting = matches!(self.popup, Some(Popup::Pending(_)));
        match res {
            Ok(answer) => {
                if let Ok(path) = review::questions_path(&self.sh.review) {
                    match ask::append_history(&path, &answer) {
                        Ok(all) => self.history = all,
                        Err(e) => self.sh.notify(format!("cannot save the question: {e}")),
                    }
                }
                let view = AnswerView::new(answer);
                if waiting {
                    self.popup = Some(Popup::Answer(view));
                } else {
                    self.last_answer = Some(view);
                    self.sh.notify("the answer is ready: A to read it");
                }
            }
            Err(e) => {
                if waiting {
                    self.popup = None;
                }
                self.sh.notify(format!("question failed: {e}"));
            }
        }
    }

    /// `A`: the last answer, else the history.
    fn reopen_answer(&mut self) {
        if let Some(p) = &self.ask_pending {
            self.popup = Some(Popup::Pending(p.clone()));
        } else if let Some(v) = self.last_answer.take() {
            self.popup = Some(Popup::Answer(v));
        } else if self.history.is_empty() {
            self.sh
                .notify("no question asked in this review yet (a on a node)");
        } else {
            self.popup = Some(Popup::History(HistoryView::default()));
        }
    }

    /// Where a reference of an answer leads: the Graph view of the symbol
    /// around it, or the Diff view (`diff`, or when no symbol is there).
    pub fn follow_ref(&mut self, r: &CodeRef, diff: bool) -> Action {
        let d = &self.sh.review.diff;
        let in_diff = d.files.iter().position(|f| match r.side {
            Side::New => f.path == r.path && f.status != FileStatus::Deleted,
            Side::Old => f.old_path.as_deref().unwrap_or(&f.path) == r.path,
        });
        let line = in_diff.and_then(|fi| {
            d.file_hunks(fi).find_map(|h| {
                let i = h.lines.iter().position(|l| match r.side {
                    Side::New => l.new_line == Some(r.line),
                    Side::Old => l.old_line == Some(r.line),
                })?;
                Some(Action::ShowLine {
                    hunk: h.id,
                    line: i,
                })
            })
        });
        let symbol = match (&self.sh.graph, r.side) {
            (Some(g), Side::New) => g
                .symbol_at(&r.path, r.line)
                .filter(|&s| g.symbol(s).kind != SymbolKind::File),
            _ => None,
        };
        match (diff, line, symbol) {
            (true, Some(a), _) | (false, Some(a), None) => a,
            (_, _, Some(s)) => Action::ShowSymbol(s),
            (_, None, None) => {
                self.sh.notify(format!(
                    "{}:{} is not in the diff nor in a known symbol: e opens it",
                    r.path, r.line
                ));
                Action::None
            }
        }
    }

    fn on_popup_key(&mut self, key: KeyEvent) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        match popup {
            Popup::AskInput(mut input) => match input.on_key(key) {
                InputOutcome::Continue => self.popup = Some(Popup::AskInput(input)),
                InputOutcome::Cancel => {}
                InputOutcome::Submit(q) => self.submit_question(&input, &q),
            },
            Popup::Pending(p) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self
                    .sh
                    .notify("still asking in the background: A to come back"),
                _ => self.popup = Some(Popup::Pending(p)),
            },
            Popup::Answer(mut v) => match v.on_key(key) {
                AnswerOutcome::Continue => self.popup = Some(Popup::Answer(v)),
                AnswerOutcome::Close => self.last_answer = Some(v),
                AnswerOutcome::History => {
                    self.last_answer = Some(v);
                    self.popup = Some(Popup::History(HistoryView::default()));
                }
                AnswerOutcome::Follow { r, diff } => {
                    let action = self.follow_ref(&r, diff);
                    if action == Action::None {
                        self.popup = Some(Popup::Answer(v));
                    } else {
                        self.last_answer = Some(v);
                        self.sh.notify("A: back to the answer");
                        self.apply(action);
                    }
                }
                AnswerOutcome::Edit(r) => {
                    if r.side == Side::Old {
                        self.sh.notify(
                            "this line only exists in the base revision: d shows it in the diff",
                        );
                    } else {
                        self.sh.open_path(&r.path, r.line);
                    }
                    self.popup = Some(Popup::Answer(v));
                }
            },
            Popup::History(mut h) => match h.on_key(key, self.history.len()) {
                HistoryOutcome::Continue => self.popup = Some(Popup::History(h)),
                HistoryOutcome::Close => {}
                HistoryOutcome::Open(i) => {
                    self.popup = Some(Popup::Answer(AnswerView::new(self.history[i].clone())))
                }
            },
        }
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

    struct FakeLlm(&'static str);

    impl LlmProvider for FakeLlm {
        fn complete(&self, req: &survol_core::llm::LlmRequest) -> survol_core::llm::Result<String> {
            assert!(req.prompt.contains("## Subject: group 1 of 3"));
            Ok(self.0.to_string())
        }
    }

    fn wait_answer(app: &mut App) {
        for _ in 0..200 {
            app.poll_ask();
            if app.ask_pending.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no answer");
    }

    #[test]
    fn asks_about_a_group_and_follows_the_links() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        let (review, grouping) = fixture(dir.path());
        let mut app = App::new(
            review,
            ReviewState::default(),
            dir.path().join("state.json"),
            Config::default(),
        );
        app.llm = Arc::new(FakeLlm(
            "It changes the model [src/a.rs:1] and the API [src/a.rs:10], not [src/zz.rs:4].",
        ));
        app.stack.set_grouping(&app.sh, grouping);
        app.on_key(key('2'));
        app.on_key(key('a'));
        let Some(Popup::AskInput(input)) = &app.popup else {
            panic!("no question input");
        };
        assert_eq!(input.subject, ask::Subject::Group(0));
        // `1`: the first suggestion.
        app.on_key(key('1'));
        assert!(matches!(app.popup, Some(Popup::Pending(_))));
        wait_answer(&mut app);
        let Some(Popup::Answer(v)) = &app.popup else {
            panic!("no answer shown");
        };
        assert_eq!(v.answer.refs.len(), 3);
        assert_eq!(v.answer.unknown_refs(), 1);
        assert_eq!(app.history.len(), 1);
        assert!(review::questions_path(&app.sh.review).unwrap().exists());

        // Tab to the second link, Enter: the Diff view on that line.
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.popup.is_none());
        assert_eq!(app.view, View::Diff);
        assert_eq!(
            app.diff.rows[app.diff.pos.cursor],
            Row::Line { hunk: 1, line: 1 }
        );
        // A: back to the answer; Esc, then A again; history after that.
        app.on_key(key('A'));
        assert!(matches!(app.popup, Some(Popup::Answer(_))));
        app.on_key(key('A'));
        assert!(matches!(app.popup, Some(Popup::History(_))));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popup, Some(Popup::Answer(_))));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.popup.is_none());

        // Same question again: from the cache.
        app.on_key(key('2'));
        app.on_key(key('a'));
        app.on_key(key('1'));
        wait_answer(&mut app);
        let Some(Popup::Answer(v)) = &app.popup else {
            panic!("no answer shown");
        };
        assert!(v.answer.from_cache);
        assert_eq!(app.history.len(), 1);

        // Without LLM: a message, no popup.
        app.popup = None;
        app.cfg.llm.enabled = false;
        app.on_key(key('a'));
        assert!(app.popup.is_none());
        assert!(app.sh.message().unwrap().contains("--no-llm"));
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
