//! Rendering: header, footer and help shared by the views, then each view.

mod ask;
mod comments;
mod diff;
mod fileview;
mod flows;
mod graph;
mod map;
pub mod markdown;
mod rows;
mod stack;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::{App, GraphStatus, GroupStatus, Popup, RemoteStatus, View};
use crate::theme::theme;
use crate::views::Focus;
use crate::views::graph::Mode;

pub fn render(f: &mut Frame, app: &mut App) {
    let [header, body, footer] = Split::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());

    render_header(f, header, app);
    match app.view {
        View::Diff => diff::render(f, body, app),
        View::Stack => stack::render(f, body, app),
        View::Graph => graph::render(f, body, app),
    }
    render_footer(f, footer, app);
    let model = app
        .cfg
        .llm
        .ask_model
        .clone()
        .unwrap_or_else(|| "default".into());
    match &mut app.popup {
        Some(Popup::AskInput(i)) => ask::render_input(f, body, i, &model),
        Some(Popup::Pending(p)) => ask::render_pending(f, body, p),
        Some(Popup::Answer(v)) => ask::render_answer(f, body, v, &app.sh.highlighter),
        Some(Popup::History(h)) => ask::render_history(f, body, h, &app.history),
        Some(Popup::Comment(e)) => comments::render_editor(f, body, e),
        Some(Popup::Review) => comments::render_panel(f, body, app),
        Some(Popup::File(_)) => {
            if let Some(Popup::File(mut v)) = app.popup.take() {
                fileview::render(f, body, app, &mut v);
                app.popup = Some(Popup::File(v));
            }
        }
        Some(Popup::Thread(_)) => {
            if let Some(Popup::Thread(mut t)) = app.popup.take() {
                comments::render_thread(f, body, app, &mut t);
                app.popup = Some(Popup::Thread(t));
            }
        }
        None => {}
    }
    if app.help {
        let ctx = HelpCtx::of(app);
        render_help(f, f.area(), ctx, &mut app.help_scroll);
    }
}

/// Bordered pane, its border and title highlighted when focused.
fn pane(focused: bool) -> Block<'static> {
    let t = theme();
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { t.border_focus } else { t.border }))
        .title_style(if focused {
            Style::new().fg(t.border_focus).bold()
        } else {
            Style::new().fg(t.meta)
        })
}

/// A popup: on the popup background, its title in a block.
pub(crate) fn popup_block(title: String) -> Block<'static> {
    let t = theme();
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.popup_border))
        .style(Style::new().bg(t.popup_bg))
        .title(Line::from(title.fg(t.block_fg).bg(t.popup_title_bg).bold()))
}

/// Secondary text: counters, hints, paths.
pub(crate) fn meta<'a>(s: impl Into<std::borrow::Cow<'a, str>>) -> Span<'a> {
    Span::styled(s, Style::new().fg(theme().meta))
}

/// Width of the left list of a two-pane view.
fn list_width(body: Rect) -> u16 {
    (body.width / 3)
        .clamp(24, 60)
        .min(body.width.saturating_sub(20))
}

/// Powerline glyphs (Nerd Font): a solid arrow, a thin one between two
/// blocks of the same colour.
const ARROW: &str = "\u{e0b0}";
const THIN_ARROW: &str = "\u{e0b1}";

/// The header: coloured blocks on a band, chained by powerline arrows, or
/// separated by a space without them.
struct Bar {
    spans: Vec<Span<'static>>,
    /// Background of the last block, `None` after plain text.
    last: Option<Color>,
    powerline: bool,
    bg: Color,
}

impl Bar {
    fn new(powerline: bool) -> Self {
        Self {
            spans: Vec::new(),
            last: None,
            powerline,
            bg: theme().header_bg,
        }
    }

    /// `text` in `fg` on a block of `bg`.
    fn block(&mut self, text: String, fg: Color, bg: Color, bold: bool) {
        if self.powerline {
            match self.last {
                Some(prev) if prev == bg => self.spans.push(THIN_ARROW.fg(theme().meta).bg(bg)),
                Some(prev) => self.spans.push(ARROW.fg(prev).bg(bg)),
                None if !self.spans.is_empty() => self.spans.push(ARROW.fg(self.bg).bg(bg)),
                None => {}
            }
        } else if !self.spans.is_empty() {
            self.spans.push(" ".bg(self.bg));
        }
        let style = Style::new().fg(fg).bg(bg);
        self.spans
            .push(Span::styled(text, if bold { style.bold() } else { style }));
        self.last = Some(bg);
    }

