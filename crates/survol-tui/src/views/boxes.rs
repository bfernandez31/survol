//! Layout of a small graph as boxes and orthogonal arrows on a character
//! grid, read left to right: an edge goes from a box to one on its right.
//!
//! A layered layout: cycles are broken (the offending edges are drawn
//! backwards), each node gets a column (longest path), an edge crossing
//! several columns goes through one-line waypoints, the columns are ordered
//! to limit crossings (barycentre sweeps) and each node is placed next to
//! what it is linked to. Between two columns, the edges leaving one box
//! share a vertical track. Nodes without any edge are laid out in rows
//! under the drawing.

use std::collections::HashMap;

use ratatui::layout::Rect;

/// Height of a box: a border, one line, a border.
pub const BOX_HEIGHT: u16 = 3;
/// Width the unlinked nodes wrap at, when the drawing is narrower.
const MIN_WRAP: u16 = 100;

/// Sides of a cell a line leaves through.
pub const LEFT: u8 = 1;
pub const RIGHT: u8 = 2;
pub const UP: u8 = 4;
pub const DOWN: u8 = 8;

/// The line character for the sides in `mask`.
pub fn glyph(mask: u8) -> char {
    match mask {
        0 => ' ',
        m if m & (UP | DOWN) == 0 => '─',
        m if m & (LEFT | RIGHT) == 0 => '│',
        5 => '╯',
        6 => '╰',
        9 => '╮',
        10 => '╭',
        7 => '┴',
        11 => '┬',
        13 => '┤',
        14 => '├',
        _ => '┼',
    }
}

/// An edge as drawn: the cells of its line and its arrow head.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wire {
    pub from: usize,
    pub to: usize,
    pub cells: Vec<((u16, u16), u8)>,
    pub arrow: Option<((u16, u16), char)>,
}

/// A titled frame around the boxes of a group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    pub rect: Rect,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drawing {
    /// Per node.
    pub boxes: Vec<Rect>,
    /// The edges drawn between boxes.
    pub wires: Vec<Wire>,
    /// Per group ([`layout_grouped`] only).
    pub frames: Vec<Frame>,
    /// The arrows between frames: `from` and `to` are groups.
    pub links: Vec<Wire>,
    pub width: u16,
    pub height: u16,
}

