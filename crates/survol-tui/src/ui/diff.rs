//! Rendering of the Diff view: file sidebar and continuous diff.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::graph::clip;
use super::rows::{RowOpts, file_stats, render_row, status_letter, truncate_left};
use super::{ACCENT, CURSOR_BG, pane};
use crate::app::App;
use crate::views::explorer::{self, Role, SideItem};
use crate::views::{Focus, Layout, Row};

pub fn render(f: &mut Frame, body: Rect, app: &mut App) {
    let diff_area = if app.diff.sidebar_visible {
        let [side, main] = Split::horizontal([
            Constraint::Length(super::list_width(body)),
            Constraint::Min(10),
        ])
        .areas(body);
        render_sidebar(f, side, app);
        main
    } else {
        body
    };
    render_diff(f, diff_area, app);
}

/// Left part cut to leave room for the right part, padded between.
fn sides(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let rw: usize = right.iter().map(Span::width).sum();
    let room = width.saturating_sub(rw);
    let mut line = clip(left, room);
    let used = line.width();
    if used < room {
        line.spans.push(" ".repeat(room - used).into());
    }
    line.spans.extend(right);
    line
}

/// `fr.gouv.finances.douane.app` → `fr.gouv…douane.app` when too long.
fn abbreviate_package(p: &str, max: usize) -> String {
    let parts: Vec<&str> = p.split('.').collect();
    if p.chars().count() <= max || parts.len() < 5 {
        return truncate_left(p, max);
    }
    let short = format!(
        "{}.{}…{}.{}",
        parts[0],
        parts[1],
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    );
    truncate_left(&short, max)
}

