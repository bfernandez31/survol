//! Rendering of the Stack view: groups tree, then summary and diff of the
//! selected node.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};

use super::rows::{RowOpts, fill, render_row, status_letter, truncate_right};
use super::{ACCENT, CURSOR_BG, pane, wrap};
use crate::app::{App, GroupStatus};
use crate::views::stack::{Node, StackRow, node_progress};
use crate::views::{Focus, Layout};

/// Most summary lines shown above the diff.
const MAX_SUMMARY_LINES: usize = 6;

pub fn render(f: &mut Frame, body: Rect, app: &mut App) {
    if app.stack.grouping.is_none() {
        return render_status(f, body, app);
    }
    let content = if app.stack.list_hidden {
        body
    } else {
        let [list, main] = Split::horizontal([
            Constraint::Length(super::list_width(body)),
            Constraint::Min(10),
        ])
        .areas(body);
        render_tree(f, list, app);
        main
    };
    render_content(f, content, app);
    if app.stack.show_warnings {
        render_warnings(f, body, app);
    }
}

/// No grouping yet: progress or error.
fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let hunks = app.sh.review.diff.hunks.len();
    let mut lines = vec![Line::default()];
    match &app.group_status {
        GroupStatus::Running {
            since, progress, ..
        } => {
            lines.push(Line::from(
                format!(
                    "  ⟳ Grouping {hunks} hunks by functional capability… {}s",
                    since.elapsed().as_secs()
                )
                .fg(ACCENT)
                .bold(),
            ));
            if !progress.is_empty() {
                lines.push(Line::from(format!("    {progress}").dim()));
            }
            lines.push(Line::default());
            lines.push(Line::from(
                "  The Diff view stays usable meanwhile (Tab or 1).".dim(),
            ));
        }
        GroupStatus::Failed(e) => {
            lines.push(Line::from("  Grouping failed".fg(Color::Red).bold()));
            for l in wrap(e, area.width.saturating_sub(6) as usize) {
                lines.push(Line::from(format!("    {l}")));
            }
            lines.push(Line::default());
            lines.push(Line::from("  R to try again.".dim()));
        }
        GroupStatus::Done => lines.push(Line::from("  No grouping.".dim())),
    }
    f.render_widget(
        Paragraph::new(lines).block(pane(true).title(" stack ")),
        area,
    );
}

