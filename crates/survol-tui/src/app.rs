//! Application state: what the views share, view switching, global keys and
//! the background grouping. Each view lives in `views/`, rendering in `ui/`.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::ask::{self, Answer, CodeRef};
use survol_core::comments::{self, Anchor, CommentStore, Placement};
use survol_core::config::Config;
use survol_core::forge::Discussion;
use survol_core::graph::{Graph, SymIdx, SymbolKind};
use survol_core::group::{Grouping, Source};
use survol_core::llm::{ClaudeCli, LlmProvider};
use survol_core::model::{FileStatus, LineKind, Side};
use survol_core::review::{self, RemoteReview, Review};
use survol_core::review_state::ReviewState;

use crate::editor;
use crate::highlight::Highlighter;
use crate::views::Layout;
use crate::views::ask::{
    AnswerOutcome, AnswerView, AskInput, HistoryOutcome, HistoryView, InputOutcome, Pending,
};
use crate::views::comments::{
    CommentTarget, Confirm, Editor, EditorOutcome, NoteIndex, NoteKind, PanelOutcome, PanelRow,
    ReviewPanel, ThreadOutcome, ThreadView, panel_rows,
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
    /// Write or edit a comment.
    Comment(CommentTarget),
    /// Show the whole thread of a note.
    Thread(NoteKind),
}

/// A modal window over the views.
pub enum Popup {
    AskInput(AskInput),
    /// Waiting for the LLM.
    Pending(Pending),
    Answer(AnswerView),
    History(HistoryView),
    /// Writing a comment.
    Comment(Editor),
    /// The Review panel ([`App::panel`]).
    Review,
    /// The whole thread of a note.
    Thread(ThreadView),
}

/// The merge request's side of the review.
pub enum RemoteStatus {
    /// A local range: nothing to fetch, drafts stay local.
    Local,
    Fetching(Instant),
    Ready,
    Failed(String),
}

enum PublishEvent {
    Progress(String),
    Done(Result<usize, String>),
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
    /// Local draft comments.
    pub comments: CommentStore,
    comments_path: PathBuf,
    /// Instance capabilities and discussions of the merge request, once fetched.
    pub remote: Option<RemoteReview>,
    /// Drafts and discussions placed in the diff.
    pub notes: NoteIndex,
}

impl Shared {
    pub fn new(review: Review, state: ReviewState, state_path: PathBuf) -> Self {
        let worktree_ready = review.worktree == review.repo.dir();
        let comments_path = state_path.with_file_name("comments.json");
        let (comments, message) = match CommentStore::load(&comments_path) {
            Ok(c) => (c, None),
            Err(e) => (
                CommentStore::default(),
                Some((
                    format!("cannot read the draft comments: {e}"),
                    Instant::now(),
                )),
            ),
        };
        let notes = NoteIndex::build(&review.diff, &comments, &[]);
        Self {
            review,
            state,
            state_path,
            highlighter: Highlighter::new(),
            layout: Layout::Unified,
            worktree_ready,
            graph: None,
            needs_clear: false,
            message,
            comments,
            comments_path,
            remote: None,
            notes,
        }
    }

    /// `o` on a note: shows it whole, or folds it back.
    pub fn toggle_note(&mut self, note: u32) {
        self.notes.toggle_fold(note, &self.highlighter);
    }

    pub fn discussions(&self) -> &[Discussion] {
        self.remote
            .as_ref()
            .map_or(&[], |r| r.discussions.as_slice())
    }

    /// Places drafts and discussions again (views must relayout after).
    pub fn rebuild_notes(&mut self) {
        let discussions = self.remote.as_ref().map_or(&[][..], |r| &r.discussions);
        let mut notes = NoteIndex::build(&self.review.diff, &self.comments, discussions);
        notes.expanded = std::mem::take(&mut self.notes.expanded);
        notes.set_width(self.notes.width(), &self.highlighter);
        notes.generation = self.notes.generation + 1;
        self.notes = notes;
    }

    /// Writes the draft comments; called after every change.
    pub fn save_comments(&mut self) {
        if let Err(e) = self.comments.save(&self.comments_path) {
            self.notify(format!("cannot save the draft comments: {e}"));
        }
        self.rebuild_notes();
    }

