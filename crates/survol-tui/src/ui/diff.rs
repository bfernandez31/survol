//! Rendering of the Diff view: file sidebar and continuous diff.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::rows::{RowOpts, file_stats, render_row, status_letter, truncate_left, truncate_right};
use super::{ACCENT, CURSOR_BG, pane};
use crate::app::App;
use crate::views::diff::SideItem;
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

fn render_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let (v, diff) = (&app.diff, &app.sh.review.diff);
    let focused = v.focus == Focus::List;
    let width = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = v
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
                let reviewed = app.sh.state.is_file_reviewed(diff, *i);
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
                if *i == v.current_file() && !focused {
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

    let title = if v.filter.is_empty() {
        format!(" files ({}) ", diff.files.len())
    } else {
        format!(" files /{} ", v.filter)
    };
    let list = List::new(items)
        .block(pane(focused).title(title))
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
