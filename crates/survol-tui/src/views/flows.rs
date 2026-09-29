//! Flows mode of the Graph view: the entry points the review impacts on the
//! left (grouped by kind), the flow of the selected one on the right as an
//! indented tree, after the change, before it, or both merged (`b`).
//!
//! The head flows are computed from the graph when the mode is first shown;
//! the base revision's graph is built on a background thread, then every
//! flow is compared ([`survol_core::flows::compare`]). Rendering lives in
//! `ui/flows.rs`.

use std::sync::mpsc;
use std::time::Instant;

use survol_core::config::Config;
use survol_core::flows::{self, Change, EntryKind, ImpactedFlow, Limits, Step};
use survol_core::graph::{Graph, SymIdx};

use super::Scroll;
use super::export::{self, Diagram};
use crate::app::Shared;

/// Which flow the right pane shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    After,
    Before,
    /// Both merged: added and removed steps marked.
    Diff,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::After => "after",
            Side::Before => "before",
            Side::Diff => "before → after",
        }
    }
}

/// The base revision's graph, for the before / after.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum BaseStatus {
    #[default]
    NotStarted,
    Running(Instant),
    Done,
    Failed(String),
}

/// A line of the entry list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryRow {
    Kind(EntryKind),
    /// Index into [`FlowsState::flows`].
    Flow(usize),
}

/// A line of the flow pane.
pub struct FlowLine<'a> {
    pub step: &'a Step,
    /// In the merged view only.
    pub change: Option<Change>,
    pub rerouted: bool,
}

#[derive(Default)]
pub struct FlowsState {
    pub flows: Vec<ImpactedFlow>,
    pub computed: bool,
    pub rows: Vec<EntryRow>,
    /// Entry list.
    pub pos: Scroll,
    /// Flow pane.
    pub step_pos: Scroll,
    pub side: Side,
    pub base: BaseStatus,
    rx: Option<mpsc::Receiver<Result<Vec<ImpactedFlow>, String>>>,
}