    /// Reads the draft comments again (after a publication in the background).
    pub fn reload_comments(&mut self) {
        match CommentStore::load(&self.comments_path) {
            Ok(c) => self.comments = c,
            Err(e) => self.notify(format!("cannot read the draft comments: {e}")),
        }
        self.rebuild_notes();
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

enum LspEvent {
    Progress(String),
    Done(Box<Result<Option<Graph>, String>>),
}

/// State of the background refinement of the graph by language servers.
pub enum LspStatus {
    /// Waiting for the graph and the worktree (or disabled).
    NotStarted,
    Running(String),
    /// Refined (`true`: a server answered), or nothing to refine.
    Done(bool),
    Failed,
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
    pub lsp_status: LspStatus,
    lsp_rx: Option<mpsc::Receiver<LspEvent>>,
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

    /// The Review panel (`P`), kept between openings.
    pub panel: ReviewPanel,
    pub remote_status: RemoteStatus,
    remote_rx: Option<mpsc::Receiver<Result<Option<RemoteReview>, String>>>,
    /// Progress of a publication running in the background.
    pub publishing: Option<String>,
    publish_rx: Option<mpsc::Receiver<PublishEvent>>,
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
            lsp_status: LspStatus::NotStarted,
            lsp_rx: None,
            help: false,
            quit: false,
            popup: None,
            llm,
            ask_pending: None,
            ask_rx: None,
            history,
            last_answer: None,
            panel: ReviewPanel::default(),
            remote_status: RemoteStatus::Local,
            remote_rx: None,
            publishing: None,
            publish_rx: None,
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

    /// The heuristic graph is ready and the worktree checked out: refine the
    /// graph with language servers in the background (once).
    pub fn maybe_start_lsp(&mut self) {
        if !matches!(self.lsp_status, LspStatus::NotStarted)
            || !self.cfg.lsp.enabled
            || !self.sh.worktree_ready
            || !matches!(self.graph_status, GraphStatus::Done)
        {
            return;
        }
        let Some(graph) = self.sh.graph.clone() else {
            return;
        };
        if graph.lsp_stats().is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let review = self.sh.review.clone();
        let cfg = self.cfg.clone();
        std::thread::spawn(move || {
            let progress = |m: &str| {
                let _ = tx.send(LspEvent::Progress(m.to_string()));
            };
            let res = review::refine_graph(&review, &cfg, &graph, &progress, true);
            let _ = tx.send(LspEvent::Done(Box::new(res.map_err(|e| e.to_string()))));
        });
        self.lsp_rx = Some(rx);
        self.lsp_status = LspStatus::Running("LSP: starting".into());
    }

    /// Applies what the refinement thread reported: progress, then the
    /// refined graph swapped in place of the heuristic one.
    pub fn poll_lsp(&mut self) {
        let Some(rx) = &self.lsp_rx else {
            return;
        };
        let mut done = None;
        for ev in rx.try_iter() {
            match ev {
                LspEvent::Progress(p) => self.lsp_status = LspStatus::Running(p),
                LspEvent::Done(res) => done = Some(*res),
            }
        }
        let Some(res) = done else {
            return;
        };
        self.lsp_rx = None;
        match res {
            Ok(Some(g)) => {
                let summary = g.lsp_stats().map(|s| s.summary()).unwrap_or_default();
                let changed = g
                    .lsp_stats()
                    .is_some_and(|s| s.confirmed + s.removed + s.added > 0);
                self.lsp_status = LspStatus::Done(g.lsp_stats().is_some_and(|s| s.any_ready()));
                if changed {
                    self.set_graph(g);
                }
                self.sh.notify(summary);
            }
            Ok(None) => self.lsp_status = LspStatus::Done(true),
            Err(e) => {
                self.sh.notify(format!("LSP refinement failed: {e}"));
                self.lsp_status = LspStatus::Failed;
            }
        }
    }

    /// Short description of the refinement for the header.
    fn lsp_label(&self) -> String {
        match &self.lsp_status {
            LspStatus::NotStarted | LspStatus::Failed => String::new(),
            LspStatus::Running(p) => {
                // While a server starts, its status says more than `0/n`.
                let p = match p.split_once(" · ") {
                    Some((count, waiting)) if count.contains(" 0/") => {
                        format!("LSP: {waiting}")
                    }
                    Some((count, _)) => count.to_string(),
                    None => p.clone(),
                };
                let p: String = p.chars().take(48).collect();
                format!(" · ⟳ {p}")
            }
            LspStatus::Done(false) => " · LSP ✗".into(),
            LspStatus::Done(true) => match self.sh.graph.as_ref().and_then(|g| g.lsp_stats()) {
                Some(s) => format!(" · LSP ✓{}", s.confirmed + s.added),
                None => String::new(),
            },
        }
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
                    format!("graph: {n} changed symbols{cached}{}", self.lsp_label())
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
            Action::Comment(t) => self.open_editor(t, false),
            Action::Thread(kind) => self.open_thread(kind, false),
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
                KeyCode::Char('P') => {
                    self.popup = Some(Popup::Review);
                    return;
                }
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
            Popup::Comment(mut e) => match e.on_key(key) {
                EditorOutcome::Continue => self.popup = Some(Popup::Comment(e)),
                EditorOutcome::Cancel => self.editor_closed(&e),
                EditorOutcome::Save(text) => {
                    self.save_comment(e.target, &text);
                    self.editor_closed(&e);
                }
            },
            Popup::Thread(mut t) => match t.on_key(key) {
                ThreadOutcome::Continue => self.popup = Some(Popup::Thread(t)),
                ThreadOutcome::Close => {
                    if t.from_panel {
                        self.popup = Some(Popup::Review);
                    }
                }
                ThreadOutcome::Write(target) => {
                    self.open_editor(target, false);
                    match &mut self.popup {
                        Some(Popup::Comment(e)) => e.from_thread = Some(t.kind),
                        _ => self.popup = Some(Popup::Thread(t)),
                    }
                }
                ThreadOutcome::Next(forward) => {
                    let order = self.sh.notes.ordered(&self.sh.review.diff);
                    let cur = self
                        .sh
                        .notes
                        .find(t.kind)
                        .and_then(|i| order.iter().position(|&x| x == i));
                    let next = match (cur, forward) {
                        (_, _) if order.is_empty() => None,
                        (None, _) => Some(0),
                        (Some(i), true) => Some((i + 1) % order.len()),
                        (Some(i), false) => Some((i + order.len() - 1) % order.len()),
                    };
                    match next {
                        Some(i) => {
                            let mut v =
                                ThreadView::new(self.sh.notes.items[order[i] as usize].kind);
                            v.from_panel = t.from_panel;
                            self.popup = Some(Popup::Thread(v));
                        }
                        None => self.popup = Some(Popup::Thread(t)),
                    }
                }
            },
            Popup::Review => self.on_panel_key(key),
            Popup::History(mut h) => match h.on_key(key, self.history.len()) {
                HistoryOutcome::Continue => self.popup = Some(Popup::History(h)),
                HistoryOutcome::Close => {}
                HistoryOutcome::Open(i) => {
                    self.popup = Some(Popup::Answer(AnswerView::new(self.history[i].clone())))
                }
            },
        }
    }

    // ----- comments ------------------------------------------------------

    /// Shows the whole thread of a note; a reply drafted to a discussion
    /// shows the discussion.
    pub fn open_thread(&mut self, kind: NoteKind, from_panel: bool) {
        let kind = match kind {
            NoteKind::Draft(id) => match self.sh.comments.get(id).map(|d| &d.anchor) {
                Some(Anchor::Reply { discussion, .. }) => self
                    .sh
                    .discussions()
                    .iter()
                    .position(|d| &d.id == discussion)
                    .map_or(kind, NoteKind::Remote),
                _ => kind,
            },
            k => k,
        };
        let mut v = ThreadView::new(kind);
        v.from_panel = from_panel;
        self.popup = Some(Popup::Thread(v));
    }

    /// Where to go back once the editor is closed.
    fn editor_closed(&mut self, e: &Editor) {
        if e.from_panel {
            self.popup = Some(Popup::Review);
        } else if let Some(kind) = e.from_thread {
            // An emptied draft is gone: nothing to go back to.
            let gone = matches!(kind, NoteKind::Draft(id) if self.sh.comments.get(id).is_none());
            if !gone {
                self.popup = Some(Popup::Thread(ThreadView::new(kind)));
            }
        }
    }

    /// Fetches the instance version and the discussions of the merge request
    /// in the background (nothing for a local range).
    pub fn start_remote(&mut self) {
        if self.sh.review.mr.is_none() {
            self.remote_status = RemoteStatus::Local;
            return;
        }
        let (tx, rx) = mpsc::channel();
        let review = self.sh.review.clone();
        let cfg = self.cfg.clone();
        std::thread::spawn(move || {
            let _ = tx.send(review::fetch_remote(&review, &cfg).map_err(|e| e.to_string()));
        });
        self.remote_rx = Some(rx);
        self.remote_status = RemoteStatus::Fetching(Instant::now());
    }

    pub fn poll_remote(&mut self) {
        let Some(rx) = &self.remote_rx else {
            return;
        };
        let res = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the request was lost".into()),
        };
        self.remote_rx = None;
        match res {
            Ok(remote) => self.set_remote(remote),
            Err(e) => {
                self.sh.notify(format!("GitLab discussions: {e}"));
                self.remote_status = RemoteStatus::Failed(e);
            }
        }
    }

