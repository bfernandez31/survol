//! Rendering of the map mode: the level drawn as boxes and arrows, and the
//! links of the selected box on the right.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use survol_core::graph::Graph;
use survol_core::graph::level::{LevelEdge, LevelNode, NodeRef, Scope};
use unicode_width::UnicodeWidthChar;

use super::rows::{truncate_left, truncate_right};
use super::{pane, wrap};
use crate::app::App;
use crate::theme::theme;
use crate::views::boxes::{self, Wire};
use crate::views::graph::{kind_label, kind_name};
use crate::views::map::MapState;

/// Width of the details pane, when there is room for it.
const DETAILS: u16 = 42;

pub fn render(f: &mut Frame, body: Rect, app: &mut App) {
    let Some(g) = &app.sh.graph else {
        return;
    };
    let map = &mut app.graph.map;
    map.ensure(g);
    let (main, side) = if body.width >= DETAILS * 2 + 20 {
        let [main, side] =
            Split::horizontal([Constraint::Min(20), Constraint::Length(DETAILS)]).areas(body);
        (main, Some(side))
    } else {
        (body, None)
    };

    let block = pane(true).title(title(map));
    let inner = block.inner(main);
    f.render_widget(block, main);
    if map.level.nodes.is_empty() {
        let text = match map.scope() {
            Scope::Root => "  No module in an indexed language.",
            Scope::Module(_) => "  No file here.",
            Scope::File(_) => "  No symbol in this file.",
        };
        f.render_widget(Paragraph::new(text.fg(theme().meta)), inner);
    } else {
        follow(map, inner);
        draw(f.buffer_mut(), inner, map);
    }
    if let Some(side) = side {
        render_details(f, side, g, map);
    }
}

