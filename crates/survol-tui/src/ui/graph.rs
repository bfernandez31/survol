//! Rendering of the Graph view: list / tree / module map on the left, code
//! preview (or module details) on the right.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use survol_core::graph::{Graph, ModuleMap, SymIdx};
use unicode_width::UnicodeWidthChar;

use super::rows::truncate_right;
use super::{ACCENT, ADDED_BG, CURSOR_BG, pane, wrap};
use crate::app::{App, GraphStatus};
use crate::highlight::Spans;
use crate::views::Focus;
use crate::views::graph::{
    GraphView, ListRow, ModRow, Mode, Target, TreeRow, impact, kind_label, kind_name,
};

pub fn render(f: &mut Frame, body: Rect, app: &mut App) {
    if app.sh.graph.is_none() {
        return render_status(f, body, app);
    }
    if app.graph.mode == Mode::Flows {
        app.graph.flows.tick(&mut app.sh, &app.cfg);
    }
    let content = if app.graph.list_hidden {
        body
    } else {
        // Flows: the flow tree needs the room.
        let w = if app.graph.mode == Mode::Flows {
            (body.width * 2 / 5).clamp(30, 72)
        } else {
            (body.width / 2).clamp(30, 90)
        }
        .min(body.width.saturating_sub(20));
        let [list, main] =
            Split::horizontal([Constraint::Length(w), Constraint::Min(10)]).areas(body);
        render_list(f, list, app);
        main
    };
    render_preview(f, content, app);
}

/// No graph yet: progress or error.
fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let mut lines = vec![Line::default()];
    match &app.graph_status {
        GraphStatus::Running { since, progress } => {
            lines.push(Line::from(
                format!(
                    "  ⟳ Building the code graph (tree-sitter)… {}s",
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
                "  The Diff and Stack views stay usable meanwhile (1, 2).".dim(),
            ));
        }
        GraphStatus::Failed(e) => {
            lines.push(Line::from("  Graph failed".fg(Color::Red).bold()));
            for l in wrap(e, area.width.saturating_sub(6) as usize) {
                lines.push(Line::from(format!("    {l}")));
            }
        }
        _ => lines.push(Line::from("  No graph.".dim())),
    }
    f.render_widget(
        Paragraph::new(lines).block(pane(true).title(" graph ")),
        area,
    );
}

/// Change state of a symbol: label and colour.
fn status(g: &Graph, s: SymIdx) -> (&'static str, Color) {
    let sym = g.symbol(s);
    if sym.removed {
        ("removed", Color::Red)
    } else if sym.changed {
        ("modified", Color::Yellow)
    } else if g.is_file_changed(&sym.file) {
        ("in diff", Color::DarkGray)
    } else {
        ("intact", Color::Magenta)
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `dir/File.java`: the file and its directory.
fn short_path(path: &str) -> &str {
    match path.rmatch_indices('/').nth(1) {
        Some((i, _)) => &path[i + 1..],
        None => path,
    }
}

/// `left`, then `right` against the right edge; `left` is cut to make room.
pub(super) fn two_sided(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> Vec<Span<'static>> {
    let rw: usize = right.iter().map(Span::width).sum();
    let mut line = clip(left, width.saturating_sub(rw + 1)).spans;
    let used: usize = line.iter().map(Span::width).sum();
    line.push(" ".repeat(width.saturating_sub(used + rw)).into());
    line.extend(right);
    line
}

/// Roles, then tags, as small badges.
fn badges(g: &Graph, s: SymIdx) -> Vec<Span<'static>> {
    let sym = g.symbol(s);
    let mut out = Vec::new();
    for r in &sym.roles {
        out.push(Span::styled(
            format!(" [{}]", kind_name(r)),
            Style::new().fg(Color::Blue),
        ));
    }
    for (k, v) in &sym.tags {
        out.push(Span::styled(
            format!(" {k}={}", truncate_right(v, 30)),
            Style::new().fg(Color::Blue).dim(),
        ));
    }
    out
}

/// Callers count, highlighting those in untouched files.
fn impact_spans(callers: usize, untouched: usize) -> Vec<Span<'static>> {
    if callers == 0 {
        return vec![" · no caller".dim()];
    }
    let mut v = vec![format!(" · {callers} caller(s)").dim()];
    if untouched > 0 {
        v.push(Span::styled(
            format!(" {untouched} untouched"),
            Style::new().fg(Color::Magenta).bold(),
        ));
    }
    v
}