    /// Installs what the forge said about the review.
    pub fn set_remote(&mut self, remote: Option<RemoteReview>) {
        self.remote_status = if remote.is_some() {
            RemoteStatus::Ready
        } else {
            RemoteStatus::Local
        };
        self.sh.remote = remote;
        self.refresh_notes();
    }

    /// After a change of drafts or discussions: place them again and redraw.
    fn refresh_notes(&mut self) {
        self.sh.rebuild_notes();
        self.diff.relayout(&self.sh);
        self.stack.relayout(&self.sh);
    }

    /// Opens the comment editor on `target`.
    pub fn open_editor(&mut self, target: CommentTarget, from_panel: bool) {
        if self.publishing.is_some() {
            return self
                .sh
                .notify("publishing in progress: the drafts are locked until it ends");
        }
        let d = &self.sh.review.diff;
        let code = |hunk: usize, from: usize, to: usize| -> Vec<String> {
            let lines = &d.hunks[hunk].lines;
            let from = from.max(to.saturating_sub(5));
            (from..=to)
                .map(|i| {
                    let l = &lines[i];
                    let sign = match l.kind {
                        LineKind::Added => '+',
                        LineKind::Removed => '-',
                        LineKind::Context => ' ',
                    };
                    format!("{sign}{}", l.text)
                })
                .collect()
        };
        let (title, context, text) = match target {
            CommentTarget::Line { hunk, line, start } => {
                let p = Placement::Line {
                    file: d.hunks[hunk].file,
                    hunk,
                    line,
                    start,
                    moved: false,
                };
                (
                    format!("comment on {}", comments::describe(d, p)),
                    code(hunk, start.unwrap_or(line), line),
                    String::new(),
                )
            }
            CommentTarget::File(f) => (
                format!("comment on the file {}", d.files[f].display_path()),
                Vec::new(),
                String::new(),
            ),
            CommentTarget::Draft(id) => {
                let Some(draft) = self.sh.comments.get(id) else {
                    return;
                };
                let p = comments::place(&draft.anchor, d);
                let context = match p {
                    Placement::Line {
                        hunk, line, start, ..
                    } => code(hunk, start.unwrap_or(line), line),
                    _ => Vec::new(),
                };
                (
                    format!("edit draft · {}", comments::describe(d, p)),
                    context,
                    draft.body.clone(),
                )
            }
            CommentTarget::Reply(di) => {
                let Some(disc) = self.sh.discussions().get(di) else {
                    return;
                };
                let first = &disc.notes[0];
                (
                    format!("reply to @{}", first.author.username),
                    first.body.lines().take(4).map(str::to_string).collect(),
                    String::new(),
                )
            }
            CommentTarget::Summary => (
                "overall comment of the review".to_string(),
                Vec::new(),
                self.sh.comments.summary.clone(),
            ),
        };
        let mut e = Editor::new(target, title, context, text);
        e.from_panel = from_panel;
        self.popup = Some(Popup::Comment(e));
    }