    /// Dark text on a block of `bg`, bold.
    fn info(&mut self, text: String, bg: Color) {
        self.block(text, theme().block_fg, bg, true);
    }

    /// Text on the band itself.
    fn plain(&mut self, text: String, fg: Color) {
        self.close();
        if !self.powerline && !self.spans.is_empty() {
            self.spans.push(" ".bg(self.bg));
        }
        self.spans.push(text.fg(fg).bg(self.bg));
    }

    /// More text on the band, after [`Bar::plain`].
    fn spans(&mut self, spans: Vec<Span<'static>>) {
        let bg = self.bg;
        self.spans.extend(spans.into_iter().map(|s| s.bg(bg)));
    }

    /// Ends the last block.
    fn close(&mut self) {
        if let Some(prev) = self.last.take()
            && self.powerline
        {
            self.spans.push(ARROW.fg(prev).bg(self.bg));
        }
    }

    fn line(mut self) -> Line<'static> {
        self.close();
        Line::from(self.spans).style(Style::new().bg(self.bg))
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let t = theme();
    let sh = &app.sh;
    let (done, total) = sh.state.progress(&sh.review.diff);
    let pct = (done * 100).checked_div(total).unwrap_or(100);
    let (add, del) = sh.review.diff.stats();
    let mut bar = Bar::new(t.powerline);
    bar.info(" survol ".into(), t.badge_bg);
    for (i, v) in View::ALL.iter().enumerate() {
        let tab = format!(" {} {} ", i + 1, v.name());
        if *v == app.view {
            bar.info(tab, t.tab_active_bg);
        } else {
            bar.block(tab, t.tab_fg, t.tab_bg, false);
        }
    }
    bar.block(format!(" {} ", sh.review.title()), t.text, t.title_bg, true);
    bar.plain(format!(" {} files ", sh.review.diff.files.len()), t.meta);
    bar.spans(rows::change_counts(add, Some(del), Style::new()));
    bar.spans(vec![" ".into()]);
    bar.info(
        format!(" {done}/{total} reviewed ({pct}%) "),
        if done == total {
            t.progress_done_bg
        } else {
            t.progress_bg
        },
    );
    let grouping = app.grouping_label();
    if !grouping.is_empty() {
        let bg = match app.group_status {
            GroupStatus::Failed(_) => t.error,
            _ => t.grouping_bg,
        };
        bar.info(format!(" {grouping} "), bg);
    }
    let graph = app.graph_label();
    if !graph.is_empty() {
        let bg = match app.graph_status {
            GraphStatus::Failed(_) => t.error,
            _ => t.graph_bg,
        };
        bar.info(format!(" {graph} "), bg);
    }
    let forge = sh.review.forge();
    match &app.remote_status {
        RemoteStatus::Fetching(_) => bar.info(format!(" ⟳ {forge} "), t.threads_bg),
        RemoteStatus::Failed(_) => bar.info(format!(" {forge} ✗ "), t.error),
        RemoteStatus::Ready => {
            let open = sh
                .discussions()
                .iter()
                .filter(|d| !d.is_system() && d.is_resolvable() && !d.is_resolved())
                .count();
            bar.info(format!(" ◆ {open} open thread(s) "), t.threads_bg);
        }
        RemoteStatus::Local => {}
    }
    let drafts = sh.comments.drafts.len();
    if drafts > 0 || !sh.comments.summary.trim().is_empty() {
        bar.info(format!(" ✎ {drafts} draft(s) "), t.drafts_bg);
    }
    if let Some(p) = &app.publishing {
        bar.info(format!(" ⟳ publishing {p} "), t.accent);
    }
    if let Some(p) = &app.ask_pending {
        let e = p.since.elapsed();
        bar.info(
            format!(" {} asking… {}s ", ask::spinner(e.as_millis()), e.as_secs()),
            t.accent,
        );
    }
    if !sh.worktree_ready {
        bar.plain(" ⟳ worktree".into(), t.meta);
    }
    f.render_widget(
        Paragraph::new(bar.line()).style(Style::new().bg(t.header_bg)),
        area,
    );
}