/// Cuts `spans` to `width` columns, from `width`-limited overflow.
pub(super) fn clip(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut room = width;
    let mut out = Vec::new();
    for s in spans {
        if room == 0 {
            break;
        }
        let w = s.width();
        if w <= room {
            room -= w;
            out.push(s);
        } else {
            let text = truncate_right(&s.content, room);
            room = 0;
            out.push(Span::styled(text, s.style));
        }
    }
    Line::from(out)
}

pub(super) fn cursor_line(line: Line<'static>, cursor: bool, width: usize) -> Line<'static> {
    if !cursor {
        return line;
    }
    let used = line.width();
    let mut spans = line.spans;
    if used < width {
        spans.push(" ".repeat(width - used).into());
    }
    Line::from(spans).style(Style::new().bg(CURSOR_BG).add_modifier(Modifier::BOLD))
}

fn render_list(f: &mut Frame, area: Rect, app: &mut App) {
    let v = &app.graph;
    let focused = v.focus == Focus::List;
    let g = app.sh.graph.as_ref().expect("graph shown");
    let title = list_title(g, v);
    let block = pane(focused).title(title).title_bottom(
        Line::from(format!(" {} · m mode ", v.mode.name()))
            .right_aligned()
            .dim(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.graph.set_list_height(inner.height as usize);

    let v = &app.graph;
    let pos = v.pos();
    let width = inner.width as usize;
    let end = (pos.scroll + inner.height as usize).min(v.len());
    let mut lines = Vec::new();
    let in_out = v.modules.as_ref().map(module_degrees);
    for i in pos.scroll..end {
        let spans = match v.mode {
            Mode::Changed => list_row(g, v, &v.list[i]),
            Mode::Found => list_row(g, v, &v.found[i]),
            Mode::Modules => mod_row(g, v, v.mod_rows[i], in_out.as_deref()),
            Mode::Flows => super::flows::entry_row(&v.flows, v.flows.rows[i], width),
            Mode::Symbol => tree_row(g, &v.tree[i], width),
        };
        lines.push(cursor_line(
            clip(spans, width),
            i == pos.cursor && focused,
            width,
        ));
        if i == pos.cursor && !focused {
            let l = lines.pop().expect("just pushed");
            lines.push(l.style(Style::new().bg(Color::Rgb(40, 40, 50))));
        }
    }
    if v.len() == 0 {
        lines.push(Line::from(
            match v.mode {
                Mode::Changed => "  No changed symbol in an indexed language.",
                Mode::Flows if !v.flows.computed => "  Computing flows…",
                Mode::Flows => "  No entry point reaches a changed symbol.",
                _ => "  Nothing here.",
            }
            .dim(),
        ));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn list_title(g: &Graph, v: &GraphView) -> Line<'static> {
    match v.mode {
        Mode::Changed => {
            let n = v.impact.len();
            let hit = v.impact.values().filter(|(_, u)| *u > 0).count();
            Line::from(vec![
                format!(" changed symbols ({n}) ").bold(),
                Span::styled(
                    format!("{hit} called from untouched files "),
                    Style::new().fg(Color::Magenta),
                ),
            ])
        }
        Mode::Found => Line::from(format!(" search `{}` ", v.query).bold()),
        Mode::Flows => super::flows::list_title(&v.flows),
        Mode::Modules => {
            let n = v.modules.as_ref().map_or(0, |m| m.modules.len());
            let c = v
                .modules
                .as_ref()
                .map_or(0, |m| m.modules.iter().filter(|x| x.changed()).count());
            Line::from(format!(" modules ({n}, {c} changed) ").bold())
        }
        Mode::Symbol => {
            let Some(st) = &v.symbol else {
                return Line::default();
            };
            let (label, color) = status(g, st.root);
            let mut spans = vec![
                format!(" {} ", g.display_name(st.root)).bold(),
                Span::styled(format!("[{label}] "), Style::new().fg(color)),
            ];
            // Breadcrumb: previous symbols, most recent first.
            let crumbs: Vec<String> = v
                .back
                .iter()
                .rev()
                .filter_map(|(_, s)| s.as_ref().map(|s| g.display_name(s.root)))
                .take(3)
                .collect();
            for c in crumbs {
                spans.push(format!("‹ {} ", truncate_right(&c, 24)).dim());
            }
            Line::from(spans)
        }
    }
}

fn list_row(g: &Graph, v: &GraphView, row: &ListRow) -> Vec<Span<'static>> {
    match row {
        ListRow::Module(m) => vec![Span::styled(
            format!("▣ {}", if m.is_empty() { "." } else { m }),
            Style::new().fg(Color::Blue).bold(),
        )],
        ListRow::File(p) => {
            let mut v = vec![
                "  ".into(),
                Span::styled(file_name(p).to_string(), Style::new().bold()),
            ];
            if !g.is_file_changed(p) {
                v.push("  intact".fg(Color::Magenta));
            }
            v
        }
        ListRow::Symbol(s) => {
            let sym = g.symbol(*s);
            let (label, color) = status(g, *s);
            let mut spans = vec![
                format!("    {:<6} ", kind_label(sym.kind)).dim(),
                Span::styled(g.display_name(*s), Style::new().fg(ACCENT)),
                format!(":{}", sym.line).dim(),
            ];
            if v.mode == Mode::Found || label == "removed" {
                spans.push(Span::styled(format!(" {label}"), Style::new().fg(color)));
            }
            let (callers, untouched) = v.impact.get(s).copied().unwrap_or_else(|| impact(g, *s));
            if !sym.removed {
                spans.extend(impact_spans(callers, untouched));
            }
            spans.extend(badges(g, *s));
            spans
        }
    }
}

/// Incoming and outgoing dependency counts of each module.
fn module_degrees(map: &ModuleMap) -> Vec<(usize, usize)> {
    let mut v = vec![(0, 0); map.modules.len()];
    for e in &map.edges {
        v[e.to].0 += e.count;
        v[e.from].1 += e.count;
    }
    v
}

fn mod_row(
    g: &Graph,
    v: &GraphView,
    row: ModRow,
    degrees: Option<&[(usize, usize)]>,
) -> Vec<Span<'static>> {
    let Some(map) = &v.modules else {
        return Vec::new();
    };
    match row {
        ModRow::Module(i) => {
            let m = &map.modules[i];
            let (inc, out) = degrees.map_or((0, 0), |d| d[i]);
            let mut name = Style::new();
            if m.changed() {
                name = name.fg(Color::Yellow).bold();
            } else {
                name = name.dim();
            }
            if m.test {
                name = name.italic();
            }
            let arrow = if m.changed_symbols == 0 {
                "  "
            } else if v.is_module_open(i) {
                "▾ "
            } else {
                "▸ "
            };
            vec![
                arrow.fg(ACCENT),
                Span::styled(
                    if m.name.is_empty() {
                        ".".into()
                    } else {
                        m.name.clone()
                    },
                    name,
                ),
                format!("  {}/{} files", m.changed_files, m.files).dim(),
                if m.changed_symbols > 0 {
                    format!("  Δ{}", m.changed_symbols).fg(Color::Yellow)
                } else {
                    "".into()
                },
                format!("  ←{inc} →{out}").fg(Color::Blue),
            ]
        }
        ModRow::Symbol { sym, .. } => {
            let s = g.symbol(sym);
            let (c, u) = v.impact.get(&sym).copied().unwrap_or((0, 0));
            let mut spans = vec![
                format!("    {:<6} ", kind_label(s.kind)).dim(),
                Span::styled(g.display_name(sym), Style::new().fg(ACCENT)),
                format!("  {}:{}", file_name(&s.file), s.line).dim(),
            ];
            spans.extend(impact_spans(c, u));
            spans
        }
    }
}

fn tree_row(g: &Graph, row: &TreeRow, width: usize) -> Vec<Span<'static>> {
    match row {
        TreeRow::Root(s) => {
            let sym = g.symbol(*s);
            let (label, color) = status(g, *s);
            let mut v = vec![
                "● ".fg(ACCENT),
                Span::styled(g.display_name(*s), Style::new().bold()),
                format!("  {}", kind_label(sym.kind)).dim(),
                Span::styled(format!("  {label}"), Style::new().fg(color)),
            ];
            v.extend(badges(g, *s));
            v
        }
        TreeRow::Section { rel, count, folded } => vec![
            if *folded { " ▸ " } else { " ▾ " }.fg(ACCENT),
            Span::styled(
                format!("{} ({count})", rel.label()),
                Style::new().bold().fg(if *count == 0 {
                    Color::DarkGray
                } else {
                    Color::Blue
                }),
            ),
        ],
        TreeRow::Node {
            rel,
            link,
            depth,
            expandable,
            expanded,
            cycle,
            ..
        } => {
            let s = link.symbol;
            let sym = g.symbol(s);
            let marker = if *cycle {
                "↻ "
            } else if *expanded {
                "▾ "
            } else if *expandable {
                "▸ "
            } else {
                "· "
            };
            let line = if rel.at_reference() && link.line > 0 {
                link.line
            } else {
                sym.line
            };
            let (label, color) = status(g, s);
            let mut left = vec![
                format!("{}{marker}", "  ".repeat(depth + 1)).fg(ACCENT),
                Span::styled(g.display_name(s), Style::new().fg(ACCENT)),
            ];
            if let Some(via) = link.via {
                left.push(format!(" via {}", g.display_name(via)).italic().dim());
            }
            left.push(format!("  {}:{line}", short_path(&sym.file)).dim());
            let mut right = Vec::new();
            if link.confidence < 0.995 {
                right.push(format!("{:.2} ", link.confidence).fg(Color::Yellow).dim());
            }
            right.push(Span::styled(format!("{label:<8}"), Style::new().fg(color)));
            two_sided(left, right, width)
        }
    }
}

// ----- preview -----------------------------------------------------------

fn render_preview(f: &mut Frame, area: Rect, app: &mut App) {
    // Flows: the flow above, the code of its selected step below.
    let area = if app.graph.mode == Mode::Flows {
        let [top, bottom] =
            Split::vertical([Constraint::Percentage(62), Constraint::Min(5)]).areas(area);
        super::flows::render_flow(f, top, app);
        bottom
    } else {
        area
    };
    let focused = app.graph.focus == Focus::Content && app.graph.mode != Mode::Flows;
    let g = app.sh.graph.as_ref().expect("graph shown");
    let target = app.graph.target(g);
    let Some(t) = target else {
        return render_module_details(f, area, app, focused);
    };
    let block = pane(focused).title(format!(" {}:{} ", t.file, t.line));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width as usize;

    let mut head = info_lines(app, &t, width);
    head.push(super::rows::fill(
        Vec::new(),
        width + 1,
        '─',
        Style::new().dim(),
    ));
    let head_h = (head.len() as u16).min(inner.height.saturating_sub(3));
    let [top, rest] =
        Split::vertical([Constraint::Length(head_h), Constraint::Min(1)]).areas(inner);
    f.render_widget(Paragraph::new(head), top);

    let (offset, hscroll) = (app.graph.preview_offset, app.graph.hscroll);
    let Some(src) = app.graph.source(&mut app.sh, &t) else {
        f.render_widget(
            Paragraph::new(Line::from(" cannot read this file".dim())),
            rest,
        );
        return;
    };
    let h = rest.height as usize;
    let n = src.lines.len();
    let focus_line =
        (t.line as i64 - 1 + offset as i64).clamp(0, n.saturating_sub(1) as i64) as usize;
    let start = focus_line.saturating_sub(h / 3).min(n.saturating_sub(h));
    let num_w = n.to_string().len().max(3);
    let mut lines = Vec::with_capacity(h);
    for i in start..(start + h).min(n) {
        let no = i as u32 + 1;
        let is_target = no == t.line;
        let is_cursor = focused && i == focus_line;
        let changed = src.changed.contains(&no);
        let mut bg = None;
        if changed {
            bg = Some(ADDED_BG);
        }
        if is_target || is_cursor {
            bg = Some(CURSOR_BG);
        }
        let marker = if is_target { "▶" } else { " " };
        let sign = if changed { "+" } else { " " };
        let mut spans = vec![
            Span::styled(marker, Style::new().fg(ACCENT).bold()),
            Span::styled(
                format!("{no:>num_w$} "),
                if is_target {
                    Style::new().fg(ACCENT).bold()
                } else {
                    Style::new().dim()
                },
            ),
            Span::styled(sign, Style::new().fg(Color::Green)),
        ];
        spans.extend(code(
            &src.spans[i],
            width.saturating_sub(num_w + 3),
            hscroll,
            bg,
        ));
        let mut line = Line::from(spans);
        if let Some(bg) = bg {
            line = line.style(Style::new().bg(bg));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), rest);
}

/// Highlighted code, scrolled and cut to `width`, padded when coloured.
fn code(hl: &Spans, width: usize, hscroll: usize, bg: Option<Color>) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut skip = hscroll;
    let mut room = width;
    for (style, text) in hl {
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
            let s = match bg {
                Some(bg) => style.bg(bg),
                None => *style,
            };
            out.push(Span::styled(chunk, s));
        }
        if room == 0 {
            break;
        }
    }
    if let Some(bg) = bg
        && room > 0
    {
        out.push(Span::styled(" ".repeat(room), Style::new().bg(bg)));
    }
    out
}