/// Lays out each group of nodes in its own frame, the frames one under the
/// other (a group above the groups it uses, as far as cycles allow). The
/// edges inside a group are drawn in its frame; the edges between two groups
/// become one arrow between their frames, in a margin on the left: it leaves
/// a frame on its first row and enters one on its second.
pub fn layout_grouped(
    widths: &[u16],
    edges: &[(usize, usize)],
    group_of: &[usize],
    titles: &[String],
) -> Drawing {
    let groups = titles.len();
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); groups];
    let mut local = vec![0usize; widths.len()];
    for (v, &g) in group_of.iter().enumerate() {
        local[v] = members[g].len();
        members[g].push(v);
    }
    let mut between: Vec<(usize, usize)> = edges
        .iter()
        .map(|&(a, b)| (group_of[a], group_of[b]))
        .filter(|(a, b)| a != b)
        .collect();
    between.sort_unstable();
    between.dedup();

    let mut out: Vec<Vec<usize>> = vec![Vec::new(); groups];
    for (e, &(a, _)) in between.iter().enumerate() {
        out[a].push(e);
    }
    let mut order = Vec::with_capacity(groups);
    let (mut state, mut reversed) = (vec![0u8; groups], vec![false; between.len()]);
    for g in 0..groups {
        visit(g, &between, &out, &mut state, &mut reversed, &mut order);
    }
    order.reverse();
    // One track per frame with arrows leaving it, a cell for the line and
    // one for the arrow head.
    let sources: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&g| !out[g].is_empty())
        .collect();
    let margin = if between.is_empty() {
        0
    } else {
        sources.len() as u16 + 2
    };

    let mut d = Drawing {
        boxes: vec![Rect::default(); widths.len()],
        frames: vec![Frame::default(); groups],
        ..Drawing::default()
    };
    let mut y = 0u16;
    for &g in &order {
        let w: Vec<u16> = members[g].iter().map(|&v| widths[v]).collect();
        let inner: Vec<(usize, usize)> = edges
            .iter()
            .filter(|&&(a, b)| group_of[a] == g && group_of[b] == g)
            .map(|&(a, b)| (local[a], local[b]))
            .collect();
        let sub = layout(&w, &inner);
        let title = titles[g].chars().count() as u16;
        let rect = Rect::new(margin, y, (sub.width + 4).max(title + 6), sub.height + 2);
        let (ox, oy) = (rect.x + 2, rect.y + 1);
        for (i, &v) in members[g].iter().enumerate() {
            let b = sub.boxes[i];
            d.boxes[v] = Rect::new(b.x + ox, b.y + oy, b.width, b.height);
        }
        for wire in sub.wires {
            d.wires.push(Wire {
                from: members[g][wire.from],
                to: members[g][wire.to],
                cells: wire
                    .cells
                    .into_iter()
                    .map(|((x, y), m)| ((x + ox, y + oy), m))
                    .collect(),
                arrow: wire.arrow.map(|((x, y), c)| ((x + ox, y + oy), c)),
            });
        }
        d.frames[g] = Frame {
            rect,
            title: titles[g].clone(),
        };
        d.width = d.width.max(rect.right());
        d.height = rect.bottom();
        y = rect.bottom() + 1;
    }

    let line = margin as i32 - 2;
    for &(a, b) in &between {
        let track = sources.iter().position(|&g| g == a).unwrap_or(0) as i32;
        let leaves = (line + 1, d.frames[a].rect.y as i32 + 1);
        let enters = (line, d.frames[b].rect.y as i32 + 2);
        let mut cells: HashMap<(i32, i32), u8> = HashMap::new();
        stroke(
            &mut cells,
            &[leaves, (track, leaves.1), (track, enters.1), enters],
        );
        *cells.entry(leaves).or_default() |= RIGHT;
        *cells.entry(enters).or_default() |= RIGHT;
        let mut cells: Vec<((u16, u16), u8)> = cells
            .into_iter()
            .map(|((x, y), m)| ((x as u16, y as u16), m))
            .collect();
        cells.sort_unstable();
        d.links.push(Wire {
            from: a,
            to: b,
            cells,
            arrow: Some((((line + 1) as u16, enters.1 as u16), '▶')),
        });
    }
    d
}

/// A real node, or the waypoint of a long edge.
#[derive(Clone, Copy)]
struct Slot {
    y: i32,
    real: bool,
}

impl Slot {
    fn height(self) -> i32 {
        if self.real { BOX_HEIGHT as i32 } else { 1 }
    }

    /// Row the edges attach to.
    fn port(self) -> i32 {
        self.y + self.height() / 2
    }
}