fn render_tree(f: &mut Frame, area: Rect, app: &App) {
    let v = &app.stack;
    let Some(g) = &v.grouping else {
        return;
    };
    let (diff, state) = (&app.sh.review.diff, &app.sh.state);
    let focused = v.focus == Focus::List;
    let width = area.width.saturating_sub(2) as usize;
    let n = g.groups.len();
    let num_w = n.to_string().len();

    let items: Vec<ListItem> = v
        .tree
        .iter()
        .map(|&node| {
            let (done, total) = node_progress(g, node, diff, state);
            let complete = done == total;
            let check = if complete { "✓" } else { " " };
            match node {
                Node::Group(gi) => {
                    let group = &g.groups[gi];
                    let arrow = if v.is_group_folded(gi) { "▶" } else { "▼" };
                    let count = format!(" {done}/{total}");
                    let head = format!("{arrow} {:>num_w$}. ", gi + 1);
                    let room = width.saturating_sub(head.chars().count() + count.len() + 2);
                    let mut title = Style::new().bold();
                    if group.mechanical {
                        title = Style::new().italic().fg(Color::Magenta);
                    }
                    if complete {
                        title = title.add_modifier(Modifier::DIM);
                    }
                    let title_text = truncate_right(&group.title, room);
                    let pad = room.saturating_sub(title_text.chars().count());
                    ListItem::new(Line::from(vec![
                        Span::styled(head, Style::new().fg(ACCENT)),
                        Span::styled(title_text, title),
                        " ".repeat(pad).into(),
                        Span::styled(
                            count,
                            Style::new().fg(if complete {
                                Color::Green
                            } else {
                                Color::Yellow
                            }),
                        ),
                        Span::styled(format!(" {check}"), Style::new().fg(Color::Green)),
                    ]))
                }
                Node::Layer { group, layer } => {
                    let l = &g.groups[group].layers[layer];
                    let arrow = if v.is_layer_open(group, layer) {
                        "▼"
                    } else {
                        "▶"
                    };
                    let mut name = Style::new().fg(Color::Blue);
                    if complete {
                        name = name.add_modifier(Modifier::DIM);
                    }
                    ListItem::new(Line::from(vec![
                        format!("   {arrow} ").fg(ACCENT),
                        Span::styled(truncate_right(&l.name, width.saturating_sub(16)), name),
                        format!(" ({})", l.hunk_ids.len()).dim(),
                        Span::styled(format!(" {check}"), Style::new().fg(Color::Green)),
                    ]))
                }
                Node::Hunk { hunk, .. } => {
                    let h = &diff.hunks[hunk];
                    let path = &diff.files[h.file].path;
                    let name = path.rsplit('/').next().unwrap_or(path);
                    let where_ = if h.section.trim().is_empty() {
                        format!(":{}", h.new_range.start)
                    } else {
                        format!(" {}", h.section.trim())
                    };
                    let room = width.saturating_sub(8 + name.chars().count());
                    let mut style = Style::new();
                    if complete {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("     {check} "), Style::new().fg(Color::Green)),
                        Span::styled(name.to_string(), style),
                        Span::styled(truncate_right(&where_, room), style.dim()),
                    ]))
                }
                Node::File { file, .. } => {
                    let fc = &diff.files[file];
                    let (letter, color) = status_letter(fc.status);
                    let name = fc.path.rsplit('/').next().unwrap_or(&fc.path);
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("   {check} "), Style::new().fg(Color::Green)),
                        Span::styled(letter, Style::new().fg(color)),
                        " ".into(),
                        truncate_right(name, width.saturating_sub(7)).dim(),
                    ]))
                }
            }
        })
        .collect();

    let (done, total) = g.progress(diff, state);
    let reviewed_groups = g
        .groups
        .iter()
        .filter(|gr| gr.is_reviewed(diff, state))
        .count();
    let list = List::new(items)
        .block(
            pane(focused)
                .title(format!(" groups {reviewed_groups}/{n} "))
                .title_bottom(
                    Line::from(format!(" {done}/{total} "))
                        .right_aligned()
                        .dim(),
                ),
        )
        .highlight_style(if focused {
            Style::new().bg(CURSOR_BG).add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(Color::Rgb(40, 40, 50))
        });
    let mut ls = ListState::default().with_selected(Some(v.sel));
    f.render_stateful_widget(list, area, &mut ls);
}

