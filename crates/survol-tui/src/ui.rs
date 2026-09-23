//! Rendering of the diff view.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use survol_core::model::{Diff, DiffLine, FileStatus, LineKind};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Focus, Layout, Row, SideItem};
use crate::highlight::{Spans, expand_tabs};

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
    let diff_area = if app.sidebar_visible {
        let side_w = (body.width / 3)
            .clamp(24, 60)
            .min(body.width.saturating_sub(20));
        let [side, main] =
            Split::horizontal([Constraint::Length(side_w), Constraint::Min(10)]).areas(body);
        render_sidebar(f, side, app);
        main
    } else {
        body
    };
    render_diff(f, diff_area, app);
    render_footer(f, footer, app);
    if app.help {
        render_help(f, f.area());
    }
}

fn status_letter(s: FileStatus) -> (&'static str, Color) {
    match s {
        FileStatus::Added => ("A", Color::Green),
        FileStatus::Modified => ("M", Color::Yellow),
        FileStatus::Deleted => ("D", Color::Red),
        FileStatus::Renamed => ("R", Color::Magenta),
        FileStatus::Copied => ("C", Color::Magenta),
    }
}

fn file_stats(diff: &Diff, f: usize) -> (usize, usize) {
    diff.file_hunks(f)
        .fold((0, 0), |(a, r), h| (a + h.added(), r + h.removed()))
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let (done, total) = app.state.progress(&app.review.diff);
    let pct = (done * 100).checked_div(total).unwrap_or(100);
    let (add, del) = app.review.diff.stats();
    let wt = if app.worktree_ready {
        ""
    } else {
        "  ⟳ worktree"
    };
    let line = Line::from(vec![
        " survol ".bold().fg(Color::Black).bg(ACCENT),
        format!(" {} ", app.review.title()).bold(),
        format!(" {} files  +{add} -{del} ", app.review.diff.files.len()).dim(),
        Span::styled(
            format!(" {done}/{total} reviewed ({pct}%) "),
            Style::new().fg(if done == total {
                Color::Green
            } else {
                Color::Yellow
            }),
        ),
        wt.dim(),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let line = if app.filter_editing {
        Line::from(vec![
            "/".fg(ACCENT),
            app.filter.clone().into(),
            "▏".fg(ACCENT),
        ])
    } else if let Some(m) = app.message() {
        Line::from(m.to_string().fg(Color::Yellow))
    } else {
        Line::from(
            " j/k move  n/N hunk  J/K file  space hunk ✓  r file ✓  u next unreviewed  o fold  s split  e edit  / filter  ? help  q quit"
                .dim(),
        )
    };
    f.render_widget(Paragraph::new(line), area);
}

fn render_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let diff = &app.review.diff;
    let focused = app.focus == Focus::Sidebar;
    let width = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .sidebar
        .iter()
        .map(|item| match item {
            SideItem::Dir(d) => {
                let d = if d.is_empty() {
                    "./".to_string()
                } else {
                    format!("{d}/")
                };
                ListItem::new(Line::from(truncate_left(&d, width).fg(Color::Blue)))
            }
            SideItem::File(i) => {
                let file = &diff.files[*i];
                let reviewed = app.state.is_file_reviewed(diff, *i);
                let (letter, color) = status_letter(file.status);
                let name = file.path.rsplit('/').next().unwrap_or(&file.path);
                let (a, r) = file_stats(diff, *i);
                let stats = format!(" +{a} -{r}");
                let mark = if reviewed { "✓ " } else { "  " };
                let room = width.saturating_sub(4 + stats.len());
                let mut name_style = Style::new();
                if reviewed || file.is_generated {
                    name_style = name_style.add_modifier(Modifier::DIM);
                }
                if *i == app.current_file() && !focused {
                    name_style = name_style.add_modifier(Modifier::BOLD).fg(ACCENT);
                }
                ListItem::new(Line::from(vec![
                    Span::styled(mark, Style::new().fg(Color::Green)),
                    Span::styled(letter, Style::new().fg(color)),
                    " ".into(),
                    Span::styled(truncate_right(name, room), name_style),
                    stats.dim(),
                ]))
            }
        })
        .collect();

    let title = if app.filter.is_empty() {
        format!(" files ({}) ", diff.files.len())
    } else {
        format!(" files /{} ", app.filter)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::new().fg(ACCENT)
        } else {
            Style::new().dim()
        })
        .title(title);
    let list = List::new(items).block(block).highlight_style(if focused {
        Style::new().bg(CURSOR_BG).add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    });
    let mut state = ListState::default().with_selected(Some(app.side_sel));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_diff(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Diff;
    let current = app.current_file();
    let file = &app.review.diff.files[current];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::new().fg(ACCENT)
        } else {
            Style::new().dim()
        })
        .title(format!(" {} ", file.display_path()))
        .title_bottom(
            Line::from(match app.layout {
                Layout::Unified => " unified ",
                Layout::Split => " split ",
            })
            .right_aligned()
            .dim(),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.set_view_height(inner.height as usize);

    let width = inner.width as usize;
    let end = (app.scroll + inner.height as usize).min(app.rows.len());
    let mut lines = Vec::with_capacity(end.saturating_sub(app.scroll));
    for i in app.scroll..end {
        let row = app.rows[i];
        let cursor = focused && i == app.cursor;
        lines.push(render_row(app, row, width, cursor));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_row(app: &mut App, row: Row, width: usize, cursor: bool) -> Line<'static> {
    let hscroll = app.hscroll;
    let App {
        review,
        state,
        highlighter,
        collapsed,
        ..
    } = app;
    let diff = &review.diff;
    let cursor_style = |s: Style| if cursor { s.bg(CURSOR_BG) } else { s };

    match row {
        Row::File(fi) => {
            let file = &diff.files[fi];
            let reviewed = state.is_file_reviewed(diff, fi);
            let (letter, color) = status_letter(file.status);
            let (a, r) = file_stats(diff, fi);
            let mut spans = vec![
                Span::styled(
                    if collapsed[fi] { "▶ " } else { "▼ " },
                    cursor_style(Style::new().fg(ACCENT)),
                ),
                Span::styled(
                    format!("{letter} "),
                    cursor_style(Style::new().fg(color).bold()),
                ),
                Span::styled(file.display_path(), cursor_style(Style::new().bold())),
                format!("  +{a}").fg(Color::Green),
                format!(" -{r}").fg(Color::Red),
            ];
            if reviewed {
                spans.push("  ✓ reviewed".fg(Color::Green));
            }
            if file.is_generated {
                spans.push("  generated".dim());
            }
            if let Some(s) = file
                .similarity
                .filter(|_| file.status == FileStatus::Renamed)
            {
                spans.push(format!("  {s}% similar").dim());
            }
            fill(spans, width, '─', Style::new().dim())
        }
        Row::Note(fi) => {
            let file = &diff.files[fi];
            let text = if file.binary {
                "binary file"
            } else {
                match file.status {
                    FileStatus::Renamed => "renamed without changes",
                    FileStatus::Copied => "copied without changes",
                    FileStatus::Added => "empty file added",
                    FileStatus::Deleted => "empty file deleted",
                    FileStatus::Modified => "mode change only",
                }
            };
            Line::from(Span::styled(
                format!("    {text}"),
                cursor_style(Style::new().dim().italic()),
            ))
        }
        Row::Hunk(h) => {
            let hunk = &diff.hunks[h];
            let reviewed = state.is_hunk_reviewed(diff, h);
            let mut style = Style::new().fg(ACCENT);
            if reviewed {
                style = style.dim();
            }
            let head = format!(
                "@@ -{},{} +{},{} @@ {}",
                hunk.old_range.start,
                hunk.old_range.len,
                hunk.new_range.start,
                hunk.new_range.len,
                hunk.section
            );
            let mark = if reviewed { " ✓ " } else { "   " };
            Line::from(vec![
                Span::styled(mark, cursor_style(Style::new().fg(Color::Green))),
                Span::styled(head, style),
            ])
        }
        Row::Line { hunk, line } => {
            let reviewed = state.is_hunk_reviewed(diff, hunk);
            let hl = highlighter.hunk(diff, hunk);
            let l = &diff.hunks[hunk].lines[line];
            let gutter = format!(
                "{:>5} {:>5} ",
                l.old_line.map(|n| n.to_string()).unwrap_or_default(),
                l.new_line.map(|n| n.to_string()).unwrap_or_default()
            );
            let mut spans = vec![Span::styled(gutter, cursor_style(Style::new().dim()))];
            spans.extend(code_spans(
                l,
                &hl[line],
                width.saturating_sub(12),
                hscroll,
                reviewed,
                false,
            ));
            Line::from(spans)
        }
        Row::Pair { hunk, left, right } => {
            let reviewed = state.is_hunk_reviewed(diff, hunk);
            let hl = highlighter.hunk(diff, hunk);
            let lines = &diff.hunks[hunk].lines;
            let half = width / 2;
            let mut spans = Vec::new();
            // The left half gives one column to the separator.
            for (side, idx, w) in [(0, left, half.saturating_sub(1)), (1, right, width - half)] {
                let text_w = w.saturating_sub(6);
                match idx {
                    Some(i) => {
                        let l = &lines[i];
                        let n = if side == 0 { l.old_line } else { l.new_line };
                        let gutter =
                            format!("{:>5} ", n.map(|n| n.to_string()).unwrap_or_default());
                        spans.push(Span::styled(gutter, cursor_style(Style::new().dim())));
                        spans.extend(code_spans(l, &hl[i], text_w, hscroll, reviewed, true));
                    }
                    None => spans.push(Span::styled(
                        " ".repeat(w),
                        Style::new().fg(Color::DarkGray),
                    )),
                }
                if side == 0 {
                    spans.push("│".dim());
                }
            }
            Line::from(spans)
        }
        Row::Spacer => Line::default(),
    }
}

/// Sign column plus highlighted, horizontally scrolled and padded code.
fn code_spans(
    line: &DiffLine,
    hl: &Spans,
    width: usize,
    hscroll: usize,
    reviewed: bool,
    pad: bool,
) -> Vec<Span<'static>> {
    let (sign, bg) = match line.kind {
        LineKind::Added => ("+", Some(ADDED_BG)),
        LineKind::Removed => ("-", Some(REMOVED_BG)),
        LineKind::Context => (" ", None),
    };
    let base = |s: Style| {
        let s = match bg {
            Some(bg) => s.bg(bg),
            None => s,
        };
        if reviewed {
            s.add_modifier(Modifier::DIM)
        } else {
            s
        }
    };
    let sign_style = match line.kind {
        LineKind::Added => Style::new().fg(Color::Green),
        LineKind::Removed => Style::new().fg(Color::Red),
        LineKind::Context => Style::new(),
    };
    let mut out = vec![Span::styled(sign, base(sign_style))];
    let mut skip = hscroll;
    let mut room = width.saturating_sub(1);
    for (style, text) in hl {
        if room == 0 {
            break;
        }
        let mut chunk = String::new();
        for ch in text.chars() {
            let w = ch.width().unwrap_or(0);
            if skip > 0 {
                skip = skip.saturating_sub(w);
                continue;
            }
            if w > room {
                room = 0;
                break;
            }
            room -= w;
            chunk.push(ch);
        }
        if !chunk.is_empty() {
            out.push(Span::styled(chunk, base(*style)));
        }
    }
    if (pad || bg.is_some()) && room > 0 {
        out.push(Span::styled(" ".repeat(room), base(Style::new())));
    }
    out
}