impl FlowsState {
    /// Computes the head flows once the graph is there, starts the base
    /// graph, and applies it when ready. Called on each frame of the mode.
    pub fn tick(&mut self, sh: &mut Shared, cfg: &Config) {
        let Some(g) = &sh.graph else {
            return;
        };
        if !self.computed {
            self.flows = flows::impacted(g, &Limits::default());
            self.computed = true;
            self.rebuild_rows(None);
        }
        if self.base == BaseStatus::NotStarted && !self.flows.is_empty() {
            self.start_base(sh, cfg);
        }
        let Some(rx) = &self.rx else {
            return;
        };
        let res = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the base graph was lost".into()),
        };
        self.rx = None;
        match res {
            Ok(compared) => {
                let keep = self.selected().map(|f| f.entry.id.clone());
                let n = compared
                    .iter()
                    .filter(|f| f.diff.as_ref().is_some_and(|d| d.is_relevant()))
                    .count();
                self.flows = compared;
                self.base = BaseStatus::Done;
                self.rebuild_rows(keep.as_deref());
                sh.notify(format!(
                    "before / after ready: {n} flow(s) differ (b to switch)"
                ));
            }
            Err(e) => {
                sh.notify(format!("base graph failed: {e}"));
                self.base = BaseStatus::Failed(e);
            }
        }
    }

    fn start_base(&mut self, sh: &Shared, cfg: &Config) {
        let (tx, rx) = mpsc::channel();
        let review = sh.review.clone();
        let cfg = cfg.clone();
        let head = sh.graph.clone().expect("graph checked");
        let mut current = self.flows.clone();
        std::thread::spawn(move || {
            let res = flows::base_graph(&review, &cfg, &head, &current, |_| {}, true)
                .map(|base| {
                    flows::compare(&mut current, &head, &base, &Limits::default());
                    current
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
        self.rx = Some(rx);
        self.base = BaseStatus::Running(Instant::now());
    }

    fn rebuild_rows(&mut self, keep: Option<&str>) {
        self.rows.clear();
        let mut last = None;
        for (i, f) in self.flows.iter().enumerate() {
            if last != Some(f.entry.kind) {
                self.rows.push(EntryRow::Kind(f.entry.kind));
                last = Some(f.entry.kind);
            }
            self.rows.push(EntryRow::Flow(i));
        }
        let at = keep
            .and_then(|id| {
                self.rows
                    .iter()
                    .position(|r| matches!(r, EntryRow::Flow(i) if self.flows[*i].entry.id == id))
            })
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|r| matches!(r, EntryRow::Flow(_)))
            });
        self.pos.cursor = at.unwrap_or(0);
        self.pos.clamp(self.rows.len());
    }

    pub fn selected(&self) -> Option<&ImpactedFlow> {
        match self.rows.get(self.pos.cursor)? {
            EntryRow::Flow(i) => self.flows.get(*i),
            EntryRow::Kind(_) => None,
        }
    }

    /// The side actually shown: `after` falls back to `before` for an entry
    /// gone in the head.
    pub fn shown_side(&self) -> Side {
        match (self.side, self.selected()) {
            (Side::After, Some(f)) if f.after.is_none() => Side::Before,
            (s, _) => s,
        }
    }

    /// Lines of the flow pane for the selected entry.
    pub fn lines(&self) -> Vec<FlowLine<'_>> {
        let Some(f) = self.selected() else {
            return Vec::new();
        };
        match self.shown_side() {
            Side::After => f
                .after
                .as_ref()
                .map(|a| plain(&a.steps))
                .unwrap_or_default(),
            Side::Before => f
                .before
                .as_ref()
                .map(|b| plain(&b.steps))
                .unwrap_or_default(),
            Side::Diff => match &f.diff {
                Some(d) => d
                    .steps
                    .iter()
                    .map(|s| FlowLine {
                        step: &s.step,
                        change: Some(s.change),
                        rerouted: s.rerouted,
                    })
                    .collect(),
                None => f
                    .after
                    .as_ref()
                    .map(|a| plain(&a.steps))
                    .unwrap_or_default(),
            },
        }
    }

    /// The selected step (flow pane), else the entry.
    pub fn target_step(&self, content: bool) -> Option<&Step> {
        if content {
            let lines = self.lines();
            let i = self.step_pos.cursor.min(lines.len().checked_sub(1)?);
            return Some(lines[i].step);
        }
        let f = self.selected()?;
        f.after
            .as_ref()
            .or(f.before.as_ref())
            .and_then(|x| x.steps.first())
    }

    /// Head symbol of a step (the base's own symbols are looked up by id).
    pub fn head_symbol(g: &Graph, step: &Step) -> Option<SymIdx> {
        g.by_id(&step.id)
    }

    pub fn move_step(&mut self, delta: isize) {
        let len = self.lines().len();
        self.step_pos.cursor = self.step_pos.cursor.saturating_add_signed(delta);
        self.step_pos.clamp(len);
    }

    pub fn select_step(&mut self, i: usize) {
        let len = self.lines().len();
        self.step_pos.cursor = i;
        self.step_pos.clamp(len);
    }

    /// Next / previous changed (or, in the merged view, added / removed) step.
    pub fn next_change(&mut self, forward: bool) {
        let lines = self.lines();
        let hit = |l: &FlowLine| {
            l.step.changed || l.change.is_some_and(|c| c != Change::Same) || l.rerouted
        };
        let c = self.step_pos.cursor;
        let found = if forward {
            (c + 1..lines.len()).find(|&i| hit(&lines[i]))
        } else {
            (0..c).rev().find(|&i| hit(&lines[i]))
        };
        if let Some(i) = found {
            self.select_step(i);
        }
    }

    /// `b`: after → before → merged → after, when the flow changed.
    pub fn cycle_side(&mut self, sh: &mut Shared) {
        match &self.base {
            BaseStatus::Running(_) | BaseStatus::NotStarted => {
                return sh.notify("before / after: the base revision's graph is being built…");
            }
            BaseStatus::Failed(e) => return sh.notify(format!("before / after unavailable: {e}")),
            BaseStatus::Done => {}
        }
        let Some(f) = self.selected() else {
            return;
        };
        let Some(d) = &f.diff else {
            return;
        };
        if !d.is_relevant() {
            self.side = Side::After;
            return sh.notify("same flow before and after the change");
        }
        let summary = d.summary();
        let has_after = f.after.is_some();
        let has_before = f.before.is_some();
        self.side = match self.side {
            Side::After if has_before => Side::Before,
            Side::After | Side::Before => Side::Diff,
            Side::Diff if has_after => Side::After,
            Side::Diff => Side::Before,
        };
        self.select_step(0);
        sh.notify(format!("flow {}: {summary}", self.side.label()));
    }

    /// `x`: the selected flow as a Mermaid flowchart (before / after when it
    /// changed) in `.git/survol/exports/`; `X`: opened in the browser too.
    pub fn export(&self, sh: &mut Shared, open: bool) -> Option<Diagram> {
        let Some(f) = self.selected() else {
            sh.notify("select an entry point to export its flow");
            return None;
        };
        let slug: String = f
            .entry
            .label
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let head = survol_core::review::short(&sh.review.head_sha);
        let stem = format!("flow-{}-{head}", truncate(&slug, 60));
        let d = Diagram {
            title: format!("Flow {} ({})", f.entry.label, f.entry.name),
            mermaid: f.to_mermaid(),
        };
        let mut text = format!("# {}\n\n{}\n\n", d.title, sh.review.title());
        if let Some(diff) = &f.diff {
            text.push_str(&format!("{}\n\n", diff.summary()));
        }
        text.push_str(&format!("```mermaid\n{}```\n", d.mermaid));
        export::export(sh, &stem, &text, &d, open).then_some(d)
    }

    /// Entry list moved: the flow pane starts at its entry.
    pub fn on_entry_moved(&mut self) {
        self.step_pos.cursor = 0;
        self.step_pos.scroll = 0;
        if self.selected().is_some_and(|f| {
            f.diff.as_ref().is_none_or(|d| !d.is_relevant()) && self.side != Side::After
        }) {
            self.side = Side::After;
        }
    }
}

fn plain(steps: &[Step]) -> Vec<FlowLine<'_>> {
    steps
        .iter()
        .map(|step| FlowLine {
            step,
            change: None,
            rerouted: false,
        })
        .collect()
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}
