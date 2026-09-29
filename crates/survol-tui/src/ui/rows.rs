//! Rendering of diff rows, shared by the Diff and Stack views.

use std::ops::Range;

use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use survol_core::model::{Diff, FileStatus, LineKind};
use unicode_width::UnicodeWidthChar;

use crate::app::Shared;
use crate::highlight::{Spans, expand_tabs};
use crate::theme::theme;
use crate::views::Row;
use crate::views::comments::NOTE_INDENT;

pub fn status_letter(s: FileStatus) -> (&'static str, Color) {
    let t = theme();
    match s {
        FileStatus::Added => ("A", t.status_added),
        FileStatus::Modified => ("M", t.status_modified),
        FileStatus::Deleted => ("D", t.status_deleted),
        FileStatus::Renamed => ("R", t.status_renamed),
        FileStatus::Copied => ("C", t.status_renamed),
    }
}

pub fn file_stats(diff: &Diff, f: usize) -> (usize, usize) {
    diff.file_hunks(f)
        .fold((0, 0), |(a, r), h| (a + h.added(), r + h.removed()))
}

/// ` +a -r` in the colours of the diff's line numbers, a zero count in the
/// secondary colour; `-r` only when `removed` is given.
pub fn change_counts(added: usize, removed: Option<usize>, style: Style) -> Vec<Span<'static>> {
    let t = theme();
    let color = |n: usize, c: Color| style.fg(if n == 0 { t.meta } else { c });
    let mut v = vec![Span::styled(
        format!(" +{added}"),
        color(added, t.added_line_nr),
    )];
    if let Some(r) = removed {
        v.push(Span::styled(format!(" -{r}"), color(r, t.removed_line_nr)));
    }
    v
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

/// Lays the notes out at the width of a diff pane of `width` columns.
pub fn fit_notes(sh: &mut Shared, width: usize) {
    let w = width.saturating_sub(NOTE_INDENT).max(20);
    sh.notes.set_width(w, &sh.highlighter);
}

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
            s.bg(theme().cursor_bg)
        } else if o.selected {
            s.bg(theme().select_bg)
        } else {
            s
        }
    };

    match row {
        Row::File(fi) => {
            let t = theme();
            let file = &diff.files[fi];
            let reviewed = state.is_file_reviewed(diff, fi);
            let (letter, color) = status_letter(file.status);
            let (a, r) = file_stats(diff, fi);
            // The whole header sits on a band, the cursor's when on it.
            let band = Style::new().bg(if o.cursor {
                t.cursor_bg
            } else {
                t.file_header_bg
            });
            let mut spans = vec![
                Span::styled(if o.folded { "▶ " } else { "▼ " }, band.fg(t.accent)),
                Span::styled(format!("{letter} "), band.fg(color).bold()),
                Span::styled(file.display_path(), band.fg(t.text).bold()),
                Span::styled(" ", band),
            ];
            spans.extend(change_counts(a, Some(r), band));
            if reviewed {
                spans.push(Span::styled("  ✓ reviewed", band.fg(t.ok)));
            }
            if file.is_generated {
                spans.push(Span::styled("  generated", band.fg(t.meta)));
            }
            if let Some(s) = file
                .similarity
                .filter(|_| file.status == FileStatus::Renamed)
            {
                spans.push(Span::styled(format!("  {s}% similar"), band.fg(t.meta)));
            }
            fill(spans, width, ' ', band)
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
                Span::styled(mark, cursor_style(Style::new().fg(theme().ok))),
                Span::styled(
                    format!(" {text}"),
                    cursor_style(Style::new().fg(theme().meta).italic()),
                ),
            ])
        }
        Row::Hunk(h) => {
            let hunk = &diff.hunks[h];
            let reviewed = state.is_hunk_reviewed(diff, h);
            let t = theme();
            let style = Style::new().fg(if reviewed { t.hunk_reviewed } else { t.hunk });
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
                Span::styled(mark, cursor_style(Style::new().fg(t.ok))),
                Span::styled(head, style),
            ])
        }
        Row::Line { hunk, line } => {
            let reviewed = state.is_hunk_reviewed(diff, hunk);
            let (hl, words) = highlighter.hunk_words(diff, hunk);
            let l = &diff.hunks[hunk].lines[line];
            let bg = row_bg(o);
            let mut spans = vec![
                line_nr(l.old_line, nr_color(l.kind, Side::Old), bg),
                line_nr(l.new_line, nr_color(l.kind, Side::New), bg),
            ];
            spans.extend(code_spans(
                l.kind,
                &hl[line],
                &words[line],
                width.saturating_sub(12),
                hscroll,
                reviewed,
                false,
            ));
            Line::from(spans)
        }
        Row::Pair { hunk, left, right } => {
            let reviewed = state.is_hunk_reviewed(diff, hunk);
            let (hl, words) = highlighter.hunk_words(diff, hunk);
            let lines = &diff.hunks[hunk].lines;
            let half = width / 2;
            let mut spans = Vec::new();
            // The left half gives one column to the separator.
            for (side, idx, w) in [(0, left, half.saturating_sub(1)), (1, right, width - half)] {
                let text_w = w.saturating_sub(6);
                match idx {
                    Some(i) => {
                        let l = &lines[i];
                        let (n, s) = if side == 0 {
                            (l.old_line, Side::Old)
                        } else {
                            (l.new_line, Side::New)
                        };
                        spans.push(line_nr(n, nr_color(l.kind, s), row_bg(o)));
                        spans.extend(code_spans(
                            l.kind, &hl[i], &words[i], text_w, hscroll, reviewed, true,
                        ));
                    }
                    None => spans.push(Span::raw(" ".repeat(w))),
                }
                if side == 0 {
                    spans.push("│".fg(theme().rule));
                }
            }
            Line::from(spans)
        }
        Row::Spacer => Line::default(),
        Row::Comment { note, part, .. } => {
            let Some(n) = notes.items.get(note as usize) else {
                return Line::default();
            };
            let Some(line) = n.lines.get(part as usize) else {
                return Line::default();
            };
            let mut spans = vec![
                Span::styled(" ".repeat(12), cursor_style(Style::new())),
                Span::styled("┃ ", Style::new().fg(n.tone.bar())),
            ];
            spans.extend(line.spans.iter().map(|s| {
                let mut s = s.clone();
                s.style = cursor_style(s.style);
                s
            }));
            super::graph::clip(spans, width)
        }
    }
}