/// Lays out nodes of the given box widths and the edges `(from, to)`.
pub fn layout(widths: &[u16], edges: &[(usize, usize)]) -> Drawing {
    let n = widths.len();
    let mut linked = vec![false; n];
    for &(a, b) in edges {
        linked[a] = true;
        linked[b] = true;
    }

    // Cycles: an edge back to a node being visited is laid out reversed.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (e, &(a, _)) in edges.iter().enumerate() {
        out[a].push(e);
    }
    let mut reversed = vec![false; edges.len()];
    let mut state = vec![0u8; n];
    let mut topo = Vec::with_capacity(n);
    for root in 0..n {
        visit(root, edges, &out, &mut state, &mut reversed, &mut topo);
    }
    topo.reverse();
    let dag: Vec<(usize, usize)> = edges
        .iter()
        .zip(&reversed)
        .map(|(&(a, b), &r)| if r { (b, a) } else { (a, b) })
        .collect();

    // Columns: longest path, then the sources moved next to what they use.
    let mut rank = vec![0usize; n];
    let mut position = vec![0usize; n];
    for (i, &v) in topo.iter().enumerate() {
        position[v] = i;
    }
    let mut ordered: Vec<(usize, usize)> = dag.clone();
    ordered.sort_by_key(|&(a, _)| position[a]);
    for &(a, b) in &ordered {
        rank[b] = rank[b].max(rank[a] + 1);
    }
    let mut has_pred = vec![false; n];
    for &(_, b) in &dag {
        has_pred[b] = true;
    }
    for v in (0..n).filter(|&v| !has_pred[v]) {
        if let Some(next) = dag.iter().filter(|e| e.0 == v).map(|e| rank[e.1]).min() {
            rank[v] = next - 1;
        }
    }

    // Slots: the linked nodes, then one waypoint per column an edge crosses.
    let columns = (0..n)
        .filter(|&v| linked[v])
        .map(|v| rank[v] + 1)
        .max()
        .unwrap_or(0);
    let mut slots: Vec<Slot> = vec![Slot { y: 0, real: true }; n];
    let mut slot_rank = rank.clone();
    let mut order: Vec<Vec<usize>> = vec![Vec::new(); columns];
    for v in (0..n).filter(|&v| linked[v]) {
        order[rank[v]].push(v);
    }
    // Per edge: the slots it goes through, left to right.
    let mut chains: Vec<Vec<usize>> = Vec::with_capacity(dag.len());
    for &(a, b) in &dag {
        let mut chain = vec![a];
        for (r, column) in order.iter_mut().enumerate().take(rank[b]).skip(rank[a] + 1) {
            slots.push(Slot { y: 0, real: false });
            slot_rank.push(r);
            column.push(slots.len() - 1);
            chain.push(slots.len() - 1);
        }
        chain.push(b);
        chains.push(chain);
    }
    let mut before: Vec<Vec<usize>> = vec![Vec::new(); slots.len()];
    let mut after: Vec<Vec<usize>> = vec![Vec::new(); slots.len()];
    for chain in &chains {
        for pair in chain.windows(2) {
            after[pair[0]].push(pair[1]);
            before[pair[1]].push(pair[0]);
        }
    }

    // Order within the columns: barycentre of the neighbours, both ways.
    let mut index = vec![0usize; slots.len()];
    let reindex = |order: &[Vec<usize>], index: &mut [usize]| {
        for column in order {
            for (i, &s) in column.iter().enumerate() {
                index[s] = i;
            }
        }
    };
    reindex(&order, &mut index);
    for sweep in 0..6 {
        let forward = sweep % 2 == 0;
        let range: Vec<usize> = if forward {
            (1..columns).collect()
        } else {
            (0..columns.saturating_sub(1)).rev().collect()
        };
        for r in range {
            let near = if forward { &before } else { &after };
            let key = |s: usize| -> f64 {
                if near[s].is_empty() {
                    index[s] as f64
                } else {
                    near[s].iter().map(|&p| index[p] as f64).sum::<f64>() / near[s].len() as f64
                }
            };
            let mut keyed: Vec<(f64, usize, usize)> =
                order[r].iter().map(|&s| (key(s), index[s], s)).collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            order[r] = keyed.into_iter().map(|k| k.2).collect();
            reindex(&order, &mut index);
        }
    }

    // Rows: each slot next to what precedes it, without overlap.
    for column in &order {
        let mut bottom = 0i32;
        let mut previous_real = true;
        for (i, &s) in column.iter().enumerate() {
            let slot = slots[s];
            let gap = i32::from(i > 0 && (slot.real || previous_real));
            let wanted = if before[s].is_empty() {
                0
            } else {
                let sum: i32 = before[s].iter().map(|&p| slots[p].port()).sum();
                sum / before[s].len() as i32 - slot.height() / 2
            };
            slots[s].y = wanted.max(bottom + gap);
            bottom = slots[s].y + slot.height();
            previous_real = slot.real;
        }
    }

    // Columns and the gutters between them: one track per box with edges
    // leaving it, plus a cell on each side for the arrow heads.
    let column_width: Vec<i32> = order
        .iter()
        .map(|column| {
            column
                .iter()
                .filter(|&&s| slots[s].real)
                .map(|&s| widths[s] as i32)
                .max()
                .unwrap_or(1)
        })
        .collect();
    let mut column_x = vec![0i32; columns];
    let mut track: HashMap<usize, i32> = HashMap::new();
    let mut x = 0i32;
    for r in 0..columns {
        column_x[r] = x;
        x += column_width[r];
        let mut sources: Vec<usize> = order[r]
            .iter()
            .copied()
            .filter(|&s| after[s].iter().any(|&t| slots[t].port() != slots[s].port()))
            .collect();
        // The lowest box turns first: fewer lines over each other.
        sources.sort_by_key(|&s| std::cmp::Reverse(slots[s].y));
        for (i, s) in sources.iter().enumerate() {
            track.insert(*s, x + 1 + i as i32);
        }
        if r + 1 < columns {
            x += sources.len() as i32 + 3;
        }
    }
    let mut width = x;
    let mut height = slots
        .iter()
        .enumerate()
        .filter(|&(s, _)| s >= n || linked[s])
        .map(|(_, s)| s.y + s.height())
        .max()
        .unwrap_or(0);

    let left = |s: usize| column_x[slot_rank[s]];
    let right = |s: usize| column_x[slot_rank[s]] + column_width[slot_rank[s]] - 1;
    let mut wires = Vec::with_capacity(edges.len());
    for (e, chain) in chains.iter().enumerate() {
        let mut cells: HashMap<(i32, i32), u8> = HashMap::new();
        for pair in chain.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (ya, yb) = (slots[a].port(), slots[b].port());
            let start = (right(a) + 1, ya);
            // A waypoint is crossed: the line goes on to its right side.
            let end = if slots[b].real {
                (left(b) - 1, yb)
            } else {
                (right(b), yb)
            };
            let mut points = vec![start];
            if ya != yb {
                let tx = track[&a];
                points.push((tx, ya));
                points.push((tx, yb));
            }
            points.push(end);
            stroke(&mut cells, &points);
            *cells.entry(start).or_default() |= LEFT;
            if slots[b].real {
                *cells.entry(end).or_default() |= RIGHT;
            }
        }
        let (first, last) = (chain[0], chain[chain.len() - 1]);
        let arrow = if reversed[e] {
            ((right(first) + 1, slots[first].port()), '◀')
        } else {
            ((left(last) - 1, slots[last].port()), '▶')
        };
        let mut cells: Vec<((u16, u16), u8)> = cells
            .into_iter()
            .map(|((x, y), m)| ((x as u16, y as u16), m))
            .collect();
        cells.sort_unstable();
        wires.push(Wire {
            from: edges[e].0,
            to: edges[e].1,
            cells,
            arrow: Some(((arrow.0.0 as u16, arrow.0.1 as u16), arrow.1)),
        });
    }

    let mut boxes: Vec<Rect> = (0..n)
        .map(|v| {
            if linked[v] {
                Rect::new(
                    left(v) as u16,
                    slots[v].y as u16,
                    column_width[rank[v]] as u16,
                    BOX_HEIGHT,
                )
            } else {
                Rect::default()
            }
        })
        .collect();

    // The unlinked nodes, in rows under the drawing.
    let wrap = width.max(MIN_WRAP as i32);
    let (mut x, mut y) = (0i32, if height > 0 { height + 1 } else { 0 });
    for v in (0..n).filter(|&v| !linked[v]) {
        let w = widths[v] as i32;
        if x > 0 && x + w > wrap {
            x = 0;
            y += BOX_HEIGHT as i32 + 1;
        }
        boxes[v] = Rect::new(x as u16, y as u16, w as u16, BOX_HEIGHT);
        x += w + 2;
        width = width.max(x - 2);
        height = y + BOX_HEIGHT as i32;
    }

    Drawing {
        boxes,
        wires,
        width: width as u16,
        height: height as u16,
        ..Drawing::default()
    }
}