/// What the selected node is: kind, name, state, link details, impact.
fn info_lines(app: &App, t: &Target, width: usize) -> Vec<Line<'static>> {
    let g = app.sh.graph.as_ref().expect("graph shown");
    let v = &app.graph;
    let sym = g.symbol(t.sym);
    let (label, color) = status(g, t.sym);
    let mut first = vec![
        format!(" {} ", kind_label(sym.kind)).dim(),
        Span::styled(g.display_name(t.sym), Style::new().bold()),
        Span::styled(format!("  {label}"), Style::new().fg(color)),
    ];
    first.extend(badges(g, t.sym));
    let mut lines = vec![clip(first, width)];

    let mut second: Vec<Span<'static>> = Vec::new();
    if v.mode == Mode::Symbol
        && let Some(TreeRow::Node { rel, link, .. }) = v.tree.get(v.pos().cursor)
    {
        second.push(format!(" {} ", rel.label()).fg(Color::Blue));
        second.push(format!("· {}", kind_name(link.kind)).dim());
        if link.confidence < 0.995 {
            second.push(format!(" · confidence {:.2}", link.confidence).fg(Color::Yellow));
        }
        if let Some(via) = link.via {
            second.push(format!(" · via {}", g.display_name(via)).italic());
        }
        if rel.at_reference() && link.line > 0 {
            second.push(format!(" · reference at line {}", link.line).dim());
        }
    }
    if !sym.removed {
        let (c, u) = impact(g, t.sym);
        second.extend(impact_spans(c, u));
    }
    let hunks = g.hunks_of_symbol(t.sym).len();
    if hunks > 0 {
        second.push(format!(" · {hunks} hunk(s), gd").dim());
    }
    if !second.is_empty() {
        lines.push(clip(second, width));
    }
    let annotations: Vec<String> = sym
        .annotations
        .iter()
        .map(|a| format!("@{}", a.name))
        .collect();
    if !annotations.is_empty() {
        lines.push(clip(
            vec![format!(" {}", annotations.join(" ")).fg(Color::Blue).dim()],
            width,
        ));
    }
    lines
}