fn title(map: &MapState) -> Line<'static> {
    let t = theme();
    let mut spans = vec![Span::raw(" ")];
    let crumbs = map.breadcrumb();
    for (i, c) in crumbs.iter().enumerate() {
        if i > 0 {
            spans.push(" › ".fg(t.meta));
        }
        let last = i + 1 == crumbs.len();
        spans.push(if last {
            c.clone().bold()
        } else {
            c.clone().fg(t.meta)
        });
    }
    let what = if map.all { "all" } else { "around the changes" };
    spans.push(format!("  {what}").fg(t.meta));
    if map.level.hidden > 0 {
        spans.push(format!(", {} hidden", map.level.hidden).fg(t.meta));
    }
    if map.drawn < map.level.edges.len() {
        spans.push(format!(", {} of {} links drawn", map.drawn, map.level.edges.len()).fg(t.meta));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Scrolls so that the selected box is in `area`, with a margin.
fn follow(map: &mut MapState, area: Rect) {
    let Some(b) = map.drawing.boxes.get(map.selected) else {
        return;
    };
    let axis = |scroll: u16, start: u16, len: u16, view: u16, total: u16, margin: u16| -> u16 {
        if total <= view {
            return 0;
        }
        let mut s = scroll;
        if start < s + margin {
            s = start.saturating_sub(margin);
        }
        if start + len + margin > s + view {
            s = (start + len + margin).saturating_sub(view);
        }
        s.min(total - view)
    };
    map.scroll = (
        axis(map.scroll.0, b.x, b.width, area.width, map.drawing.width, 6),
        axis(
            map.scroll.1,
            b.y,
            b.height,
            area.height,
            map.drawing.height,
            2,
        ),
    );
}

fn draw(buf: &mut Buffer, area: Rect, map: &MapState) {
    let t = theme();
    let (sx, sy) = map.scroll;
    // A cell of the drawing on the screen, when it is in the pane.
    let at = |x: u16, y: u16| -> Option<(u16, u16)> {
        let (x, y) = (x.checked_sub(sx)?, y.checked_sub(sy)?);
        (x < area.width && y < area.height).then(|| (area.x + x, area.y + y))
    };
    let mut put = |x: u16, y: u16, c: char, style: Style| {
        if let Some(cell) = at(x, y).and_then(|p| buf.cell_mut(p)) {
            cell.set_char(c).set_style(style);
        }
    };

    // Every line first, then the ones of the selected box over them: what it
    // uses in one colour, what uses it in another.
    let sel = map.selected;
    let mut lines = |wires: &mut dyn Iterator<Item = &Wire>, style: Style| {
        let wires: Vec<&Wire> = wires.collect();
        let mut masks: HashMap<(u16, u16), u8> = HashMap::new();
        for w in &wires {
            for &(cell, m) in &w.cells {
                *masks.entry(cell).or_default() |= m;
            }
        }
        for ((x, y), m) in masks {
            put(x, y, boxes::glyph(m), style);
        }
        for w in &wires {
            if let Some(((x, y), c)) = w.arrow {
                put(x, y, c, style);
            }
        }
    };
    // The frames, and the arrows between them: those of the selected box's
    // frame that one of its own links goes through take its colours.
    let group = |i: usize| map.groups.get(i).copied();
    let through = |from: usize, to: usize| {
        map.level.edges.iter().any(|e| {
            let ends = (e.from == sel && from == e.from) || (e.to == sel && to == e.to);
            ends && group(e.from) == group(from) && group(e.to) == group(to)
        })
    };
    let links = &map.drawing.links;
    lines(&mut links.iter(), Style::new().fg(t.rule));
    if let Some(home) = group(sel) {
        let node_in = |g: usize| (0..map.groups.len()).find(|&i| map.groups[i] == g);
        let hot = |w: &&Wire, out: bool| {
            let (near, far) = if out { (w.from, w.to) } else { (w.to, w.from) };
            near == home
                && node_in(far).is_some_and(|other| {
                    if out {
                        through(sel, other)
                    } else {
                        through(other, sel)
                    }
                })
        };
        lines(
            &mut links.iter().filter(|w| hot(w, false)),
            Style::new().fg(t.relation).bold(),
        );
        lines(
            &mut links.iter().filter(|w| hot(w, true)),
            Style::new().fg(t.accent).bold(),
        );
    }
    let wires = &map.drawing.wires;
    lines(&mut wires.iter(), Style::new().fg(t.rule));
    lines(
        &mut wires.iter().filter(|w| w.to == sel && w.from != sel),
        Style::new().fg(t.relation).bold(),
    );
    lines(
        &mut wires.iter().filter(|w| w.from == sel),
        Style::new().fg(t.accent).bold(),
    );

    for frame in &map.drawing.frames {
        let r = frame.rect;
        let style = Style::new().fg(t.rule);
        let (x1, y1) = (r.x + r.width - 1, r.y + r.height - 1);
        for x in r.x + 1..x1 {
            put(x, r.y, '─', style);
            put(x, y1, '─', style);
        }
        for y in r.y + 1..y1 {
            put(r.x, y, '│', style);
            put(x1, y, '│', style);
        }
        put(r.x, r.y, '╭', style);
        put(x1, r.y, '╮', style);
        put(r.x, y1, '╰', style);
        put(x1, y1, '╯', style);
        let title = format!(" {} ", frame.title);
        write(&mut put, r.x + 1, r.y, x1, &title, Style::new().fg(t.dir));
    }

    for (i, node) in map.level.nodes.iter().enumerate() {
        let r = map.drawing.boxes[i];
        let selected = i == sel;
        // Around the selected box: what it uses, what uses it, drawn or not.
        let linked =
            |from: usize, to: usize| map.level.edges.iter().any(|e| e.from == from && e.to == to);
        let border = if selected {
            Style::new().fg(t.border_focus).bold()
        } else if linked(sel, i) {
            Style::new().fg(t.accent).bold()
        } else if linked(i, sel) {
            Style::new().fg(t.relation).bold()
        } else if node.outside {
            Style::new().fg(t.meta)
        } else {
            Style::new().fg(t.border)
        };
        // Outside the level: a dashed box.
        let (h, v) = if node.outside {
            ('┄', '┆')
        } else {
            ('─', '│')
        };
        let (x1, y1) = (r.x + r.width - 1, r.y + r.height - 1);
        for x in r.x + 1..x1 {
            put(x, r.y, h, border);
            put(x, y1, h, border);
        }
        for y in r.y + 1..y1 {
            put(r.x, y, v, border);
            put(x1, y, v, border);
            for x in r.x + 1..x1 {
                put(x, y, ' ', Style::new());
            }
        }
        put(r.x, r.y, '╭', border);
        put(x1, r.y, '╮', border);
        put(r.x, y1, '╰', border);
        put(x1, y1, '╯', border);
        // The name in the text colour when the review changes it, greyed
        // otherwise: what changed is on the border.
        let mut text = if node.removed {
            Style::new().fg(t.status_deleted).crossed_out()
        } else if node.changed() && !node.outside {
            Style::new()
        } else {
            Style::new().fg(t.meta)
        };
        if selected {
            text = text.bold().reversed();
        }
        write(&mut put, r.x + 2, r.y + 1, x1, &map.labels[i], text);
        let badge = &map.badges[i];
        if !badge.is_empty() {
            let colour = if node.removed {
                t.status_deleted
            } else {
                t.status_modified
            };
            let badge = format!(" {badge} ");
            let x = x1.saturating_sub(badge.chars().count() as u16 + 1);
            write(&mut put, x, y1, x1, &badge, Style::new().fg(colour));
        }
    }
}

/// Writes `text` from `x`, stopping before `end`.
fn write(
    put: &mut impl FnMut(u16, u16, char, Style),
    x: u16,
    y: u16,
    end: u16,
    text: &str,
    style: Style,
) {
    let mut x = x;
    for c in text.chars() {
        let w = c.width().unwrap_or(0) as u16;
        if w == 0 || x + w > end {
            continue;
        }
        put(x, y, c, style);
        x += w;
    }
}

fn render_details(f: &mut Frame, area: Rect, g: &Graph, map: &MapState) {
    let t = theme();
    let block = pane(false).title(" links ");
    let width = block.inner(area).width as usize;
    let mut lines: Vec<Line> = Vec::new();
    let Some(node) = map.selected_node() else {
        f.render_widget(block, area);
        return;
    };
    let room = width.saturating_sub(1);
    let name = &map.names[map.selected];
    for l in wrap(&name.name, room) {
        lines.push(Line::from(format!(" {l}").bold()));
    }
    if !name.parent.is_empty() {
        lines.push(Line::from(
            format!(" {}", truncate_left(&name.parent, room)).fg(t.dir),
        ));
    }
    for l in wrap(&describe(g, node), room) {
        lines.push(Line::from(format!(" {l}").fg(t.meta)));
    }
    let enter = match (&node.of, node.outside) {
        (NodeRef::Symbol(_), _) => "Enter: its callers, callees and tests",
        (_, true) => "Outside this level. Enter: go there",
        (NodeRef::Module(_), _) => "Enter: its files",
        (NodeRef::File(_), _) => "Enter: its symbols",
    };
    lines.push(Line::from(format!(" {enter}").fg(t.meta).italic()));

    let sel = map.selected;
    let mut section = |title: &str, colour, edges: Vec<(&LevelEdge, usize)>| {
        if edges.is_empty() {
            return;
        }
        lines.push(Line::default());
        lines.push(Line::from(
            format!(" {title} ({})", edges.len()).fg(colour).bold(),
        ));
        for (e, other) in edges {
            let kinds: Vec<String> = e
                .kinds
                .iter()
                .map(|(k, n)| format!("{} {n}", kind_name(k)))
                .collect();
            let name = &map.names[other];
            let count = format!("×{}", e.count);
            let room = width.saturating_sub(count.chars().count() + 4);
            let short = truncate_right(&name.name, room);
            let gap = width.saturating_sub(short.chars().count() + count.chars().count() + 3);
            lines.push(Line::from(vec![
                format!("  {short}").into(),
                " ".repeat(gap).into(),
                count.fg(colour),
            ]));
            let room = width.saturating_sub(5);
            if !name.parent.is_empty() {
                lines.push(Line::from(
                    format!("    {}", truncate_left(&name.parent, room)).fg(t.dir),
                ));
            }
            lines.push(Line::from(
                format!("    {}", truncate_right(&kinds.join(", "), room)).fg(t.meta),
            ));
        }
    };
    let edges = &map.level.edges;
    section(
        "→ uses",
        t.accent,
        edges
            .iter()
            .filter(|e| e.from == sel)
            .map(|e| (e, e.to))
            .collect(),
    );
    section(
        "← used by",
        t.relation,
        edges
            .iter()
            .filter(|e| e.to == sel)
            .map(|e| (e, e.from))
            .collect(),
    );
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// What the box holds, in a few words.
fn describe(g: &Graph, node: &LevelNode) -> String {
    let changes = |n: usize| match n {
        0 => String::new(),
        n => format!(", {n} changed symbol(s)"),
    };
    match &node.of {
        NodeRef::Module(_) => format!("module, {} file(s){}", node.size, changes(node.changes)),
        NodeRef::File(_) => format!("file, {} symbol(s){}", node.size, changes(node.changes)),
        NodeRef::Symbol(s) => {
            let sym = g.symbol(*s);
            let state = if sym.removed {
                ", removed"
            } else if sym.changed {
                ", modified"
            } else {
                ""
            };
            format!("{} line {}{state}", kind_label(sym.kind), sym.line)
        }
    }
}
