//! Rendering of diff rows, shared by the Diff and Stack views.

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use survol_core::model::{Diff, DiffLine, FileStatus, LineKind};
use unicode_width::UnicodeWidthChar;

use super::{ACCENT, ADDED_BG, CURSOR_BG, REMOVED_BG};
use crate::app::Shared;
use crate::highlight::{Spans, expand_tabs};
use crate::views::Row;
use crate::views::comments::NoteStyle;

pub fn status_letter(s: FileStatus) -> (&'static str, Color) {
    match s {
        FileStatus::Added => ("A", Color::Green),
        FileStatus::Modified => ("M", Color::Yellow),
        FileStatus::Deleted => ("D", Color::Red),
        FileStatus::Renamed => ("R", Color::Magenta),
        FileStatus::Copied => ("C", Color::Magenta),
    }
}

pub fn file_stats(diff: &Diff, f: usize) -> (usize, usize) {
    diff.file_hunks(f)
        .fold((0, 0), |(a, r), h| (a + h.added(), r + h.removed()))
}

/// How to draw a row besides its content.
#[derive(Debug, Clone, Copy)]
pub struct RowOpts {
    pub width: usize,
    pub cursor: bool,
    pub hscroll: usize,
    /// Fold state of a file header.
    pub folded: bool,
    /// Inside a `V` selection.
    pub selected: bool,
}

const SELECT_BG: Color = Color::Rgb(110, 90, 20);

pub fn render_row(sh: &mut Shared, row: Row, o: RowOpts) -> Line<'static> {
    let Shared {
        review,
        state,
        highlighter,
        notes,
        ..
    } = sh;
    let diff = &review.diff;
    let (width, hscroll) = (o.width, o.hscroll);
    let cursor_style = |s: Style| {
        if o.cursor {
            s.bg(CURSOR_BG)
        } else if o.selected {
            s.bg(SELECT_BG)
        } else {
            s
        }
    };

    match row {
        Row::File(fi) => {
            let file = &diff.files[fi];
            let reviewed = state.is_file_reviewed(diff, fi);
            let (letter, color) = status_letter(file.status);
            let (a, r) = file_stats(diff, fi);
            let mut spans = vec![
                Span::styled(
                    if o.folded { "▶ " } else { "▼ " },
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
            let mark = if state.is_file_reviewed(diff, fi) {
                " ✓ "
            } else {
                "   "
            };
            Line::from(vec![
                Span::styled(mark, cursor_style(Style::new().fg(Color::Green))),
                Span::styled(
                    format!(" {text}"),
                    cursor_style(Style::new().dim().italic()),
                ),
            ])
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
        Row::Comment { note, part, .. } => {
            let n = &notes.items[note as usize];
            let (style, text) = &n.lines[part as usize];
            let (bar, text_style) = match style {
                NoteStyle::DraftHead => (Color::Yellow, Style::new().fg(Color::Yellow).bold()),
                NoteStyle::Draft => (Color::Yellow, Style::new().fg(Color::Yellow)),
                NoteStyle::RemoteHead => (Color::Magenta, Style::new().fg(Color::Magenta).bold()),
                NoteStyle::Remote => (Color::Magenta, Style::new()),
                NoteStyle::Resolved => (Color::DarkGray, Style::new().dim()),
                NoteStyle::Reply => (Color::Magenta, Style::new().dim()),
            };
            Line::from(vec![
                Span::styled(" ".repeat(12), cursor_style(Style::new())),
                Span::styled("┃ ", Style::new().fg(bar)),
                Span::styled(
                    truncate_right(text, width.saturating_sub(15)),
                    cursor_style(text_style),
                ),
            ])
        }
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
pub fn fill(mut spans: Vec<Span<'static>>, width: usize, ch: char, style: Style) -> Line<'static> {
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used + 1 < width {
        spans.push(" ".into());
        spans.push(Span::styled(ch.to_string().repeat(width - used - 1), style));
    }
    Line::from(spans)
}

pub fn truncate_right(s: &str, max: usize) -> String {
    let s = expand_tabs(s);
    if s.chars().count() <= max {
        return s;
    }
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}…")
}

pub fn truncate_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep: String = s.chars().skip(n - max.saturating_sub(1)).collect();
    format!("…{keep}")
}