    /// Saves what the editor wrote for `target`.
    pub fn save_comment(&mut self, target: CommentTarget, text: &str) {
        let d = &self.sh.review.diff;
        let empty = text.trim().is_empty();
        let anchor = match target {
            CommentTarget::Line { hunk, line, start } => Some(Anchor::Line {
                start: start.map(|s| comments::line_anchor(d, hunk, s)),
                line: comments::line_anchor(d, hunk, line),
            }),
            CommentTarget::File(f) => Some(comments::file_anchor(d, f)),
            CommentTarget::Reply(di) => self.sh.discussions().get(di).map(|x| Anchor::Reply {
                discussion: x.id.clone(),
                author: x.notes[0].author.username.clone(),
            }),
            CommentTarget::Draft(id) => {
                self.sh.comments.update(id, text);
                if empty {
                    self.sh.notify("empty draft deleted");
                }
                None
            }
            CommentTarget::Summary => {
                self.sh.comments.summary = text.trim_end().to_string();
                None
            }
        };
        if let Some(anchor) = anchor {
            if empty {
                return self.sh.notify("empty comment: nothing saved");
            }
            self.sh.comments.add(anchor, text);
            let n = self.sh.comments.drafts.len();
            self.sh.notify(format!(
                "draft saved ({n} in this review): P to review and publish"
            ));
        }
        self.sh.save_comments();
        self.diff.relayout(&self.sh);
        self.stack.relayout(&self.sh);
    }

