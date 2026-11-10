//! Rendering of the map mode: the middle box, what uses it on the left, what
//! it uses on the right, arrows only from the left column to the middle and
//! from the middle to the right column; the selected box on the right pane.

use std::collections::BTreeMap;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use survol_core::graph::level::{LevelEdge, LevelNode, NodeRef, Scope};
use survol_core::graph::{EdgeKind, Graph};
use unicode_width::UnicodeWidthChar;

use super::rows::{truncate_left, truncate_right};
use super::{pane, wrap};
use crate::app::App;
use crate::theme::theme;
use crate::views::Scroll;
use crate::views::graph::{kind_label, kind_name};
use crate::views::map::{Column, MapState, Neighbour};

/// Width of the details pane, when there is room for it.
const DETAILS: u16 = 42;
/// Height of a box of a side column.
const BOX: u16 = 3;

// Directions a line leaves a cell in.
const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

pub fn render(f: &mut Frame, body: Rect, app: &mut App) {
    let Some(g) = &app.sh.graph else {
        return;
    };
    let map = &mut app.graph.map;
    map.ensure(g);
    let (main, side) = if body.width >= DETAILS * 2 + 40 {
        let [main, side] =
            Split::horizontal([Constraint::Min(40), Constraint::Length(DETAILS)]).areas(body);
        (main, Some(side))
    } else {
        (body, None)
    };

    let block = pane(true).title(title(map));
    let inner = block.inner(main);
    f.render_widget(block, main);
    if map.center.is_none() {
        let text = match map.scope() {
            Scope::Root => "  No module in an indexed language.",
            Scope::Module(_) => "  No file here.",
            Scope::File(_) => "  No symbol in this file.",
        };
        f.render_widget(Paragraph::new(text.fg(theme().meta)), inner);
    } else if inner.width < 40 || inner.height < 8 {
        f.render_widget(
            Paragraph::new("  Too small for the map.".fg(theme().meta)),
            inner,
        );
    } else {
        draw(f.buffer_mut(), inner, g, map);
    }
    if let Some(side) = side {
        render_details(f, side, g, map);
    }
}

fn title(map: &MapState) -> Line<'static> {
    let t = theme();
    let mut spans = vec![Span::raw(" ")];
    for c in map.breadcrumb() {
        spans.push(truncate_left(&c, 32).fg(t.meta));
        spans.push(" › ".fg(t.meta));
    }
    if let Some(c) = map.center {
        spans.push(map.names[c].name.clone().bold());
    }
    let (at, changed) = map.changed_position();
    if changed > 0 {
        let at = at.map_or("-".to_string(), |i| (i + 1).to_string());
        spans.push(format!("  changed {at}/{changed}").fg(t.meta));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// The boxes of a side column shown in `height` rows: their indexes in the
/// column and their top rows (from 0), and the boxes left out above and
/// below. Over the height, a row at the top and one at the bottom say how
/// many are left out; under it, the boxes are centred.
fn window(pos: &mut Scroll, len: usize, height: u16) -> (Vec<(usize, u16)>, usize, usize) {
    let fits = (height / BOX) as usize;
    let (capacity, first) = if len > fits {
        (((height.saturating_sub(2)) / BOX).max(1) as usize, 1)
    } else {
        (fits, (height - len as u16 * BOX) / 2)
    };
    pos.height = capacity;
    pos.clamp(len);
    let end = (pos.scroll + capacity).min(len);
    let shown = (pos.scroll..end)
        .enumerate()
        .map(|(k, i)| (i, first + k as u16 * BOX))
        .collect();
    (shown, pos.scroll, len - end)
}

/// The cells of the drawing, clipped to the pane.
struct Canvas<'a> {
    buf: &'a mut Buffer,
    area: Rect,
}