/// Background of a row under the cursor or in the selection.
fn row_bg(o: RowOpts) -> Option<Color> {
    if o.cursor {
        Some(theme().cursor_bg)
    } else if o.selected {
        Some(theme().select_bg)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Old,
    New,
}

/// Colour of a line number: the line's own colour on its side of the change.
pub(super) fn nr_color(kind: LineKind, side: Side) -> Color {
    let t = theme();
    match (kind, side) {
        (LineKind::Added, Side::New) => t.added_line_nr,
        (LineKind::Removed, Side::Old) => t.removed_line_nr,
        _ => t.line_nr,
    }
}

/// A 5-wide line number and its separator. Under the cursor or in the
/// selection it takes the terminal's text colour, readable on that background.
pub(super) fn line_nr(n: Option<u32>, color: Color, bg: Option<Color>) -> Span<'static> {
    let text = format!("{:>5} ", n.map(|n| n.to_string()).unwrap_or_default());
    match bg {
        Some(bg) => Span::styled(text, Style::new().bg(bg)),
        None => Span::styled(text, Style::new().fg(color)),
    }
}

/// Sign block, then the highlighted, horizontally scrolled and padded code;
/// `words` (character ranges) get the changed-word background.
pub(super) fn code_spans(
    kind: LineKind,
    hl: &Spans,
    words: &[Range<usize>],
    width: usize,
    hscroll: usize,
    reviewed: bool,
    pad: bool,
) -> Vec<Span<'static>> {
    let t = theme();
    let (sign, sign_style, bg, word_bg) = match kind {
        LineKind::Added => (
            "+",
            Style::new().fg(t.added_sign).bg(t.added_sign_bg).bold(),
            Some(t.added_bg),
            t.added_word_bg,
        ),
        LineKind::Removed => (
            "-",
            Style::new().fg(t.removed_sign).bg(t.removed_sign_bg).bold(),
            Some(t.removed_bg),
            t.removed_word_bg,
        ),
        LineKind::Context => (" ", Style::new(), None, Color::Reset),
    };
    let base = |s: Style, word: bool| {
        let s = match bg {
            Some(_) if word => s.bg(word_bg),
            Some(bg) => s.bg(bg),
            None => s,
        };
        if reviewed { s.fg(t.reviewed) } else { s }
    };
    let in_word = |col: usize| words.iter().any(|r| r.contains(&col));
    let mut out = vec![
        Span::styled(sign, sign_style),
        Span::styled(" ", base(Style::new(), false)),
    ];
    let mut skip = hscroll;
    let mut room = width.saturating_sub(2);
    let mut col = 0;
    for (style, text) in hl {
        if room == 0 {
            break;
        }
        let mut chunk = String::new();
        let mut word = false;
        for ch in text.chars() {
            let c = col;
            col += 1;
            let w = ch.width().unwrap_or(0);
            if skip > 0 {
                skip = skip.saturating_sub(w);
                continue;
            }
            if w > room {
                room = 0;
                break;
            }
            let wd = in_word(c);
            if wd != word && !chunk.is_empty() {
                out.push(Span::styled(std::mem::take(&mut chunk), base(*style, word)));
            }
            word = wd;
            room -= w;
            chunk.push(ch);
        }
        if !chunk.is_empty() {
            out.push(Span::styled(chunk, base(*style, word)));
        }
    }
    if (pad || bg.is_some()) && room > 0 {
        out.push(Span::styled(" ".repeat(room), base(Style::new(), false)));
    }
    out
}

/// Pads a header line with `ch` up to `width`.
pub fn fill(mut spans: Vec<Span<'static>>, width: usize, ch: char, style: Style) -> Line<'static> {
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used + 1 < width {
        spans.push(Span::styled(" ", style));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_letters_and_counts_follow_the_diff_colours() {
        let t = theme();
        assert_eq!(status_letter(FileStatus::Added).1, t.status_added);
        assert_eq!(status_letter(FileStatus::Deleted).1, t.status_deleted);
        assert_eq!(status_letter(FileStatus::Copied).1, t.status_renamed);
        let c = change_counts(3, Some(0), Style::new().bg(t.file_header_bg));
        assert_eq!(c[0].content, " +3");
        assert_eq!(c[0].style.fg, Some(t.added_line_nr));
        assert_eq!(c[0].style.bg, Some(t.file_header_bg));
        // A zero count is secondary text.
        assert_eq!(c[1].content, " -0");
        assert_eq!(c[1].style.fg, Some(t.meta));
        assert_eq!(change_counts(0, None, Style::new()).len(), 1);
        let d = change_counts(1, Some(2), Style::new());
        assert_eq!(d[1].style.fg, Some(t.removed_line_nr));
    }
}