    fn on_panel_key(&mut self, key: KeyEvent) {
        let rows = panel_rows(
            &self.sh.review.diff,
            &self.sh.comments,
            self.sh.discussions(),
        );
        self.popup = Some(Popup::Review);
        match self.panel.on_key(key, &rows) {
            PanelOutcome::Continue => {}
            PanelOutcome::Close => self.popup = None,
            PanelOutcome::Edit(t) => self.open_editor(t, true),
            PanelOutcome::Jump(row) => {
                let action = self.panel_jump(row);
                if action != Action::None {
                    self.popup = None;
                    self.apply(action);
                }
            }
            PanelOutcome::Delete(id) => {
                self.sh.comments.remove(id);
                self.sh.save_comments();
                self.diff.relayout(&self.sh);
                self.stack.relayout(&self.sh);
                self.sh.notify("draft deleted");
            }
            PanelOutcome::AskPublish => self.prepare_publish(),
            PanelOutcome::Publish(plan) => self.start_publish(plan),
            PanelOutcome::Refresh => self.start_remote(),
            PanelOutcome::Thread(kind) => self.open_thread(kind, true),
        }
    }

    /// Where a row of the Review panel is in the diff.
    fn panel_jump(&mut self, row: PanelRow) -> Action {
        let d = &self.sh.review.diff;
        let at_discussion = |disc: &Discussion| {
            let pos = disc.position()?;
            match comments::place_position(pos, d)? {
                (_, Some((hunk, line))) => Some(Action::ShowLine { hunk, line }),
                (file, None) => Some(Action::ShowFile(file)),
            }
        };
        let action = match row {
            PanelRow::Summary => None,
            PanelRow::Draft(id) => match self.sh.comments.get(id) {
                Some(draft) => match (&draft.anchor, comments::place(&draft.anchor, d)) {
                    (_, Placement::Line { hunk, line, .. }) => {
                        Some(Action::ShowLine { hunk, line })
                    }
                    (_, Placement::File { file }) => Some(Action::ShowFile(file)),
                    (Anchor::Reply { discussion, .. }, _) => self
                        .sh
                        .discussions()
                        .iter()
                        .find(|x| &x.id == discussion)
                        .and_then(at_discussion),
                    _ => None,
                },
                None => None,
            },
            PanelRow::Discussion(i) => self.sh.discussions().get(i).and_then(at_discussion),
        };
        action.unwrap_or_else(|| {
            self.sh.notify("not in the diff shown here");
            Action::None
        })
    }

    /// `p` in the Review panel: what would be sent, to confirm.
    fn prepare_publish(&mut self) {
        if self.sh.review.mr.is_none() {
            return self.sh.notify(
                "local range: drafts stay local (publishing needs a merge request; survol-cli publish --dry-run shows the requests)",
            );
        }
        if self.publishing.is_some() {
            return self.sh.notify("already publishing");
        }
        let caps = match (&self.remote_status, &self.sh.remote) {
            (RemoteStatus::Ready, Some(r)) => r.capabilities.clone(),
            (RemoteStatus::Fetching(_), _) => {
                return self
                    .sh
                    .notify("still asking GitLab for its version and discussions…");
            }
            (RemoteStatus::Failed(e), _) => {
                return self
                    .sh
                    .notify(format!("cannot reach GitLab ({e}): r to retry"));
            }
            _ => return self.sh.notify("GitLab is not reachable: r to retry"),
        };
        let plan = comments::plan(
            &self.sh.comments,
            &self.sh.review.diff,
            &review::shas(&self.sh.review),
            &caps,
        );
        if plan.comments.is_empty() {
            let stale = plan.skipped.len();
            return self.sh.notify(if stale > 0 {
                format!("nothing to publish: {stale} stale draft(s) only")
            } else {
                "nothing to publish: write comments with c / C, a summary with S".to_string()
            });
        }
        self.panel.confirm = Some(Confirm::Publish {
            plan,
            json: false,
            scroll: 0,
        });
    }