/// Pads a header line with `ch` up to `width`.
fn fill(mut spans: Vec<Span<'static>>, width: usize, ch: char, style: Style) -> Line<'static> {
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used + 1 < width {
        spans.push(" ".into());
        spans.push(Span::styled(ch.to_string().repeat(width - used - 1), style));
    }
    Line::from(spans)
}

fn truncate_right(s: &str, max: usize) -> String {
    let s = expand_tabs(s);
    if s.chars().count() <= max {
        return s;
    }
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}…")
}

fn truncate_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep: String = s.chars().skip(n - max.saturating_sub(1)).collect();
    format!("…{keep}")
}

const HELP: &[(&str, &str)] = &[
    ("j / k, ↓ / ↑", "move line"),
    ("Ctrl-d / Ctrl-u", "half page down / up"),
    ("Ctrl-f / Ctrl-b", "page down / up"),
    ("gg / G", "top / bottom"),
    ("n / N  ({ })", "next / previous hunk"),
    ("J / K  ([ ])", "next / previous file"),
    ("u", "next unreviewed hunk"),
    ("space", "toggle hunk reviewed, go to next"),
    ("r / v", "toggle file reviewed (folds it)"),
    ("o / za, Enter on header", "fold / unfold file"),
    ("zM / zR", "fold / unfold all"),
    ("s", "unified ↔ split"),
    ("h / l, 0", "scroll horizontally, reset"),
    ("e, Enter on a line", "open in editor (parent nvim if any)"),
    ("Tab", "focus files ↔ diff"),
    ("B", "show / hide file list"),
    ("/", "filter files, Esc to clear"),
    ("q", "quit (state is saved on each change)"),
];

fn render_help(f: &mut Frame, area: Rect) {
    let w = 72.min(area.width);
    let h = (HELP.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| Line::from(vec![format!(" {k:<26}").fg(ACCENT), (*d).into()]))
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
