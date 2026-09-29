//! Rendering of the whole-file view: the file with its changes in place,
//! and a scrollbar marking the changes and the threads.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use survol_core::model::LineKind;

use super::ask::popup_block;
use super::rows::{RowOpts, Side, code_spans, line_nr, nr_color, render_row};
use crate::app::App;
use crate::highlight::Spans;
use crate::intraline::Words;
use crate::theme::theme;
use crate::views::Row;
use crate::views::fileview::{CodeLine, FileRow, FileView};

/// Changed words of a code line the diff shows.
fn line_words(app: &mut App, c: &CodeLine) -> Words {
    match c.diff {
        Some((h, i)) => app.sh.highlighter.hunk_words(&app.sh.review.diff, h).1[i].clone(),
        None => Words::new(),
    }
}

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
    let bg = cursor.then(|| theme().cursor_bg);
    let mut spans = vec![
        line_nr(c.old, nr_color(c.kind, Side::Old), bg),
        line_nr(c.new, nr_color(c.kind, Side::New), bg),
    ];
    let hl = line_spans(app, v, c);
    let words = line_words(app, c);
    spans.extend(code_spans(
        c.kind,
        &hl,
        &words,
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
                let (no, s) = if side == 0 {
                    (c.old, Side::Old)
                } else {
                    (c.new, Side::New)
                };
                let bg = cursor.then(|| theme().cursor_bg);
                spans.push(line_nr(no, nr_color(c.kind, s), bg));
                let hl = line_spans(app, v, &c);
                let words = line_words(app, &c);
                spans.extend(code_spans(
                    c.kind,
                    &hl,
                    &words,
                    w.saturating_sub(6),
                    v.hscroll,
                    false,
                    true,
                ));
            }
            None => spans.push(Span::raw(" ".repeat(w))),
        }
        if side == 0 {
            spans.push("│".fg(theme().rule));
        }
    }
    spans
}

/// Scrollbar mark of each row: threads, then changes, in the colours of the
/// line numbers and of the note bars.
fn marks(app: &App, v: &FileView) -> Vec<Option<(bool, Color)>> {
    let t = theme();
    v.rows
        .iter()
        .map(|r| match r {
            FileRow::Note { note, part: 0 } => Some((
                true,
                app.sh
                    .notes
                    .items
                    .get(*note as usize)
                    .map_or(t.discussion, |n| n.tone.bar()),
            )),
            FileRow::Code(c) if c.kind == LineKind::Added => Some((false, t.added_line_nr)),
            FileRow::Code(c) if c.kind == LineKind::Removed => Some((false, t.removed_line_nr)),
            FileRow::Pair { right: Some(c), .. } if c.kind == LineKind::Added => {
                Some((false, t.added_line_nr))
            }
            FileRow::Pair { left: Some(c), .. } if c.kind == LineKind::Removed => {
                Some((false, t.removed_line_nr))
            }
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
        .title(Line::from(mode).right_aligned().fg(theme().meta))
        .title_bottom(Line::from(hint).fg(theme().meta));
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
            line = line.patch_style(Style::new().bg(theme().select_bg));
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
                .max_by_key(|(thread, _)| *thread)
                .copied();
            let t = theme();
            match mark {
                Some((_, c)) => Line::from(Span::styled("▐", Style::new().fg(c))),
                None if in_view => Line::from(Span::styled("┃", Style::new().fg(t.meta))),
                None => Line::from("│".fg(t.rule)),
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}
