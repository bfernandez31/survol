//! Rendering of the whole-file view: the file with its changes in place,
//! and a scrollbar marking the changes and the threads.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use survol_core::model::LineKind;

use super::ask::popup_block;
use super::rows::{RowOpts, SELECT_BG, code_spans, render_row};
use super::{ACCENT, CURSOR_BG};
use crate::app::App;
use crate::highlight::Spans;
use crate::views::Row;
use crate::views::fileview::{CodeLine, FileRow, FileView};

/// Highlighted text of a code line: from the head file, or from the hunk
/// for a removed line.
fn line_spans(app: &mut App, v: &FileView, c: &CodeLine) -> Spans {
    if let Some(n) = c.new
        && let Some(s) = v.src.spans.get(n as usize - 1)
    {
        return s.clone();
    }
    match c.diff {
        Some((h, i)) => app.sh.highlighter.hunk(&app.sh.review.diff, h)[i].clone(),
        None => Spans::new(),
    }
}

fn code_line(
    app: &mut App,
    v: &FileView,
    c: &CodeLine,
    width: usize,
    cursor: bool,
) -> Vec<Span<'static>> {
    let n = |x: Option<u32>| x.map(|n| n.to_string()).unwrap_or_default();
    let gutter = Style::new().dim();
    let gutter = if cursor { gutter.bg(CURSOR_BG) } else { gutter };
    let mut spans = vec![Span::styled(
        format!("{:>5} {:>5} ", n(c.old), n(c.new)),
        gutter,
    )];
    let hl = line_spans(app, v, c);
    spans.extend(code_spans(
        c.kind,
        &hl,
        width.saturating_sub(12),
        v.hscroll,
        false,
        false,
    ));
    spans
}

fn pair_line(
    app: &mut App,
    v: &FileView,
    left: Option<CodeLine>,
    right: Option<CodeLine>,
    width: usize,
    cursor: bool,
) -> Vec<Span<'static>> {
    let half = width / 2;
    let mut spans = Vec::new();
    for (side, c, w) in [(0, left, half.saturating_sub(1)), (1, right, width - half)] {
        match c {
            Some(c) => {
                let no = if side == 0 { c.old } else { c.new };
                let mut g = Style::new().dim();
                if cursor {
                    g = g.bg(CURSOR_BG);
                }
                spans.push(Span::styled(
                    format!("{:>5} ", no.map(|n| n.to_string()).unwrap_or_default()),
                    g,
                ));
                let hl = line_spans(app, v, &c);
                spans.extend(code_spans(
                    c.kind,
                    &hl,
                    w.saturating_sub(6),
                    v.hscroll,
                    false,
                    true,
                ));
            }
            None => spans.push(Span::raw(" ".repeat(w))),
        }
        if side == 0 {
            spans.push("│".dim());
        }
    }
    spans
}

/// Colour of the scrollbar mark of each row: threads, then changes.
fn marks(app: &App, v: &FileView) -> Vec<Option<Color>> {
    v.rows
        .iter()
        .map(|r| match r {
            FileRow::Note { note, part: 0 } => Some(
                app.sh
                    .notes
                    .items
                    .get(*note as usize)
                    .map_or(Color::Magenta, |n| n.tone.bar()),
            ),
            FileRow::Code(c) if c.kind == LineKind::Added => Some(Color::Green),
            FileRow::Code(c) if c.kind == LineKind::Removed => Some(Color::Red),
            FileRow::Pair { right: Some(c), .. } if c.kind == LineKind::Added => Some(Color::Green),
            FileRow::Pair { left: Some(c), .. } if c.kind == LineKind::Removed => Some(Color::Red),
            _ => None,
        })
        .collect()
}

pub fn render(f: &mut Frame, area: Rect, app: &mut App, v: &mut FileView) {
    let (changes, threads) = v.counts();
    let title = format!(
        " {} · whole file · {} lines · {changes} change(s) · {threads} thread(s) ",
        v.path,
        v.src.lines.len()
    );
    let mode = format!(
        " {} · removed lines {} ",
        if v.split { "side by side" } else { "unified" },
        if v.show_removed { "shown" } else { "hidden" }
    );
    let hint = " n/N change · ]c/[c thread · d removed · s side by side · c comment · Enter thread · e editor · Esc ";
    let block = popup_block(title)
        .title(Line::from(mode).right_aligned().dim())
        .title_bottom(Line::from(hint).dim());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    if inner.width < 20 || inner.height < 2 {
        return;
    }
    if v.notes_gen != app.sh.notes.generation {
        v.relayout(&app.sh);
    }
    v.set_view_height(inner.height as usize);
    // The last column is the scrollbar.
    let width = inner.width as usize - 1;
    let sel = v.visual.map(|x| (x.min(v.pos.cursor), x.max(v.pos.cursor)));
    let end = (v.pos.scroll + inner.height as usize).min(v.rows.len());
    let mut lines = Vec::with_capacity(inner.height as usize);
    for i in v.pos.scroll..end {
        let cursor = i == v.pos.cursor;
        let selected = sel.is_some_and(|(a, b)| i >= a && i <= b);
        let mut line = match v.rows[i] {
            FileRow::Code(c) => Line::from(code_line(app, v, &c, width, cursor)),
            FileRow::Pair { left, right } => {
                Line::from(pair_line(app, v, left, right, width, cursor))
            }
            FileRow::Note { note, part } => render_row(
                &mut app.sh,
                Row::Comment {
                    hunk: None,
                    line: None,
                    note,
                    part,
                },
                RowOpts {
                    width,
                    cursor,
                    hscroll: 0,
                    folded: false,
                    selected,
                },
            ),
        };
        if selected && !cursor && !matches!(v.rows[i], FileRow::Note { .. }) {
            line = line.patch_style(Style::new().bg(SELECT_BG));
        }
        lines.push(line);
    }
    let text = Rect::new(inner.x, inner.y, inner.width - 1, inner.height);
    f.render_widget(Paragraph::new(lines), text);
    render_scrollbar(
        f,
        Rect::new(inner.right() - 1, inner.y, 1, inner.height),
        app,
        v,
    );
}

/// The position in the file, and where its changes and threads are.
fn render_scrollbar(f: &mut Frame, area: Rect, app: &App, v: &FileView) {
    let h = area.height as usize;
    let n = v.rows.len().max(1);
    let marks = marks(app, v);
    let lines: Vec<Line> = (0..h)
        .map(|y| {
            // Short file: one cell per row; else rows spread over the cells.
            let (from, to) = if n <= h {
                if y >= n {
                    return Line::default();
                }
                (y, y + 1)
            } else {
                (y * n / h, ((y + 1) * n / h).max(y * n / h + 1).min(n))
            };
            let in_view = to > v.pos.scroll && from < v.pos.scroll + h;
            // Threads win over changes.
            let mark = marks[from.min(marks.len())..to.min(marks.len())]
                .iter()
                .flatten()
                .max_by_key(|c| **c != Color::Green && **c != Color::Red)
                .copied();
            match mark {
                Some(c) => Line::from(Span::styled("▐", Style::new().fg(c))),
                None if in_view => Line::from(Span::styled("┃", Style::new().fg(ACCENT).dim())),
                None => Line::from("│".dim()),
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}