impl Canvas<'_> {
    fn put(&mut self, x: u16, y: u16, c: char, style: Style) {
        if self.area.contains((x, y).into())
            && let Some(cell) = self.buf.cell_mut((x, y))
        {
            cell.set_char(c).set_style(style);
        }
    }

    /// Writes `text` from `x`, stopping before `end`.
    fn write(&mut self, x: u16, y: u16, end: u16, text: &str, style: Style) {
        let mut x = x;
        for c in text.chars() {
            let w = c.width().unwrap_or(0) as u16;
            if w == 0 || x + w > end {
                continue;
            }
            self.put(x, y, c, style);
            x += w;
        }
    }

    fn rect(&mut self, r: Rect, dashed: bool, style: Style) {
        let (h, v) = if dashed {
            ('┄', '┆')
        } else {
            ('─', '│')
        };
        let (x1, y1) = (r.x + r.width - 1, r.y + r.height - 1);
        for x in r.x + 1..x1 {
            self.put(x, r.y, h, style);
            self.put(x, y1, h, style);
        }
        for y in r.y + 1..y1 {
            self.put(r.x, y, v, style);
            self.put(x1, y, v, style);
            for x in r.x + 1..x1 {
                self.put(x, y, ' ', Style::new());
            }
        }
        self.put(r.x, r.y, '╭', style);
        self.put(x1, r.y, '╮', style);
        self.put(r.x, y1, '╰', style);
        self.put(x1, y1, '╯', style);
    }
}

/// A line between a side box and the middle one: its cells with the
/// directions it leaves them in, where its label goes, and its arrow head.
struct Wire {
    cells: Vec<(u16, u16, u8)>,
    label: (u16, u16, u16),
    head: (u16, u16),
}

/// The line from row `from` at `x0` to row `to` at `x1`, turning at column
/// `turn`.
fn wire(x0: u16, from: u16, turn: u16, x1: u16, to: u16) -> Vec<(u16, u16, u8)> {
    let vertical = |a: u16, b: u16| match a.cmp(&b) {
        std::cmp::Ordering::Less => DOWN,
        std::cmp::Ordering::Greater => UP,
        std::cmp::Ordering::Equal => 0,
    };
    let mut cells: Vec<(u16, u16, u8)> = (x0..turn).map(|x| (x, from, LEFT | RIGHT)).collect();
    let (out, back) = (vertical(from, to), vertical(to, from));
    if from == to {
        cells.push((turn, from, LEFT | RIGHT));
    } else {
        cells.push((turn, from, LEFT | out));
        cells.extend((from.min(to) + 1..from.max(to)).map(|y| (turn, y, UP | DOWN)));
        cells.push((turn, to, RIGHT | back));
    }
    cells.extend((turn + 1..x1).map(|x| (x, to, LEFT | RIGHT)));
    cells
}

fn glyph(m: u8) -> char {
    match m {
        m if m == UP | DOWN => '│',
        m if m == DOWN | RIGHT => '╭',
        m if m == DOWN | LEFT => '╮',
        m if m == UP | RIGHT => '╰',
        m if m == UP | LEFT => '╯',
        m if m == UP | DOWN | RIGHT => '├',
        m if m == UP | DOWN | LEFT => '┤',
        m if m == LEFT | RIGHT | DOWN => '┬',
        m if m == LEFT | RIGHT | UP => '┴',
        m if m == UP | DOWN | LEFT | RIGHT => '┼',
        m if m & (UP | DOWN) != 0 => '│',
        _ => '─',
    }
}

/// The label of a link: its kind (`+` when there are others), and how many.
fn link_label(e: &LevelEdge) -> String {
    let main = e.kinds.iter().max_by_key(|(_, n)| **n).map(|(k, _)| *k);
    let mut out = main.map_or(String::new(), short_kind);
    if e.kinds.len() > 1 {
        out.push('+');
    }
    if e.count > 1 {
        out.push_str(&format!(" {}", e.count));
    }
    out
}

fn short_kind(k: EdgeKind) -> String {
    match k {
        EdgeKind::Calls => "calls",
        EdgeKind::Imports => "imports",
        EdgeKind::Inherits => "extends",
        EdgeKind::Overrides => "overrides",
        EdgeKind::Uses => "uses",
        EdgeKind::Tests => "tests",
        EdgeKind::Injects => "injects",
        EdgeKind::HttpCalls => "http",
        EdgeKind::Configures => "config",
        EdgeKind::Routes => "routes",
        EdgeKind::Publishes => "events",
    }
    .to_string()
}