/// Footer help: `key description` pairs separated by two spaces, the keys
/// highlighted.
fn key_help(text: &str) -> Line<'static> {
    let t = theme();
    let mut spans = vec![Span::raw(" ")];
    for item in text.split("  ").filter(|s| !s.trim().is_empty()) {
        let item = item.trim();
        let (k, d) = item.split_once(' ').unwrap_or((item, ""));
        spans.push(k.to_string().fg(t.key).bold());
        spans.push(format!(" {d}  ").fg(t.meta));
    }
    Line::from(spans)
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let t = theme();
    let line = if app.view == View::Graph && app.graph.query_editing {
        Line::from(vec![
            " find symbol: ".fg(t.accent),
            app.graph.query.clone().into(),
            "▏".fg(t.accent),
            "   (Owner.addPet, addPet, or part of a name)".fg(t.meta),
        ])
    } else if app.view == View::Diff && app.diff.filter_editing {
        Line::from(vec![
            "/".fg(t.accent),
            app.diff.filter.clone().into(),
            "▏".fg(t.accent),
        ])
    } else if app.view == View::Stack && app.stack.confirm_regroup {
        Line::from(
            " Regroup without cache? This makes a new LLM call (may take a minute).  y / n"
                .fg(t.warn)
                .bold(),
        )
    } else if app.view == View::Stack && app.stack.confirm_fold_renames {
        Line::from(
            if app.cfg.review.fold_renames {
                " Group the rename-only hunks like the others? New LLM call, unless already cached.  y / n"
            } else {
                " Set the rename-only hunks aside in their own group? New LLM call, unless already cached.  y / n"
            }
            .fg(t.warn)
            .bold(),
        )
    } else if app.view == View::Stack && app.stack.filter_editing {
        Line::from(vec![
            "/".fg(t.accent),
            app.stack.filter.clone().into(),
            "▏".fg(t.accent),
            "   (title, summary, layer or file of a group)".fg(t.meta),
        ])
    } else if let Some(keys) = which_key(app) {
        key_help(keys)
    } else if let Some(m) = app.sh.message() {
        Line::from(m.to_string().fg(t.warn))
    } else {
        key_help(match app.view {
            View::Diff if app.diff.focus == Focus::List => {
                " j/k files  h/l fold  Enter/C-l diff  space ✓  X file ✓  u unreviewed  c/C file comment  / filter  m files  gs graph  gf file  e edit  P review  ? help"
            }
            View::Diff => {
                " j/k move  n/N hunk  J/K file  ]t thread  space ✓  x ✓ stay  X file ✓  u unreviewed  c comment  v range  C file  Enter whole file  gs graph  gG group  e edit  P review  ? help"
            }
            View::Stack if app.stack.focus == Focus::List => {
                " j/k move  h/l fold  space ✓ + next  x/X ✓ node/group  u unreviewed  J/K group  / filter  Enter/gd diff  C file comment  gs graph  R regroup  P review  ? help"
            }
            View::Stack => {
                " j/k move  n/N hunk  ]t thread  space ✓ + next  J/K group  Enter/gd diff  c comment  v range  C file  Esc tree  gs graph  P review  ? help"
            }
            View::Graph => match app.graph.mode {
                Mode::Symbol => {
                    " j/k move  l/h expand/collapse  o fold  Enter focus node  ⌫/C-o back  J/K section  n/N changed  e edit  gd diff  gG group  / find  f flows  M map  m mode  a ask  ? help"
                }
                Mode::Flows => {
                    " j/k move  Enter/l flow  n/N change  Enter symbol  b before/after  y/gx Mermaid  S to comment  e edit  gd diff  h back  m mode  ? help"
                }
                Mode::Map => {
                    " h/l column  j/k move  Enter middle / down  ⌫/u back  n/N changed  y/gx Mermaid  e edit  gd diff  gG group  m mode  ? help"
                }
                Mode::Modules => {
                    " j/k move  Enter/l symbols  o fold  zM/zR all  t all/impacted  y/gx Mermaid  S to comment  e edit  gd diff  / find  m mode  ? help"
                }
                _ => {
                    " j/k move  Enter symbol graph  J/K module  n/N symbol  e edit  gd diff  gG group  gf file  f flows  M map  / find  m mode  a ask  C-l preview  ? help"
                }
            },
        })
    };
    f.render_widget(
        Paragraph::new(line).style(Style::new().bg(t.footer_bg)),
        area,
    );
}