/// Depth-first visit: marks the edges closing a cycle, pushes the nodes in
/// post-order.
fn visit(
    v: usize,
    edges: &[(usize, usize)],
    out: &[Vec<usize>],
    state: &mut [u8],
    reversed: &mut [bool],
    post: &mut Vec<usize>,
) {
    if state[v] != 0 {
        return;
    }
    state[v] = 1;
    for &e in &out[v] {
        let to = edges[e].1;
        match state[to] {
            0 => visit(to, edges, out, state, reversed, post),
            1 => reversed[e] = true,
            _ => {}
        }
    }
    state[v] = 2;
    post.push(v);
}

/// Marks the cells of the axis-aligned polyline through `points`.
fn stroke(cells: &mut HashMap<(i32, i32), u8>, points: &[(i32, i32)]) {
    for pair in points.windows(2) {
        let ((mut x, mut y), (tx, ty)) = (pair[0], pair[1]);
        while (x, y) != (tx, ty) {
            let (dx, dy) = ((tx - x).signum(), (ty - y).signum());
            let (ahead, behind) = match (dx, dy) {
                (1, _) => (RIGHT, LEFT),
                (-1, _) => (LEFT, RIGHT),
                (_, 1) => (DOWN, UP),
                _ => (UP, DOWN),
            };
            *cells.entry((x, y)).or_default() |= ahead;
            x += dx;
            y += dy;
            *cells.entry((x, y)).or_default() |= behind;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The nearest box from `from` in a direction, `None` at the edge.
pub fn step(boxes: &[Rect], from: usize, direction: Direction) -> Option<usize> {
    let centre = |r: Rect| {
        (
            r.x as i32 * 2 + r.width as i32,
            r.y as i32 * 2 + r.height as i32,
        )
    };
    let (x, y) = centre(*boxes.get(from)?);
    boxes
        .iter()
        .enumerate()
        .filter(|&(i, r)| i != from && r.width > 0)
        .filter_map(|(i, r)| {
            let (dx, dy) = (centre(*r).0 - x, centre(*r).1 - y);
            // Rows are a third as tall as a box is wide: weigh them up.
            let (along, across) = match direction {
                Direction::Left => (-dx, dy.abs() * 3),
                Direction::Right => (dx, dy.abs() * 3),
                Direction::Up => (-dy * 3, dx.abs()),
                Direction::Down => (dy * 3, dx.abs()),
            };
            (along > 0).then_some((along + across * 2, i))
        })
        .min()
        .map(|(_, i)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drawing as text, boxes named by their index.
    fn draw(widths: &[u16], edges: &[(usize, usize)]) -> String {
        let d = layout(widths, edges);
        let mut grid = vec![vec![' '; d.width as usize]; d.height as usize];
        let mut masks: HashMap<(u16, u16), u8> = HashMap::new();
        for w in &d.wires {
            for &(at, m) in &w.cells {
                *masks.entry(at).or_default() |= m;
            }
        }
        for ((x, y), m) in masks {
            grid[y as usize][x as usize] = glyph(m);
        }
        for w in &d.wires {
            if let Some(((x, y), c)) = w.arrow {
                grid[y as usize][x as usize] = c;
            }
        }
        for (i, r) in d.boxes.iter().enumerate() {
            for y in r.top()..r.bottom() {
                for x in r.left()..r.right() {
                    let edge = y == r.top() || y == r.bottom() - 1;
                    let side = x == r.left() || x == r.right() - 1;
                    grid[y as usize][x as usize] = match (edge, side) {
                        (true, true) => '+',
                        (true, false) => '-',
                        (false, true) => '|',
                        (false, false) => ' ',
                    };
                }
            }
            grid[r.y as usize + 1][r.x as usize + 1] = char::from(b'a' + i as u8);
        }
        grid.iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_chain_reads_left_to_right() {
        assert_eq!(
            draw(&[5, 5, 5], &[(0, 1), (1, 2)]),
            "\
+---+   +---+   +---+
|a  |──▶|b  |──▶|c  |
+---+   +---+   +---+"
        );
    }

    #[test]
    fn edges_leaving_a_box_share_a_track() {
        assert_eq!(
            draw(&[5, 5, 5], &[(0, 1), (0, 2)]),
            "\
+---+    +---+
|a  |─┬─▶|b  |
+---+ │  +---+
      │
      │  +---+
      ╰─▶|c  |
         +---+"
        );
    }

    #[test]
    fn a_long_edge_goes_through_a_waypoint() {
        let text = draw(&[5, 5, 5], &[(0, 1), (1, 2), (0, 2)]);
        // a → c runs under b, on its own line, and ends on c.
        assert_eq!(text.matches('▶').count(), 2, "{text}");
        let d = layout(&[5, 5, 5], &[(0, 1), (1, 2), (0, 2)]);
        let xs: Vec<u16> = d.boxes.iter().map(|b| b.x).collect();
        assert!(xs[0] < xs[1] && xs[1] < xs[2], "{text}");
        let long = &d.wires[2];
        assert!(long.cells.iter().any(|&((x, _), _)| x == xs[1]), "{text}");
    }

    #[test]
    fn a_cycle_is_drawn_with_a_backward_arrow() {
        let text = draw(&[5, 5], &[(0, 1), (1, 0)]);
        assert!(text.contains('▶') && text.contains('◀'), "{text}");
    }

    #[test]
    fn unlinked_nodes_go_under_the_drawing() {
        let d = layout(&[5, 5, 7, 7], &[(0, 1)]);
        assert_eq!(d.boxes[2], Rect::new(0, 4, 7, 3));
        assert_eq!(d.boxes[3], Rect::new(9, 4, 7, 3));
        assert_eq!((d.width, d.height), (16, 7));
    }

    #[test]
    fn groups_get_a_frame_each_and_one_arrow_between_two_frames() {
        // 0, 1 in group 0; 2 in group 1; both nodes of group 0 use node 2.
        let d = layout_grouped(
            &[5, 5, 5],
            &[(0, 1), (0, 2), (1, 2)],
            &[0, 0, 1],
            &["first".to_string(), "second".to_string()],
        );
        let (first, second) = (d.frames[0].rect, d.frames[1].rect);
        // One track, a line cell and the arrow cell on the left.
        assert_eq!(first, Rect::new(3, 0, 17, 5));
        assert_eq!(second, Rect::new(3, 6, 12, 5));
        assert_eq!(d.boxes[0], Rect::new(5, 1, 5, 3));
        assert_eq!(d.boxes[2], Rect::new(5, 7, 5, 3));
        assert_eq!(d.wires.len(), 1);
        assert_eq!((d.wires[0].from, d.wires[0].to), (0, 1));
        assert_eq!(d.links.len(), 1);
        let link = &d.links[0];
        assert_eq!((link.from, link.to), (0, 1));
        // Leaves the first frame on its first row, enters the second on its
        // second row.
        assert_eq!(link.arrow, Some(((2, 8), '▶')));
        assert!(link.cells.contains(&((2, 1), LEFT | RIGHT)));
        assert!(link.cells.contains(&((0, 1), RIGHT | DOWN)));
        assert!(link.cells.contains(&((0, 8), RIGHT | UP)));
        assert_eq!((d.width, d.height), (20, 11));
    }

    #[test]
    fn steps_to_the_nearest_box() {
        let d = layout(&[5, 5, 5], &[(0, 1), (0, 2)]);
        assert_eq!(step(&d.boxes, 0, Direction::Right), Some(1));
        assert_eq!(step(&d.boxes, 1, Direction::Down), Some(2));
        assert_eq!(step(&d.boxes, 2, Direction::Left), Some(0));
        assert_eq!(step(&d.boxes, 0, Direction::Left), None);
    }
}