fn draw(buf: &mut Buffer, area: Rect, g: &Graph, map: &mut MapState) {
    let t = theme();
    let Some(center) = map.center else {
        return;
    };
    let mut cv = Canvas { buf, area };
    // Columns, and the lanes the arrows take between them.
    // The middle box a little wider than the others.
    let gap = if area.width >= 110 { 18 } else { 12 };
    let side = (area.width - 2 * gap) * 3 / 10;
    let width = area.width - 2 * gap - 2 * side;
    let (lx, cx) = (area.x, area.x + side + gap);
    let rx = cx + width + gap;
    let rw = area.x + area.width - rx;
    let top = area.y + 2;
    let height = area.height - 2;

    let (left_n, right_n) = (map.left.len(), map.right.len());
    cv.write(
        lx,
        area.y,
        lx + side,
        &format!("used by ({left_n})"),
        Style::new().fg(t.relation).bold(),
    );
    cv.write(
        rx,
        area.y,
        rx + rw,
        &format!("uses ({right_n})"),
        Style::new().fg(t.accent).bold(),
    );

    // The middle box: its name, where it lives, what it holds.
    let node = &map.level.nodes[center];
    let name = &map.names[center];
    let room = width.saturating_sub(4) as usize;
    let mut lines: Vec<(String, Style)> = wrap(&name.name, room)
        .into_iter()
        .map(|l| (l, name_style(node).bold()))
        .collect();
    let names = lines.len();
    if !name.parent.is_empty() {
        lines.push((truncate_left(&name.parent, room), Style::new().fg(t.dir)));
    }
    lines.push((describe(g, node), Style::new().fg(t.meta)));
    let ch = lines.len() as u16 + 2;
    let cy = top + height.saturating_sub(ch) / 2;
    let row = cy + 1;

    // The lines: every one, then the selected one over them, bold.
    let (left, la, lb) = window(&mut map.left_pos, left_n, height);
    let (right, ra, rb) = window(&mut map.right_pos, right_n, height);
    let mut wires: Vec<(Wire, Neighbour, bool)> = Vec::new();
    for &(i, y) in &left {
        let (from, turn) = (top + y + 1, cx - 3);
        let w = Wire {
            cells: wire(lx + side, from, turn, cx - 1, row),
            label: (lx + side + 1, from, turn.saturating_sub(1)),
            head: (cx - 1, row),
        };
        wires.push((w, map.left[i], true));
    }
    for &(i, y) in &right {
        let (to, turn) = (top + y + 1, cx + width + 2);
        let w = Wire {
            cells: wire(cx + width, row, turn, rx - 1, to),
            label: (turn + 2, to, rx - 2),
            head: (rx - 1, to),
        };
        wires.push((w, map.right[i], false));
    }
    let mut masks: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    for (w, _, _) in &wires {
        for &(x, y, m) in &w.cells {
            *masks.entry((x, y)).or_default() |= m;
        }
    }
    let selected = map.selected_neighbour();
    let colour = |incoming: bool| if incoming { t.relation } else { t.accent };
    let mut wires: Vec<_> = wires.into_iter().collect();
    // The selected line last, over the others.
    wires.sort_by_key(|(_, n, _)| Some(*n) == selected);
    for (w, n, incoming) in &wires {
        let hot = Some(*n) == selected;
        let style = if hot {
            Style::new().fg(colour(*incoming)).bold()
        } else {
            Style::new().fg(colour(*incoming))
        };
        for &(x, y, _) in &w.cells {
            cv.put(x, y, glyph(masks[&(x, y)]), style);
        }
        cv.put(w.head.0, w.head.1, '▶', style);
        // The kind and the number of links, else only the number.
        let e = &map.level.edges[n.edge];
        let (x, y, end) = w.label;
        let room = end.saturating_sub(x) as usize;
        let mut label = format!(" {} ", link_label(e));
        if label.chars().count() > room {
            label = format!(" ×{} ", e.count);
        }
        if label.chars().count() <= room {
            let style = if hot { style } else { Style::new().fg(t.meta) };
            cv.write(x, y, end, &label, style);
        }
    }

    // The boxes.
    let focus = Style::new().fg(t.border_focus).bold();
    let middle = Rect::new(cx, cy, width, ch);
    let border = if map.column == Column::Center {
        focus
    } else {
        Style::new().fg(t.border).bold()
    };
    cv.rect(middle, false, border);
    for (k, (text, style)) in lines.iter().enumerate() {
        let style = if k < names && map.column == Column::Center {
            style.reversed()
        } else {
            *style
        };
        let text = truncate_right(text, width.saturating_sub(4) as usize);
        cv.write(cx + 2, cy + 1 + k as u16, cx + width - 1, &text, style);
    }
    badge(&mut cv, middle, &map.badges[center], node);
    let sides = [(&left, &map.left, lx, side), (&right, &map.right, rx, rw)];
    for (shown, column, x, w) in sides {
        for &(i, y) in shown {
            let n = column[i];
            let node = &map.level.nodes[n.node];
            let r = Rect::new(x, top + y, w, BOX);
            let hot = Some(n) == selected;
            let border = if hot {
                focus
            } else if node.outside {
                Style::new().fg(t.meta)
            } else {
                Style::new().fg(t.border)
            };
            cv.rect(r, node.outside, border);
            let style = if hot {
                name_style(node).bold().reversed()
            } else {
                name_style(node)
            };
            let text = truncate_right(&map.names[n.node].name, w.saturating_sub(4) as usize);
            cv.write(x + 2, r.y + 1, x + w - 1, &text, style);
            badge(&mut cv, r, &map.badges[n.node], node);
        }
    }
    // What does not fit.
    let more = Style::new().fg(t.meta).italic();
    for (above, below, x, w) in [(la, lb, lx, side), (ra, rb, rx, rw)] {
        if above > 0 {
            cv.write(x + 2, top, x + w, &format!("↑ +{above}"), more);
        }
        if below > 0 {
            cv.write(x + 2, top + height - 1, x + w, &format!("↓ +{below}"), more);
        }
    }
}