/// After the first key of a prefix: what the second can be.
fn which_key(app: &App) -> Option<&'static str> {
    let keys = match &app.popup {
        Some(Popup::File(v)) => match v.pending? {
            'g' => " g→ g top",
            'z' => " z→ a toggle  o open  c close the note",
            _ => " ]/[→ c change  t thread",
        },
        Some(Popup::Thread(t)) if t.pending_g => " g→ g top",
        Some(Popup::Answer(v)) if v.pending_g => " g→ d diff  s graph  f whole file",
        Some(Popup::Review) if app.panel.pending_g => " g→ g first row",
        Some(_) => return None,
        None => {
            let p = match app.view {
                View::Diff => app.diff.pending,
                View::Stack => app.stack.pending,
                View::Graph => app.graph.pending,
            }?;
            match (app.view, p) {
                (View::Diff, 'g') => " g→ g top  s graph symbol  G stack group  f whole file",
                (View::Stack, 'g') => " g→ g top  d diff  s graph symbol  f whole file",
                (View::Graph, 'g') => {
                    " g→ g top  d diff  G stack group  f whole file  x export + open"
                }
                (View::Graph, 'z') => {
                    " z→ a toggle  o open  c close  M fold all  R unfold all (symbol tree, module map)"
                }
                (_, 'z') => " z→ a toggle  o open  c close  M fold all  R unfold all",
                (View::Diff, _) => " ]/[→ c hunk  f file  t thread  u unreviewed",
                (View::Stack, _) => " ]/[→ f group  c hunk  t thread  u unreviewed",
                (View::Graph, _) => " ]/[→ c changed symbol / step  f section or module",
            }
        }
    };
    Some(keys)
}