    /// Publishes in the background; drafts are locked meanwhile.
    fn start_publish(&mut self, plan: comments::Plan) {
        let (tx, rx) = mpsc::channel();
        let review = self.sh.review.clone();
        let cfg = self.cfg.clone();
        let mut store = self.sh.comments.clone();
        let path = self.sh.comments_path.clone();
        std::thread::spawn(move || {
            let mut progress = |m: &str| {
                let _ = tx.send(PublishEvent::Progress(m.to_string()));
            };
            let res = review::publish(&review, &cfg, &plan, &mut store, &path, &mut progress)
                .map_err(|e| e.to_string());
            let _ = tx.send(PublishEvent::Done(res));
        });
        self.publish_rx = Some(rx);
        self.publishing = Some("starting".into());
    }

    pub fn poll_publish(&mut self) {
        let Some(rx) = &self.publish_rx else {
            return;
        };
        let mut done = None;
        for ev in rx.try_iter() {
            match ev {
                PublishEvent::Progress(p) => self.publishing = Some(p),
                PublishEvent::Done(r) => done = Some(r),
            }
        }
        let Some(res) = done else {
            return;
        };
        self.publish_rx = None;
        self.publishing = None;
        self.sh.reload_comments();
        match res {
            Ok(n) => self
                .sh
                .notify(format!("review published: {n} comment(s) on GitLab")),
            Err(e) => self.sh.notify(format!(
                "publication failed: {e} (p retries, without duplicates)"
            )),
        }
        // The published comments come back as discussions.
        self.start_remote();
        self.diff.relayout(&self.sh);
        self.stack.relayout(&self.sh);
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
        // Language servers: status while they work, then the outcome.
        app.lsp_status = LspStatus::Running("LSP: refining 0/12 · java indexing… Importing".into());
        assert!(
            app.graph_label()
                .ends_with(" · ⟳ LSP: java indexing… Importing")
        );
        app.lsp_status = LspStatus::Running("LSP: refining 3/12".into());
        assert!(app.graph_label().ends_with(" · ⟳ LSP: refining 3/12"));
        app.lsp_status = LspStatus::Done(false);
        assert!(app.graph_label().ends_with(" · LSP ✗"));
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

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(c));
        }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn cursor_to(app: &mut App, row: Row) {
        app.diff.pos.cursor = app.diff.rows.iter().position(|r| *r == row).unwrap();
    }

    fn comment_rows(app: &App) -> Vec<Row> {
        app.diff
            .rows
            .iter()
            .copied()
            .filter(|r| matches!(r, Row::Comment { .. }))
            .collect()
    }

    #[test]
    fn comments_lines_ranges_and_files_in_the_diff_view() {
        let dir = tempfile::tempdir().unwrap();
        let (review, _) = fixture(dir.path());
        let mut app = App::new(
            review,
            ReviewState::default(),
            dir.path().join("state.json"),
            Config::default(),
        );
        // `c` on the added line of hunk 0.
        cursor_to(&mut app, Row::Line { hunk: 0, line: 1 });
        app.on_key(key('c'));
        let Some(Popup::Comment(e)) = &app.popup else {
            panic!("no editor");
        };
        assert_eq!(e.title, "comment on src/a.rs:1");
        assert_eq!(e.context, ["+b"]);
        type_text(&mut app, "Why b?");
        app.on_key(ctrl('s'));
        assert!(app.popup.is_none());
        assert_eq!(app.sh.comments.drafts.len(), 1);
        assert!(dir.path().join("comments.json").exists());
        // Shown under its line; the cursor stays on the line.
        let i = app
            .diff
            .rows
            .iter()
            .position(|r| *r == Row::Line { hunk: 0, line: 1 })
            .unwrap();
        assert!(matches!(
            app.diff.rows[i + 1],
            Row::Comment {
                hunk: Some(0),
                line: Some(1),
                part: 0,
                ..
            }
        ));
        assert_eq!(app.diff.pos.cursor, i);

        // V, k, c: a range over both lines of the hunk.
        app.on_key(key('V'));
        app.on_key(key('k'));
        app.on_key(key('c'));
        let Some(Popup::Comment(e)) = &app.popup else {
            panic!("no editor");
        };
        assert_eq!(e.title, "comment on src/a.rs:1 (old)-1");
        type_text(&mut app, "Range");
        app.on_key(ctrl('s'));
        assert_eq!(app.sh.comments.drafts.len(), 2);
        assert!(app.diff.visual.is_none());

        // C: the whole file.
        app.on_key(key('C'));
        type_text(&mut app, "File note");
        app.on_key(ctrl('s'));
        let f = app
            .diff
            .rows
            .iter()
            .position(|r| *r == Row::File(0))
            .unwrap();
        assert!(matches!(
            app.diff.rows[f + 1],
            Row::Comment { hunk: None, .. }
        ));

        // `c` on a draft edits it; emptying it deletes it.
        let first = comment_rows(&app)[0];
        cursor_to(&mut app, first);
        app.on_key(key('c'));
        let Some(Popup::Comment(e)) = &app.popup else {
            panic!("no editor");
        };
        assert!(e.title.starts_with("edit draft"));
        assert_eq!(e.text, "File note");
        for _ in 0..9 {
            app.on_key(code(KeyCode::Backspace));
        }
        app.on_key(ctrl('s'));
        assert_eq!(app.sh.comments.drafts.len(), 2);

        // A range across hunks is refused.
        cursor_to(&mut app, Row::Line { hunk: 0, line: 0 });
        app.on_key(key('V'));
        cursor_to(&mut app, Row::Line { hunk: 1, line: 0 });
        app.on_key(key('c'));
        assert!(app.popup.is_none());
        assert!(app.sh.message().unwrap().contains("one hunk"));

        // A local range cannot be published.
        app.on_key(key('P'));
        assert!(matches!(app.popup, Some(Popup::Review)));
        app.on_key(key('p'));
        assert!(app.sh.message().unwrap().contains("local range"));
        // Enter on the first draft jumps to it.
        app.on_key(key('j'));
        app.on_key(code(KeyCode::Enter));
        assert!(app.popup.is_none());
        assert_eq!(app.view, View::Diff);
    }

    fn remote(disc_line: Option<u32>, version: &str) -> RemoteReview {
        use survol_core::forge::{Author, Capabilities, Note, Position};
        let note = Note {
            id: 1,
            body: "Is this needed?".into(),
            author: Author {
                username: "alice".into(),
                name: "Alice".into(),
            },
            created_at: String::new(),
            system: false,
            resolvable: true,
            resolved: false,
            position: Some(Position {
                position_type: "text".into(),
                base_sha: "b".into(),
                start_sha: "s".into(),
                head_sha: "h".into(),
                old_path: "src/b.rs".into(),
                new_path: "src/b.rs".into(),
                old_line: None,
                new_line: disc_line,
                line_range: None,
            }),
        };
        RemoteReview {
            capabilities: Capabilities::from_version(version),
            discussions: vec![Discussion {
                id: "d1".into(),
                individual_note: false,
                notes: vec![note],
            }],
            pending_drafts: 2,
        }
    }

    #[test]
    fn discussions_inline_replies_and_publish_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let (mut review, _) = fixture(dir.path());
        review.mr = Some(survol_core::model::MergeRequest {
            project: "grp/app".into(),
            iid: 7,
            title: "Orders".into(),
            description: String::new(),
            source_branch: "feat".into(),
            target_branch: "main".into(),
            base_sha: "b".into(),
            start_sha: "s".into(),
            head_sha: "h".into(),
            web_url: String::new(),
        });
        let mut app = App::new(
            review,
            ReviewState::default(),
            dir.path().join("state.json"),
            Config::default(),
        );
        app.set_remote(Some(remote(Some(1), "17.3.0")));
        // The discussion shows under line 1 of src/b.rs (hunk 2, `+f`).
        let rows = comment_rows(&app);
        assert_eq!(rows.len(), 2, "head and body");
        assert!(matches!(
            rows[0],
            Row::Comment {
                hunk: Some(2),
                line: Some(1),
                ..
            }
        ));
        // `c` on it: a reply.
        cursor_to(&mut app, rows[0]);
        app.on_key(key('c'));
        let Some(Popup::Comment(e)) = &app.popup else {
            panic!("no editor");
        };
        assert_eq!(e.title, "reply to @alice");
        type_text(&mut app, "Yes, for the API.");
        app.on_key(ctrl('s'));
        assert!(matches!(
            &app.sh.comments.drafts[0].anchor,
            Anchor::Reply { discussion, .. } if discussion == "d1"
        ));
        assert_eq!(comment_rows(&app).len(), 4, "the reply draft follows");

        // Summary from the panel, then the publish confirmation.
        app.on_key(key('P'));
        app.on_key(key('S'));
        type_text(&mut app, "LGTM");
        app.on_key(ctrl('s'));
        assert!(matches!(app.popup, Some(Popup::Review)));
        assert_eq!(app.sh.comments.summary, "LGTM");
        app.on_key(key('p'));
        let Some(Confirm::Publish { plan, .. }) = &app.panel.confirm else {
            panic!("no confirmation: {:?}", app.sh.message());
        };
        assert_eq!(plan.mode, comments::Mode::Drafts);
        let reqs = plan.requests("grp%2Fapp", "7");
        assert_eq!(reqs.len(), 3);
        assert_eq!(
            reqs[0].body.as_ref().unwrap()["in_reply_to_discussion_id"],
            "d1"
        );
        // `n` cancels: nothing sent, still in the panel.
        app.on_key(key('n'));
        assert!(app.panel.confirm.is_none());
        assert!(app.publishing.is_none());
        assert!(matches!(app.popup, Some(Popup::Review)));

        // An old instance: direct mode.
        app.set_remote(Some(remote(Some(1), "15.2.0")));
        app.on_key(key('p'));
        let Some(Confirm::Publish { plan, .. }) = &app.panel.confirm else {
            panic!("no confirmation");
        };
        assert_eq!(plan.mode, comments::Mode::Direct);
    }

    #[test]
    fn notes_fold_and_open_their_thread() {
        let dir = tempfile::tempdir().unwrap();
        let (review, _) = fixture(dir.path());
        let mut app = App::new(
            review,
            ReviewState::default(),
            dir.path().join("state.json"),
            Config::default(),
        );
        let mut r = remote(Some(1), "17.3.0");
        r.discussions[0].notes[0].body =
            "**Why** this?\n\n1. one\n2. two\n3. three\n4. four\n5. five".into();
        app.set_remote(Some(r));
        // Head, 4 lines, then the fold indicator.
        let rows = comment_rows(&app);
        assert_eq!(rows.len(), 6);
        cursor_to(&mut app, rows[2]);
        app.on_key(key('o'));
        let rows = comment_rows(&app);
        assert_eq!(rows.len(), 9, "whole: head, 7 lines, fold hint");
        assert!(matches!(
            app.diff.rows[app.diff.pos.cursor],
            Row::Comment { part: 0, .. }
        ));
        // za folds it back.
        app.on_key(key('z'));
        app.on_key(key('a'));
        assert_eq!(comment_rows(&app).len(), 6);

        // Enter: the thread; c replies, then back to the thread.
        crate::ui::tests::screen(&mut app, 100, 24);
        app.on_key(code(KeyCode::Enter));
        let Some(Popup::Thread(t)) = &app.popup else {
            panic!("no thread");
        };
        assert_eq!(t.kind, NoteKind::Remote(0));
        let screen = crate::ui::tests::screen(&mut app, 100, 30).join("\n");
        assert!(screen.contains("thread · src/b.rs:1"), "{screen}");
        assert!(screen.contains("5. five"), "{screen}");
        assert!(screen.contains("▶"), "the code line: {screen}");
        app.on_key(key('c'));
        let Some(Popup::Comment(e)) = &app.popup else {
            panic!("no editor");
        };
        assert_eq!(e.title, "reply to @alice");
        type_text(&mut app, "Done.");
        app.on_key(ctrl('s'));
        assert!(matches!(app.popup, Some(Popup::Thread(_))));
        let screen = crate::ui::tests::screen(&mut app, 100, 30).join("\n");
        assert!(screen.contains("draft reply"), "{screen}");
        // n wraps around the notes of the diff: the reply draft, then back.
        app.on_key(key('n'));
        app.on_key(code(KeyCode::Esc));
        assert!(app.popup.is_none());
        // From the Review panel, t: the thread, Esc back to the panel.
        app.on_key(key('P'));
        app.on_key(key('G'));
        app.on_key(key('t'));
        assert!(matches!(app.popup, Some(Popup::Thread(_))));
        app.on_key(code(KeyCode::Esc));
        assert!(matches!(app.popup, Some(Popup::Review)));
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
