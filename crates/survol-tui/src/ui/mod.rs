//! Rendering: header, footer and help shared by the views, then each view.

mod diff;
mod rows;
mod stack;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::{App, GroupStatus, View};

const ADDED_BG: Color = Color::Rgb(22, 52, 34);
const REMOVED_BG: Color = Color::Rgb(62, 24, 28);
const CURSOR_BG: Color = Color::Rgb(60, 60, 80);
const ACCENT: Color = Color::Cyan;

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
    }
    render_footer(f, footer, app);
    if app.help {
        render_help(f, f.area());
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
    if !sh.worktree_ready {
        spans.push("  ⟳ worktree".dim());
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let line = if app.view == View::Diff && app.diff.filter_editing {
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
                    " j/k move  n/N hunk  J/K file  space hunk ✓  r file ✓  u next unreviewed  o fold  s split  e edit  / filter  Tab view  ? help  q quit"
                }
                View::Stack => {
                    " j/k move  h/l fold  space ✓ + next  u next unreviewed  J/K group  Enter/gd to diff  C-h/C-l pane  e edit  R regroup  Tab view  ? help"
                }
            }
            .dim(),
        )
    };
    f.render_widget(Paragraph::new(line), area);
}

const HELP: &[(&str, &str)] = &[
    ("# All views", ""),
    (
        "Tab / Shift-Tab, 1 / 2",
        "next / previous view, Diff / Stack",
    ),
    ("Ctrl-h / Ctrl-l", "focus list / content pane"),
    ("B", "show / hide the list pane"),
    ("j / k, Ctrl-d / Ctrl-u", "move, half page down / up"),
    ("gg / G", "top / bottom"),
    ("h / l, 0", "scroll content horizontally, reset"),
    ("s", "unified ↔ split"),
    ("e", "open in editor (parent nvim if any)"),
    ("q", "quit (state is saved on each change)"),
    ("# Diff", ""),
    ("n / N  ({ })", "next / previous hunk"),
    ("J / K  ([ ])", "next / previous file"),
    ("u", "next unreviewed hunk"),
    ("space", "toggle hunk reviewed, go to next"),
    ("r / v", "toggle file reviewed (folds it)"),
    ("o / za, Enter on header", "fold / unfold file"),
    ("zM / zR", "fold / unfold all"),
    ("/", "filter files, Esc to clear"),
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
];

fn render_help(f: &mut Frame, area: Rect) {
    let w = 76.min(area.width);
    let h = (HELP.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let lines: Vec<Line> = HELP
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

/// Word-wraps `text` to `width` columns (at least one line).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let len = line.chars().count();
            if len > 0 && len + 1 + word.chars().count() > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wraps_words() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("", 10), [""]);
    }
}