const HELP: &[(&str, &str)] = &[
    ("# All views", ""),
    (
        "Tab / Shift-Tab, 1 2 3",
        "next / previous view, Diff / Stack / Graph",
    ),
    ("Ctrl-h / Ctrl-l", "focus list / content pane"),
    ("B", "show / hide the list pane"),
    ("j / k, Ctrl-d / Ctrl-u", "move, half page down / up"),
    ("Ctrl-f / Ctrl-b", "page down / up"),
    ("gg / G", "top / bottom"),
    (
        "h / l, 0",
        "content: scroll horizontally, reset (lists: fold / unfold, parent)",
    ),
    (
        "] / [ + letter",
        "next / previous: c change (hunk), f file / group / section, t thread, u unreviewed",
    ),
    (
        "za / zo / zc, o",
        "toggle / open / close the fold under the cursor",
    ),
    ("zM / zR", "fold / unfold everything"),
    (
        "gd / gs / gG / gf",
        "go to: the Diff, the symbol in the Graph, the group in the Stack, the whole file",
    ),
    ("Enter", "down a level, inside survol (e opens the editor)"),
    (
        "Esc",
        "cancel the selection or the filter, back to the list",
    ),
    (
        "Ctrl-o",
        "back where a jump came from (gd, gs, gG, Enter, a link, the Review panel)",
    ),
    ("s", "unified ↔ split (also the layout gf opens in)"),
    ("e", "open in the editor (parent nvim if any)"),
    ("a", "ask the LLM about the node / group / hunk"),
    ("A", "last answer, then the review's questions"),
    ("P", "Review panel: drafts, summary, discussions, publish"),
    (
        "?",
        "these keys (j / k, gg / G scroll); in a popup, the popup's keys",
    ),
    (
        "q, Ctrl-c",
        "quit (state is saved on each change; not while publishing)",
    ),
    (
        "Ctrl-r",
        "reload: new snapshot of the working tree, new commits (twice if a question or the grouping is running)",
    ),
    ("# Diff", ""),
    ("n / N, ]c / [c", "next / previous hunk"),
    ("J / K, ]f / [f", "next / previous file"),
    ("]t / [t", "next / previous thread (draft or discussion)"),
    ("u, ]u / [u", "next / previous unreviewed hunk"),
    ("space", "toggle hunk reviewed, go to the next unreviewed"),
    ("x", "toggle hunk reviewed, stay"),
    ("X", "toggle file reviewed (folds it)"),
    ("o / za, Enter on header", "fold / unfold file"),
    ("zM / zR", "fold / unfold all files (list: every directory)"),
    ("/", "filter files, Esc to clear"),
    ("m", "files: tree → pairs (classes and tests) → flat"),
    ("h / l (list)", "fold / unfold directory, parent"),
    (
        "space / x / X (list)",
        "on a directory: mark all its files reviewed",
    ),
    ("Esc (list)", "clear the filter"),
    (
        "gs / gG",
        "Graph view of the symbol / Stack group of the hunk",
    ),
    (
        "c (content)",
        "comment the line (on a draft: edit; on a thread: reply)",
    ),
    ("v / V then c (content)", "select lines, comment the range"),
    ("C, c / C (list)", "comment the whole file"),
    (
        "Enter on a line (content)",
        "the whole file at that line (as gf)",
    ),
    (
        "o / za on a note (content)",
        "show the whole note, fold it back",
    ),
    (
        "Enter on a note (content)",
        "its thread: code, note, replies (c reply, n/N next)",
    ),
    ("# Stack", ""),
    ("space", "toggle group / layer / hunk reviewed, go on"),
    ("x / X", "toggle the node / its group reviewed, stay"),
    ("u, ]u / [u", "next / previous unreviewed group"),
    ("J / K, ]f / [f", "next / previous group"),
    ("]c / [c", "next / previous hunk"),
    (
        "/ (list)",
        "filter the groups (title, summary, layers, files), Esc to clear",
    ),
    ("h / l (list)", "fold / unfold, parent / child"),
    ("o / za, zM / zR", "fold / unfold node, all"),
    (
        "Enter / gd",
        "the Diff view: at the line (content), at the node's first hunk (list)",
    ),
    ("n / N (content)", "next / previous hunk"),
    ("]t / [t (content)", "next / previous thread"),
    ("Esc (content)", "focus the tree"),
    ("w", "grouping warnings"),
    ("R", "regroup without cache (asks: LLM call)"),
    (
        "F",
        "rename / move / import-only hunks: own group, or with the others",
    ),
    ("gs", "Graph view of the hunk's symbol"),
    (
        "c / v / V then c / C (content)",
        "comment line / range / file",
    ),
    ("C (list)", "comment the file of the node (one file only)"),
    (
        "o / za, Enter on a note (content)",
        "whole note / fold; its thread",
    ),
    ("# Graph", ""),
    (
        "m",
        "mode: changed symbols / search / module map / map / flows / symbol",
    ),
    (
        "M",
        "map: one box, what uses it on the left, what it uses on the right",
    ),
    (
        "f",
        "flows: impacted entry points and their end-to-end flow",
    ),
    ("/", "find a symbol by name (changed or not); Esc: back"),
    (
        "Enter (lists)",
        "symbol: focus it (new root); section / module: fold",
    ),
    (
        "o / za / zo / zc (lists)",
        "fold / unfold the section or module",
    ),
    (
        "zM / zR (lists)",
        "every section of the symbol, every module",
    ),
    (
        "l / h (lists)",
        "expand / collapse (callers of callers…), parent",
    ),
    (
        "J / K, ]f / [f (lists)",
        "next / previous section or module",
    ),
    ("n / N, ]c / [c (lists)", "next / previous changed symbol"),
    (
        "Backspace / Ctrl-o",
        "back to the previous symbol or list, then where a jump came from",
    ),
    ("e", "open the node's line in the editor"),
    (
        "gd / gG",
        "the symbol's hunks in the Diff view / its Stack group",
    ),
    ("Ctrl-l, j / k", "preview pane, scroll it"),
    ("Enter (content)", "the whole file at the line (as gf)"),
    (
        "t (module map)",
        "all modules ↔ changed ones and their neighbours",
    ),
    (
        "y / gx (module map)",
        "write the module map as Mermaid (gx: and open it in the browser)",
    ),
    ("S", "add the last exported diagram to the overall comment"),
    (
        "map: h / l, j / k",
        "column on the left / right, next / previous box of the column",
    ),
    (
        "map: Enter",
        "side box: to the middle; middle box: down a level (module → files → symbols)",
    ),
    (
        "map: Backspace / u / Ctrl-o",
        "previous middle box, else back up a level",
    ),
    (
        "map: n / N, ]c / [c",
        "next / previous changed box of the level to the middle",
    ),
    (
        "map: y / gx",
        "the middle box and its columns as Mermaid (gx: in the browser)",
    ),
    (
        "flows: Enter / l, h",
        "into the flow, back to the entry points",
    ),
    (
        "flows: n / N, ]c / [c",
        "next / previous changed (or added / removed) step",
    ),
    ("flows: Enter (content)", "symbol view of the step"),
    (
        "flows: b",
        "after → before → merged (when the flow differs)",
    ),
    (
        "flows: y / gx",
        "write the flow as Mermaid (gx: and open it in the browser)",
    ),
    ("# Whole file", ""),
    ("n / N, ]c / [c", "next / previous change"),
    ("]t / [t", "next / previous thread"),
    ("d", "hide / show the removed lines"),
    ("s", "side by side"),
    ("c, v / V then c", "comment a diff line / range"),
    ("o / za / zo / zc", "fold a note"),
    ("Enter on a note", "its thread"),
    ("e", "open the editor at the line"),
    ("h / l, gg / G, Ctrl-d / Ctrl-u", "move"),
    ("Esc / q", "close"),
    ("# Thread", ""),
    ("c", "reply (on a draft: edit it)"),
    ("e", "open the line of the thread in the editor"),
    ("n / N, Tab / Shift-Tab", "next / previous thread"),
    ("j / k, Ctrl-d / Ctrl-u, space", "scroll"),
    ("gg / G", "top / bottom"),
    ("Esc / q", "close"),
    ("# Review panel", ""),
    ("Enter", "go to the line; on the summary: edit it"),
    ("c", "edit the draft / summary, reply to a discussion"),
    ("e", "open the line in the editor"),
    ("t", "the whole thread"),
    ("d / x", "delete the draft (asks)"),
    ("S", "the overall comment"),
    ("p", "publish (shows what is sent, J: the JSON requests)"),
    ("r", "fetch the discussions again"),
    ("j / k, gg / G", "move"),
    ("Esc / q / P", "close"),
    ("# Answer", ""),
    ("Tab / n, Shift-Tab / N", "next / previous link"),
    ("Enter", "follow the link: the Graph, else the Diff"),
    (
        "gs / gd / gf",
        "the link in the Graph / the Diff / the whole file",
    ),
    ("e", "open the link in the editor"),
    ("j / k, Ctrl-d / Ctrl-u", "scroll"),
    ("A", "the review's questions"),
    ("Esc / q", "close"),
    ("# Questions", ""),
    ("j / k, Enter", "move, open the answer"),
    ("Esc / q / A", "close"),
];