fn render_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let (v, diff, state) = (&app.diff, &app.sh.review.diff, &app.sh.state);
    let focused = v.focus == Focus::List;
    let width = area.width.saturating_sub(2) as usize;
    let reviewed: Vec<bool> = (0..diff.files.len())
        .map(|i| state.is_file_reviewed(diff, i))
        .collect();
    let flat = v.explorer == explorer::Mode::Flat;
    let items: Vec<ListItem> = v
        .sidebar
        .iter()
        .map(|item| match item {
            SideItem::Dir {
                key,
                depth,
                label,
                note,
                files,
            } => {
                let folded = v.is_dir_folded(key);
                let done = files.iter().filter(|&&f| reviewed[f]).count();
                let mut left = vec![
                    Span::raw("  ".repeat(*depth as usize)),
                    Span::styled(if folded { "▸ " } else { "▾ " }, Style::new().fg(ACCENT)),
                    Span::styled(format!("{label} "), Style::new().fg(Color::Blue)),
                ];
                if !note.is_empty() {
                    left.push(Span::styled(
                        abbreviate_package(note, 28),
                        Style::new().add_modifier(Modifier::DIM),
                    ));
                }
                let mut right = Vec::new();
                if folded {
                    let (a, r) = files.iter().fold((0, 0), |(a, r), &f| {
                        let (x, y) = file_stats(diff, f);
                        (a + x, r + y)
                    });
                    right.push(format!(" {} files +{a} -{r}", files.len()).dim());
                }
                right.push(if done == files.len() {
                    format!(" ✓ {done}/{}", files.len()).fg(Color::Green)
                } else {
                    format!(" {done}/{}", files.len()).dim()
                });
                ListItem::new(sides(left, right, width))
            }
            SideItem::File {
                file: i,
                depth,
                prefix,
                role,
                place,
            } => {
                let file = &diff.files[*i];
                let (letter, color) = status_letter(file.status);
                let name = file.path.rsplit('/').next().unwrap_or(&file.path);
                let (a, r) = file_stats(diff, *i);
                let mut name_style = Style::new();
                if reviewed[*i] || file.is_generated {
                    name_style = name_style.add_modifier(Modifier::DIM);
                }
                if *i == v.current_file() && !focused {
                    name_style = name_style.add_modifier(Modifier::BOLD).fg(ACCENT);
                }
                let mut left = vec![
                    Span::raw("  ".repeat(*depth as usize)),
                    Span::styled(
                        if reviewed[*i] { "✓ " } else { "  " },
                        Style::new().fg(Color::Green),
                    ),
                ];
                if let Role::Test { .. } = role {
                    left.push("└ ⚗ ".dim());
                }
                left.push(Span::styled(letter, Style::new().fg(color)));
                left.push(" ".into());
                if !prefix.is_empty() {
                    left.push(prefix.clone().dim());
                }
                left.push(Span::styled(name.to_string(), name_style));
                match role {
                    Role::Test { by_graph: true } => left.push(" (graph)".dim()),
                    Role::Class { tested: false } => left.push(" ⚠ no test".fg(Color::Yellow)),
                    _ => {}
                }
                let mut stats = format!(" +{a}");
                if r > 0 {
                    stats.push_str(&format!(" -{r}"));
                }
                let right = if flat {
                    // The name first, where it lives after, cut on the left.
                    left.push(stats.dim());
                    let used: usize = left.iter().map(Span::width).sum();
                    let room = width.saturating_sub(used + 2);
                    if room > 4 {
                        vec![format!("  {}", truncate_left(place, room)).dim()]
                    } else {
                        Vec::new()
                    }
                } else {
                    vec![stats.dim()]
                };
                ListItem::new(sides(left, right, width))
            }
        })
        .collect();

    let (done, total) = (reviewed.iter().filter(|r| **r).count(), diff.files.len());
    let title = if v.filter.is_empty() {
        format!(" files ({total}) · {done}/{total} ✓ ")
    } else {
        format!(" files /{} ", v.filter)
    };
    let list = List::new(items)
        .block(
            pane(focused).title(title).title(
                Line::from(format!(" m: {} ", v.explorer.name()))
                    .right_aligned()
                    .dim(),
            ),
        )
        .highlight_style(if focused {
            Style::new().bg(CURSOR_BG).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        });
    let mut state = ListState::default().with_selected(Some(v.side_sel));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_diff(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.diff.focus == Focus::Content;
    let file = &app.sh.review.diff.files[app.diff.current_file()];
    let block = pane(focused)
        .title(format!(" {} ", file.display_path()))
        .title_bottom(
            Line::from(match app.sh.layout {
                Layout::Unified => " unified ",
                Layout::Split => " split ",
            })
            .right_aligned()
            .dim(),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.diff.set_view_height(inner.height as usize);
    super::rows::fit_notes(&mut app.sh, inner.width as usize);
    if app.diff.notes_gen != app.sh.notes.generation {
        app.diff.relayout(&app.sh);
    }

    let v = &app.diff;
    let sel = v.visual.map(|x| (x.min(v.pos.cursor), x.max(v.pos.cursor)));
    let end = (v.pos.scroll + inner.height as usize).min(v.rows.len());
    let mut lines = Vec::with_capacity(end.saturating_sub(v.pos.scroll));
    for i in v.pos.scroll..end {
        let row = app.diff.rows[i];
        let folded = match row {
            Row::File(fi) => app.diff.collapsed[fi],
            _ => false,
        };
        let opts = RowOpts {
            width: inner.width as usize,
            cursor: focused && i == app.diff.pos.cursor,
            hscroll: app.diff.hscroll,
            folded,
            selected: sel.is_some_and(|(a, b)| i >= a && i <= b),
        };
        lines.push(render_row(&mut app.sh, row, opts));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::abbreviate_package;

    #[test]
    fn abbreviates_long_packages_in_the_middle() {
        assert_eq!(
            abbreviate_package("fr.gouv.finances.douane.surveillance.mathieu", 30),
            "fr.gouv…surveillance.mathieu"
        );
        assert_eq!(abbreviate_package("com.acme.app", 30), "com.acme.app");
    }
}