/// The name in the text colour when the review changes it, greyed otherwise.
fn name_style(node: &LevelNode) -> Style {
    let t = theme();
    if node.removed {
        Style::new().fg(t.status_deleted).crossed_out()
    } else if node.changed() && !node.outside {
        Style::new()
    } else {
        Style::new().fg(t.meta)
    }
}

/// What changed inside, on the bottom border.
fn badge(cv: &mut Canvas, r: Rect, badge: &str, node: &LevelNode) {
    if badge.is_empty() {
        return;
    }
    let t = theme();
    let colour: Color = if node.removed {
        t.status_deleted
    } else {
        t.status_modified
    };
    let badge = format!(" {badge} ");
    let x1 = r.x + r.width - 1;
    let x = x1.saturating_sub(badge.chars().count() as u16 + 1);
    cv.write(x, r.y + r.height - 1, x1, &badge, Style::new().fg(colour));
}

fn render_details(f: &mut Frame, area: Rect, g: &Graph, map: &MapState) {
    let t = theme();
    let block = pane(false).title(" details ");
    let width = block.inner(area).width as usize;
    let room = width.saturating_sub(1);
    let (Some(node), Some(center)) = (map.selected_node(), map.center) else {
        f.render_widget(block, area);
        return;
    };
    let neighbour = map.selected_neighbour();
    let index = neighbour.map_or(center, |n| n.node);
    let name = &map.names[index];
    let mut lines: Vec<Line> = Vec::new();
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
    let enter = match (&node.of, neighbour.is_some(), node.outside) {
        (_, true, true) => "Outside this level. Enter: follow the link there",
        (_, true, false) => "Enter: to the middle",
        (NodeRef::Symbol(_), false, _) => "Enter: its callers, callees and tests",
        (NodeRef::Module(_), false, _) => "Enter: its files",
        (NodeRef::File(_), false, _) => "Enter: its symbols",
    };
    for l in wrap(enter, room) {
        lines.push(Line::from(format!(" {l}").fg(t.meta).italic()));
    }
    lines.push(Line::default());

    let kinds = |kinds: &BTreeMap<EdgeKind, usize>, lines: &mut Vec<Line>| {
        for (k, n) in kinds {
            lines.push(Line::from(format!("   {} {n}", kind_name(k)).fg(t.meta)));
        }
    };
    let center_name = &map.names[center].name;
    match neighbour {
        Some(n) => {
            let e = map.edge(n);
            let (title, colour) = if map.column == Column::Left {
                (format!("uses {center_name}"), t.relation)
            } else {
                (format!("used by {center_name}"), t.accent)
            };
            lines.push(Line::from(
                format!(
                    " {} ×{}",
                    truncate_right(&title, room.saturating_sub(6)),
                    e.count
                )
                .fg(colour)
                .bold(),
            ));
            kinds(&e.kinds, &mut lines);
        }
        None => {
            // The links of each side, by kind.
            for (title, side, colour) in [
                ("used by", &map.left, t.relation),
                ("uses", &map.right, t.accent),
            ] {
                let mut by_kind: BTreeMap<EdgeKind, usize> = BTreeMap::new();
                let mut count = 0;
                for &n in side {
                    let e = map.edge(n);
                    count += e.count;
                    for (k, c) in &e.kinds {
                        *by_kind.entry(*k).or_default() += c;
                    }
                }
                lines.push(Line::from(
                    format!(" {title}: {} box(es), {count} link(s)", side.len())
                        .fg(colour)
                        .bold(),
                ));
                kinds(&by_kind, &mut lines);
            }
        }
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// What the box holds, in a few words.
fn describe(g: &Graph, node: &LevelNode) -> String {
    let count = |n: usize, what: &str| {
        let s = if n == 1 { "" } else { "s" };
        let changed = match node.changes {
            0 => String::new(),
            c => format!(", {c} changed"),
        };
        format!("{n} {what}{s}{changed}")
    };
    match &node.of {
        NodeRef::Module(_) => count(node.size, "file"),
        NodeRef::File(_) => count(node.size, "symbol"),
        NodeRef::Symbol(s) => {
            let sym = g.symbol(*s);
            let state = if sym.removed {
                ", removed"
            } else if sym.changed {
                ", modified"
            } else {
                ""
            };
            format!("{}, line {}{state}", kind_label(sym.kind), sym.line)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_column_scrolls_with_its_cursor_and_says_what_it_leaves_out() {
        let mut pos = Scroll::default();
        // Three boxes in 15 rows: centred.
        let (shown, above, below) = window(&mut pos, 3, 15);
        assert_eq!(shown, [(0, 3), (1, 6), (2, 9)]);
        assert_eq!((above, below), (0, 0));
        // Ten in 14 rows: four, below a row for what is left out above.
        let (shown, above, below) = window(&mut pos, 10, 14);
        assert_eq!((shown.len(), shown[0], above, below), (4, (0, 1), 0, 6));
        pos.cursor = 9;
        let (shown, above, below) = window(&mut pos, 10, 14);
        assert_eq!((shown.last().unwrap().0, above, below), (9, 6, 0));
    }

    #[test]
    fn lines_join_at_the_turn() {
        // From row 2 down to row 5, turning at column 3.
        let cells = wire(0, 2, 3, 6, 5);
        let at = |x, y| {
            cells
                .iter()
                .find(|c| (c.0, c.1) == (x, y))
                .map(|c| glyph(c.2))
        };
        assert_eq!(at(0, 2), Some('─'));
        assert_eq!(at(3, 2), Some('╮'));
        assert_eq!(at(3, 4), Some('│'));
        assert_eq!(at(3, 5), Some('╰'));
        assert_eq!(at(5, 5), Some('─'));
        // Straight across.
        assert!(wire(0, 2, 3, 6, 2).iter().all(|c| glyph(c.2) == '─'));
        assert_eq!(glyph(UP | DOWN | RIGHT), '├');
    }

    #[test]
    fn a_link_label_names_its_main_kind() {
        let mut e = LevelEdge {
            from: 0,
            to: 1,
            count: 1,
            kinds: BTreeMap::from([(EdgeKind::Calls, 1)]),
        };
        assert_eq!(link_label(&e), "calls");
        e.kinds.insert(EdgeKind::Imports, 2);
        e.count = 3;
        assert_eq!(link_label(&e), "imports+ 3");
    }
}