/// Which keys the help shows: a section (a view, or a popup), the focus of
/// a view and the mode of the Graph view.
#[derive(Debug, Clone, Copy)]
pub struct HelpCtx {
    pub section: &'static str,
    /// A view (with "All views") rather than a popup.
    pub view: bool,
    /// The list pane has the keyboard.
    pub list: bool,
    pub mode: Option<Mode>,
}

impl HelpCtx {
    pub fn of(app: &App) -> Self {
        let popup = match &app.popup {
            Some(Popup::File(_)) => Some("Whole file"),
            Some(Popup::Thread(_)) => Some("Thread"),
            Some(Popup::Review) => Some("Review panel"),
            Some(Popup::Answer(_)) => Some("Answer"),
            Some(Popup::History(_)) => Some("Questions"),
            _ => None,
        };
        if let Some(section) = popup {
            return HelpCtx {
                section,
                view: false,
                list: false,
                mode: None,
            };
        }
        let list = match app.view {
            View::Diff => app.diff.focus == Focus::List,
            View::Stack => app.stack.focus == Focus::List,
            View::Graph => app.graph.focus == Focus::List,
        };
        HelpCtx {
            section: app.view.name(),
            view: true,
            list,
            mode: (app.view == View::Graph).then_some(app.graph.mode),
        }
    }

    /// Whether the entry of key `k` applies here.
    fn shows(&self, k: &str) -> bool {
        if k.contains("(list)") && !self.list {
            return false;
        }
        if k.contains("(content)") && self.list {
            return false;
        }
        let Some(mode) = self.mode else {
            return true;
        };
        let in_map = k.starts_with("map:");
        let in_flows = k.starts_with("flows:");
        match mode {
            Mode::Map => !in_flows && !k.contains("(lists)") && !k.contains("(module map)"),
            Mode::Flows => !in_map && !k.contains("(lists)") && !k.contains("(module map)"),
            Mode::Modules => !in_map && !in_flows,
            _ => !in_map && !in_flows && !k.contains("(module map)"),
        }
    }
}

/// Keys of all views, then of the view (or the keys of the popup), those
/// of other focuses and modes left out.
pub fn help_lines(ctx: HelpCtx) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let mut keep = false;
    for &(k, d) in HELP {
        if let Some(title) = k.strip_prefix("# ") {
            keep = (ctx.view && title == "All views") || title == ctx.section;
            if keep {
                out.push((k, d));
            }
        } else if keep && ctx.shows(k) {
            out.push((k, d));
        }
    }
    out
}

fn render_help(f: &mut Frame, area: Rect, ctx: HelpCtx, scroll: &mut usize) {
    let help = help_lines(ctx);
    let w = 90.min(area.width);
    let room = w.saturating_sub(2) as usize;
    let kw = help
        .iter()
        .filter(|(k, _)| !k.starts_with("# "))
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
        .min(room / 2);
    // Descriptions wrapped under themselves, after the key column.
    let dw = room.saturating_sub(kw + 4).max(8);
    let mut lines: Vec<Line> = Vec::new();
    for (k, d) in &help {
        if let Some(title) = k.strip_prefix("# ") {
            lines.push(Line::from(format!(" {title}").bold()));
            continue;
        }
        for (i, part) in wrap(d, dw).into_iter().enumerate() {
            let key = if i == 0 { *k } else { "" };
            lines.push(Line::from(vec![
                format!("   {key:<kw$} ").fg(theme().key),
                part.into(),
            ]));
        }
    }
    let h = (lines.len() as u16 + 2).min(area.height);
    let inner = h.saturating_sub(2) as usize;
    *scroll = (*scroll).min(lines.len().saturating_sub(inner));
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let title = if lines.len() > inner {
        " keys · j/k scroll · any other key closes ".to_string()
    } else {
        " keys ".to_string()
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .scroll((*scroll as u16, 0))
            .block(popup_block(title)),
        rect,
    );
}

