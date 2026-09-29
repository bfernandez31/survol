//! Rendering: header, footer and help shared by the views, then each view.

mod ask;
mod comments;
mod diff;
mod fileview;
mod flows;
mod graph;
pub mod markdown;
mod rows;
mod stack;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::{App, GraphStatus, GroupStatus, Popup, RemoteStatus, View};
use crate::views::graph::Mode;

pub(crate) const ADDED_BG: Color = Color::Rgb(22, 52, 34);
pub(crate) const REMOVED_BG: Color = Color::Rgb(62, 24, 28);
pub(crate) const CURSOR_BG: Color = Color::Rgb(60, 60, 80);
pub(crate) const ACCENT: Color = Color::Cyan;

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
        render_help(f, f.area(), app.view);
    }
}

/// Bordered pane, highlighted when focused.
fn pane(focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::new().fg(ACCENT)
        } else {
            Style::new().dim()
        })
}

/// Width of the left list of a two-pane view.
fn list_width(body: Rect) -> u16 {
    (body.width / 3)
        .clamp(24, 60)
        .min(body.width.saturating_sub(20))
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let sh = &app.sh;
    let (done, total) = sh.state.progress(&sh.review.diff);
    let pct = (done * 100).checked_div(total).unwrap_or(100);
    let (add, del) = sh.review.diff.stats();
    let mut spans = vec![" survol ".bold().fg(Color::Black).bg(ACCENT), " ".into()];
    for (i, v) in View::ALL.iter().enumerate() {
        let tab = format!(" {} {} ", i + 1, v.name());
        spans.push(if *v == app.view {
            tab.bold().fg(Color::Black).bg(Color::Gray)
        } else {
            tab.dim()
        });
    }
    spans.extend([
        format!("  {} ", sh.review.title()).bold(),
        format!(" {} files  +{add} -{del} ", sh.review.diff.files.len()).dim(),
        Span::styled(
            format!(" {done}/{total} reviewed ({pct}%) "),
            Style::new().fg(if done == total {
                Color::Green
            } else {
                Color::Yellow
            }),
        ),
    ]);
    let grouping = app.grouping_label();
    if !grouping.is_empty() {
        let style = match app.group_status {
            GroupStatus::Failed(_) => Style::new().fg(Color::Red),
            GroupStatus::Running { .. } => Style::new().fg(ACCENT),
            GroupStatus::Done => Style::new().dim(),
        };
        spans.push(Span::styled(format!(" {grouping} "), style));
    }
    let graph = app.graph_label();
    if !graph.is_empty() {
        let style = match app.graph_status {
            GraphStatus::Failed(_) => Style::new().fg(Color::Red),
            GraphStatus::Running { .. } => Style::new().fg(ACCENT),
            _ => Style::new().dim(),
        };
        spans.push(Span::styled(format!(" {graph} "), style));
    }
    let drafts = sh.comments.drafts.len();
    if drafts > 0 || !sh.comments.summary.trim().is_empty() {
        spans.push(Span::styled(
            format!(" ✎ {drafts} draft(s) "),
            Style::new().fg(Color::Yellow),
        ));
    }
    match &app.remote_status {
        RemoteStatus::Fetching(_) => spans.push(" ⟳ GitLab ".fg(ACCENT)),
        RemoteStatus::Failed(_) => spans.push(" GitLab ✗ ".fg(Color::Red)),
        RemoteStatus::Ready => {
            let open = sh
                .discussions()
                .iter()
                .filter(|d| !d.is_system() && d.is_resolvable() && !d.is_resolved())
                .count();
            spans.push(format!(" ◆ {open} open thread(s) ").fg(Color::Magenta));
        }
        RemoteStatus::Local => {}
    }
    if let Some(p) = &app.publishing {
        spans.push(Span::styled(
            format!(" ⟳ publishing {p} "),
            Style::new().fg(ACCENT).bold(),
        ));
    }
    if let Some(p) = &app.ask_pending {
        let e = p.since.elapsed();
        spans.push(Span::styled(
            format!(" {} asking… {}s ", ask::spinner(e.as_millis()), e.as_secs()),
            Style::new().fg(ACCENT),
        ));
    }
    if !sh.worktree_ready {
        spans.push("  ⟳ worktree".dim());
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let line = if app.view == View::Graph && app.graph.query_editing {
        Line::from(vec![
            " find symbol: ".fg(ACCENT),
            app.graph.query.clone().into(),
            "▏".fg(ACCENT),
            "   (Owner.addPet, addPet, or part of a name)".dim(),
        ])
    } else if app.view == View::Diff && app.diff.filter_editing {
        Line::from(vec![
            "/".fg(ACCENT),
            app.diff.filter.clone().into(),
            "▏".fg(ACCENT),
        ])
    } else if app.view == View::Stack && app.stack.confirm_regroup {
        Line::from(
            " Regroup without cache? This makes a new LLM call (may take a minute).  y / n"
                .fg(Color::Yellow)
                .bold(),
        )
    } else if let Some(m) = app.sh.message() {
        Line::from(m.to_string().fg(Color::Yellow))
    } else {
        Line::from(
            match app.view {
                View::Diff => {
                    " j/k move  n/N hunk  J/K file  space ✓  r file ✓  u unreviewed  c comment  V range  C file  P review  a ask  gs graph  gf file  e edit  / filter  m files  ? help"
                }
                View::Stack => {
                    " j/k move  h/l fold  space ✓ + next  u unreviewed  J/K group  Enter/gd diff  c comment  V range  P review  a ask  gs graph  gf file  R regroup  ? help"
                }
                View::Graph => match app.graph.mode {
                    Mode::Symbol => {
                        " j/k move  l/h expand/collapse  Enter focus node  ⌫/C-o back  n/N section  e edit  gd diff  gf file  / find  m mode  a ask  A answers  ? help"
                    }
                    Mode::Flows => {
                        " j/k move  Enter/l flow  n/N change  Enter symbol  b before/after  x/X Mermaid  S to comment  e edit  gd diff  h back  m mode  ? help"
                    }
                    Mode::Modules => {
                        " j/k move  Enter/l changed symbols  Enter on symbol: graph  e edit  gd diff  x/X Mermaid  S to comment  / find  m mode  ? help"
                    }
                    _ => {
                        " j/k move  Enter symbol graph  J/K module  e edit  gd diff  gf file  a ask  / find symbol  m mode  C-l preview  ? help"
                    }
                },
            }
            .dim(),
        )
    };
    f.render_widget(Paragraph::new(line), area);
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
    ("gg / G", "top / bottom"),
    ("h / l, 0", "scroll content horizontally, reset"),
    ("s", "unified ↔ split"),
    (
        "gf",
        "whole file: changes in place (n/N, ]c/[c, d, s, c, e)",
    ),
    ("e", "open in editor (parent nvim if any)"),
    ("a", "ask the LLM about the node / group / hunk"),
    ("A", "last answer, then the review's questions"),
    ("P", "Review panel: drafts, summary, discussions, publish"),
    ("q", "quit (state is saved on each change)"),
    ("# Diff", ""),
    ("n / N  ({ })", "next / previous hunk"),
    ("J / K  ([ ])", "next / previous file"),
    ("u", "next unreviewed hunk"),
    ("space", "toggle hunk reviewed, go to next"),
    ("r / v", "toggle file reviewed (folds it)"),
    ("o / za, Enter on header", "fold / unfold file"),
    ("zM / zR", "fold / unfold all (list: every directory)"),
    ("/", "filter files, Esc to clear"),
    ("m", "files: tree → pairs (classes and tests) → flat"),
    ("h / l (list)", "fold / unfold directory, parent"),
    ("space on a directory", "mark all its files reviewed"),
    ("gs", "Graph view of the symbol under the cursor"),
    (
        "c",
        "comment the line (on a draft: edit; on a thread: reply)",
    ),
    ("V then c", "select lines, comment the range"),
    ("C", "comment the whole file"),
    ("o / za on a note", "show the whole note, fold it back"),
    (
        "Enter on a note",
        "its thread: code, note, replies (c reply, n/N next)",
    ),
    ("# Stack", ""),
    ("space", "toggle group / layer / hunk reviewed, go on"),
    ("u", "next unreviewed group"),
    ("J / K  ([ ])", "next / previous group"),
    ("h / l (list)", "fold / unfold, parent / child"),
    ("o / za, zM / zR", "fold / unfold node, all"),
    ("Enter / gd", "show the hunk in the Diff view"),
    ("n / N (content)", "next / previous hunk"),
    ("w", "grouping warnings"),
    ("R", "regroup without cache (asks: LLM call)"),
    ("gs", "Graph view of the hunk's symbol"),
    ("c / V then c / C (content)", "comment line / range / file"),
    ("o / za, Enter on a note", "whole note / fold; its thread"),
    ("# Graph", ""),
    (
        "m",
        "mode: changed symbols / search / module map / flows / symbol",
    ),
    (
        "f",
        "flows: impacted entry points and their end-to-end flow",
    ),
    (
        "flows: Enter / l, h",
        "into the flow, back to the entry points",
    ),
    (
        "flows: n / N",
        "next / previous changed (or added / removed) step",
    ),
    (
        "flows: b",
        "after → before → merged (when the flow differs)",
    ),
    (
        "flows: x / X",
        "write the flow as Mermaid (X: and open it in the browser)",
    ),
    ("Enter", "symbol: focus it (new root); section/module: fold"),
    ("l / h", "expand / collapse (callers of callers…), parent"),
    ("Backspace / Ctrl-o", "back to the previous symbol or list"),
    ("n / N  (J / K)", "next / previous section or module"),
    ("/", "find a symbol by name (changed or not)"),
    ("e", "open the node's line in the editor"),
    ("gd", "the symbol's hunks in the Diff view"),
    ("Ctrl-l, j / k", "preview pane, scroll it"),
    ("x", "write the module map as Mermaid (.git/survol/exports)"),
    ("X", "same, plus an HTML page opened in the browser"),
    ("S", "add the last exported diagram to the overall comment"),
    (
        "a (answer: Tab, Enter, d, e)",
        "ask; answer links: next, graph, diff, editor",
    ),
];

/// Keys of all views, then of `view`.
fn help_lines(view: View) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let mut keep = false;
    for &(k, d) in HELP {
        if let Some(title) = k.strip_prefix("# ") {
            keep = title == "All views" || title == view.name();
        }
        if keep {
            out.push((k, d));
        }
    }
    out
}

fn render_help(f: &mut Frame, area: Rect, view: View) {
    let help = help_lines(view);
    let w = 80.min(area.width);
    let h = (help.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let lines: Vec<Line> = help
        .iter()
        .map(|(k, d)| match k.strip_prefix("# ") {
            Some(title) => Line::from(format!(" {title}").bold()),
            None => Line::from(vec![format!("   {k:<26}").fg(ACCENT), (*d).into()]),
        })
        .collect();
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" keys "),
        ),
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use survol_core::comments::{self, Anchor};
    use survol_core::config::Config;
    use survol_core::review_state::ReviewState;

    use super::{help_lines, wrap};
    use crate::app::{App, View};
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
    fn help_shows_the_current_view() {
        let graph = help_lines(View::Graph);
        assert_eq!(graph[0].0, "# All views");
        assert!(graph.iter().any(|(k, _)| *k == "# Graph"));
        assert!(!graph.iter().any(|(k, _)| *k == "# Stack"));
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
}