/// Module row selected: its dependencies in and out.
fn render_module_details(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let v = &app.graph;
    let Some(map) = &v.modules else {
        return;
    };
    let name = match v.mode {
        Mode::Modules => match v.mod_rows.get(v.mod_pos.cursor) {
            Some(ModRow::Module(i)) => Some(map.modules[*i].name.clone()),
            _ => None,
        },
        Mode::Changed | Mode::Found => {
            let rows = if v.mode == Mode::Changed {
                &v.list
            } else {
                &v.found
            };
            match rows.get(v.pos().cursor) {
                Some(ListRow::Module(m)) => Some(m.clone()),
                _ => None,
            }
        }
        Mode::Symbol | Mode::Flows => None,
    };
    let Some(i) = name.and_then(|n| map.modules.iter().position(|m| m.name == n)) else {
        f.render_widget(pane(focused), area);
        return;
    };
    let m = &map.modules[i];
    let block = pane(focused).title(format!(" module {} ", m.name));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = vec![Line::from(vec![
        format!(" {} file(s), {} changed", m.files, m.changed_files).into(),
        format!(" · {} changed symbol(s)", m.changed_symbols).fg(Color::Yellow),
        if m.test { " · tests".dim() } else { "".into() },
    ])];
    let mut section = |title: &str, deps: Vec<(usize, &survol_core::graph::ModuleEdge)>| {
        lines.push(Line::default());
        lines.push(Line::from(
            format!(" {title} ({})", deps.len()).fg(Color::Blue).bold(),
        ));
        for (other, e) in deps {
            let o = &map.modules[other];
            let kinds: Vec<String> = e
                .kinds
                .iter()
                .map(|(k, n)| format!("{} {n}", kind_name(k)))
                .collect();
            lines.push(Line::from(vec![
                format!("   {:>4}  ", e.count).fg(ACCENT),
                Span::styled(
                    o.name.clone(),
                    if o.changed() {
                        Style::new().fg(Color::Yellow)
                    } else {
                        Style::new()
                    },
                ),
                format!("  {}", kinds.join(", ")).dim(),
            ]));
        }
    };
    section(
        "Depends on",
        map.edges
            .iter()
            .filter(|e| e.from == i)
            .map(|e| (e.to, e))
            .collect(),
    );
    section(
        "Used by",
        map.edges
            .iter()
            .filter(|e| e.to == i)
            .map(|e| (e.from, e))
            .collect(),
    );
    lines.push(Line::default());
    lines.push(Line::from(
        " Enter / l: its changed symbols · x: export the map as Mermaid".dim(),
    ));
    f.render_widget(Paragraph::new(lines), inner);
}