/// Word-wraps `text` to `width` columns (at least one line). Words longer
/// than a line are cut.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let len = line.chars().count();
            let mut word: Vec<char> = word.chars().collect();
            if len > 0 && len + 1 + word.len() > width {
                lines.push(std::mem::take(&mut line));
            }
            while word.len() > width {
                lines.push(word.drain(..width).collect());
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.extend(word);
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use survol_core::comments::{self, Anchor};
    use survol_core::config::Config;
    use survol_core::review_state::ReviewState;

    use super::{ARROW, Bar, Focus, HelpCtx, Mode, Popup, help_lines, key_help, wrap};
    use crate::app::{App, View};
    use crate::theme::theme;
    use crate::views::stack::tests::fixture;

    /// The screen of `app` drawn on a `w` × `h` terminal, one string per row.
    pub(crate) fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| super::render(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect()
    }

    /// An app on the test fixture, its state under `dir`.
    pub(crate) fn app(dir: &std::path::Path) -> App {
        let (review, _) = fixture(dir);
        App::new(
            review,
            ReviewState::default(),
            dir.join("state.json"),
            Config::default(),
        )
    }

    #[test]
    fn long_notes_are_wrapped_to_the_pane_not_cut() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        let d = &app.sh.review.diff;
        let line = comments::line_anchor(d, 0, 1);
        let body = "The confrontation is made on the unlocked read, the lock is only taken \
            afterwards by refresh which rehydrates the row without replaying the filter.";
        app.sh
            .comments
            .add(Anchor::Line { start: None, line }, body);
        app.sh.rebuild_notes();
        app.diff.relayout(&app.sh);
        let text = screen(&mut app, 90, 30).join("\n");
        for word in body.split_whitespace() {
            assert!(text.contains(word), "`{word}` is missing:\n{text}");
        }
        assert!(!text.contains('…'), "nothing is cut:\n{text}");
    }

    #[test]
    fn help_shows_the_current_view_focus_mode_or_popup() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        let keys = |app: &App| -> Vec<&str> {
            help_lines(HelpCtx::of(app))
                .iter()
                .map(|(k, _)| *k)
                .collect()
        };
        app.switch(View::Graph);
        let graph = keys(&app);
        assert_eq!(graph[0], "# All views");
        assert!(graph.contains(&"# Graph") && !graph.contains(&"# Stack"));
        // The changed symbols: no map nor flows keys.
        assert!(
            !graph
                .iter()
                .any(|k| k.starts_with("map:") || k.starts_with("flows:"))
        );
        app.graph.mode = Mode::Map;
        let map = keys(&app);
        assert!(map.contains(&"map: y / gx") && !map.iter().any(|k| k.starts_with("flows:")));
        // The Diff view: list keys only with the list focused.
        app.switch(View::Diff);
        assert!(!keys(&app).contains(&"Esc (list)"));
        app.diff.focus = Focus::List;
        let list = keys(&app);
        assert!(list.contains(&"Esc (list)") && !list.contains(&"c (content)"));
        // A popup: its keys only.
        app.popup = Some(Popup::Review);
        let panel = keys(&app);
        assert_eq!(panel[0], "# Review panel");
        assert!(!panel.contains(&"# All views"));
    }

    #[test]
    fn help_is_wrapped_not_cut_and_scrolls() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        for view in View::ALL {
            app.switch(view);
            app.help = true;
            app.help_scroll = 0;
            let text = screen(&mut app, 90, 120).join("\n");
            for (k, d) in help_lines(HelpCtx::of(&app)) {
                for word in k.trim_start_matches("# ").split_whitespace() {
                    assert!(text.contains(word), "`{word}` of `{k}` is missing:\n{text}");
                }
                for word in d.split_whitespace() {
                    assert!(text.contains(word), "`{word}` of `{k}` is missing:\n{text}");
                }
            }
        }
        // On a short screen: a hint, and the scroll is kept on the last page.
        app.switch(View::Graph);
        app.help_scroll = 1000;
        let text = screen(&mut app, 90, 20).join("\n");
        assert!(text.contains("j/k scroll"), "{text}");
        assert!(app.help_scroll < 100);
        assert!(text.contains("overall comment"), "the last line:\n{text}");
    }

    #[test]
    fn footer_follows_the_focus() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        let footer = |app: &mut App| screen(app, 250, 20).pop().unwrap();
        assert!(footer(&mut app).contains("c comment"));
        app.diff.focus = crate::views::Focus::List;
        let list = footer(&mut app);
        assert!(list.contains("c/C file comment") && !list.contains("v range"));
    }

    #[test]
    fn footer_shows_what_follows_a_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        let footer = |app: &mut App| screen(app, 250, 20).pop().unwrap();
        app.on_key(crate::views::stack::tests::key('g'));
        let g = footer(&mut app);
        assert!(g.contains("G") && g.contains("stack group"), "{g}");
        app.on_key(crate::views::stack::tests::key('g'));
        app.on_key(crate::views::stack::tests::key(']'));
        assert!(footer(&mut app).contains("]/[→"));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!footer(&mut app).contains("]/[→"));
    }

    #[test]
    fn wraps_words() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("", 10), [""]);
        assert_eq!(
            wrap("see https://example.org/a/b", 10),
            ["see", "https://ex", "ample.org/", "a/b"]
        );
    }

    fn text(line: &ratatui::text::Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn header_blocks_are_chained_by_arrows_or_spaces() {
        let t = theme();
        let build = |powerline| {
            let mut bar = Bar::new(powerline);
            bar.info(" survol ".into(), t.badge_bg);
            bar.info(" 1 Diff ".into(), t.tab_active_bg);
            bar.plain(" 3 files ".into(), t.meta);
            bar.info(" 1/2 ".into(), t.progress_bg);
            bar.line()
        };
        let a = build(true);
        assert_eq!(
            text(&a),
            format!(" survol {ARROW} 1 Diff {ARROW} 3 files {ARROW} 1/2 {ARROW}")
        );
        // The arrow takes the colour of the block it leaves, on the next one.
        let arrow = &a.spans[1];
        assert_eq!(
            (arrow.style.fg, arrow.style.bg),
            (Some(t.badge_bg), Some(t.tab_active_bg))
        );
        assert_eq!(a.spans[0].style.fg, Some(t.block_fg));
        let plain = build(false);
        assert_eq!(text(&plain), " survol   1 Diff   3 files   1/2 ");
        assert!(!text(&plain).contains(ARROW));
    }

    #[test]
    fn footer_keys_stand_out_from_their_description() {
        let t = theme();
        let l = key_help(" j/k move  space ✓ + next  ? help");
        assert_eq!(text(&l), " j/k move  space ✓ + next  ? help  ");
        let key = l.spans.iter().find(|s| s.content == "space").unwrap();
        assert_eq!(key.style.fg, Some(t.key));
        let desc = l.spans.iter().find(|s| s.content == " ✓ + next  ").unwrap();
        assert_eq!(desc.style.fg, Some(t.meta));
    }

    #[test]
    fn the_screen_uses_the_chrome_colours() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        term.draw(|f| super::render(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let t = theme();
        let row =
            |y: u16| -> String { (0..100).map(|x| buf[(x, y)].symbol().to_string()).collect() };
        // Header: badge, then powerline arrows, on the header band.
        assert!(row(0).starts_with(" survol "), "{}", row(0));
        assert!(row(0).contains(ARROW));
        assert_eq!(buf[(1, 0)].bg, t.badge_bg);
        assert_eq!(buf[(99, 0)].bg, t.header_bg);
        // Footer band, keys in the key colour.
        assert_eq!(buf[(99, 19)].bg, t.footer_bg);
        assert_eq!(buf[(1, 19)].fg, t.key);
        // Focused pane (the diff) has the focus border; the list does not.
        let diff_x = (0..100)
            .find(|&x| buf[(x, 1)].symbol() == "╭" && x > 0)
            .unwrap();
        assert_eq!(buf[(diff_x, 1)].fg, t.border_focus);
        assert_eq!(buf[(0, 1)].fg, t.border);
        // The first file header is a band across the diff pane.
        let y = (2..19)
            .find(|&y| {
                buf[(diff_x + 1, y)].bg == t.file_header_bg
                    || buf[(diff_x + 1, y)].bg == t.cursor_bg
            })
            .expect("a file header");
        assert_eq!(buf[(diff_x + 1 + 60, y)].bg, buf[(diff_x + 1, y)].bg);
    }
}