fn render_content(f: &mut Frame, area: Rect, app: &mut App) {
    let v = &app.stack;
    let (Some(g), Some(node)) = (&v.grouping, v.selected()) else {
        return;
    };
    let focused = v.focus == Focus::Content;
    let gi = node.group();
    let group = &g.groups[gi];
    let block = pane(focused)
        .title(Line::from(vec![
            format!(" {}/{} ", gi + 1, g.groups.len()).fg(ACCENT),
            format!("{} ", group.title).bold(),
        ]))
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

    // Summary, progress and layers, warnings, then the diff.
    let width = inner.width as usize;
    let (diff, state) = (&app.sh.review.diff, &app.sh.state);
    let mut head: Vec<Line> = wrap(&group.summary, width.saturating_sub(2))
        .into_iter()
        .take(MAX_SUMMARY_LINES)
        .map(|l| Line::from(format!(" {l}").italic()))
        .collect();
    let (done, total) = group.progress(diff, state);
    let mut info = vec![Span::styled(
        format!(" {done}/{total} reviewed"),
        Style::new().fg(if done == total {
            Color::Green
        } else {
            Color::Yellow
        }),
    )];
    let layers: Vec<String> = group
        .layers
        .iter()
        .map(|l| format!("{} {}", l.name, l.hunk_ids.len()))
        .collect();
    if !layers.is_empty() {
        info.push(format!("  ·  {}", layers.join(" · ")).dim());
    }
    if !group.file_ids.is_empty() {
        info.push(format!("  ·  {} file(s) without hunks", group.file_ids.len()).dim());
    }
    head.push(Line::from(info));
    if !g.warnings.is_empty() {
        head.push(Line::from(
            format!(
                " ⚠ {} grouping warning(s) ({:?}), w to show",
                g.warnings.len(),
                g.source
            )
            .fg(Color::Yellow),
        ));
    }
    head.push(fill(Vec::new(), width + 1, '─', Style::new().dim()));
    let head_h = (head.len() as u16).min(inner.height.saturating_sub(3));
    let [top, rest] =
        Split::vertical([Constraint::Length(head_h), Constraint::Min(1)]).areas(inner);
    f.render_widget(Paragraph::new(head), top);

    app.stack.set_view_height(rest.height as usize);
    let v = &app.stack;
    let (scroll, cursor, hscroll) = (v.pos.scroll, v.pos.cursor, v.hscroll);
    let end = (scroll + rest.height as usize).min(v.rows.len());
    let mut lines = Vec::with_capacity(end.saturating_sub(scroll));
    for i in scroll..end {
        let is_cursor = focused && i == cursor;
        let row = app.stack.rows[i];
        lines.push(match row {
            StackRow::Diff(r) => render_row(
                &mut app.sh,
                r,
                RowOpts {
                    width,
                    cursor: is_cursor,
                    hscroll,
                    folded: false,
                },
            ),
            StackRow::Layer { group, layer } => {
                let g = app.stack.grouping.as_ref().expect("grouping shown");
                let l = &g.groups[group].layers[layer];
                let mut style = Style::new().fg(Color::Blue).bold();
                if is_cursor {
                    style = style.bg(CURSOR_BG);
                }
                fill(
                    vec![
                        Span::styled(format!("■ {}", l.name), style),
                        format!("  {} hunk(s)", l.hunk_ids.len()).dim(),
                    ],
                    width,
                    '━',
                    Style::new().fg(Color::Blue).dim(),
                )
            }
            StackRow::File(fi) => {
                let file = &app.sh.review.diff.files[fi];
                let (letter, color) = status_letter(file.status);
                let cur = |s: Style| if is_cursor { s.bg(CURSOR_BG) } else { s };
                let mut spans = vec![
                    Span::styled(format!("  {letter} "), cur(Style::new().fg(color).bold())),
                    Span::styled(file.display_path(), cur(Style::new().bold())),
                ];
                if file.is_generated {
                    spans.push("  generated".dim());
                }
                fill(spans, width, '─', Style::new().dim())
            }
        });
    }
    f.render_widget(Paragraph::new(lines), rest);
}

fn render_warnings(f: &mut Frame, area: Rect, app: &App) {
    let Some(g) = &app.stack.grouping else {
        return;
    };
    let w = 90.min(area.width);
    let mut lines = vec![Line::from(
        format!(
            " source: {:?} · {} LLM call(s){}",
            g.source,
            g.llm_calls,
            if g.from_cache { " · from cache" } else { "" }
        )
        .dim(),
    )];
    for warning in &g.warnings {
        for (i, l) in wrap(warning, w.saturating_sub(6) as usize)
            .into_iter()
            .enumerate()
        {
            let bullet = if i == 0 { " • " } else { "   " };
            lines.push(Line::from(format!("{bullet}{l}")));
        }
    }
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(Color::Yellow))
                .title(" grouping warnings "),
        ),
        rect,
    );
}
