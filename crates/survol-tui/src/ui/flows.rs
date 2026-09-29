//! Rendering of the Flows mode of the Graph view: the impacted entry points
//! (list pane) and the selected flow as an indented tree (content pane,
//! above the code preview).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use survol_core::flows::{Change, FlowStatus, Layer, Repeat, Terminal, Via};

use super::graph::{cursor_line, two_sided};
use super::{pane, wrap};
use crate::app::App;
use crate::theme::theme;
use crate::views::Focus;
use crate::views::flows::{BaseStatus, EntryRow, FlowsState, Side};

/// Title of the entry list.
pub fn list_title(v: &FlowsState) -> Line<'static> {
    let n = v.flows.iter().filter(|f| f.after.is_some()).count();
    let mut spans = vec![format!(" impacted entry points ({n}) ").bold()];
    match &v.base {
        BaseStatus::Running(since) => {
            spans.push(format!("⟳ before/after {}s ", since.elapsed().as_secs()).fg(theme().meta))
        }
        BaseStatus::Done => {
            let d = v
                .flows
                .iter()
                .filter(|f| f.diff.as_ref().is_some_and(|d| d.is_relevant()))
                .count();
            spans.push(Span::styled(
                format!("{d} changed before/after "),
                Style::new().fg(theme().status_modified),
            ));
        }
        BaseStatus::Failed(_) => spans.push("before/after failed ".fg(theme().error)),
        BaseStatus::NotStarted => {}
    }
    Line::from(spans)
}

/// A line of the entry list.
pub fn entry_row(v: &FlowsState, row: EntryRow, width: usize) -> Vec<Span<'static>> {
    match row {
        EntryRow::Kind(k) => {
            let n = v.flows.iter().filter(|f| f.entry.kind == k).count();
            vec![Span::styled(
                format!("▣ {} ({n})", k.title()),
                Style::new().fg(theme().layer).bold(),
            )]
        }
        EntryRow::Flow(i) => {
            let t = theme();
            let f = &v.flows[i];
            let mut left = vec![
                "  ".into(),
                Span::styled(f.entry.label.clone(), Style::new().fg(t.accent)),
            ];
            if f.entry.label != f.entry.name {
                left.push(format!("  {}", f.entry.name).fg(theme().meta));
            }
            let mut right: Vec<Span<'static>> = Vec::new();
            match f.diff.as_ref().map(|d| d.status) {
                Some(FlowStatus::New) => right.push(" new".fg(t.status_added).bold()),
                Some(FlowStatus::Removed) => right.push(" gone".fg(t.status_deleted).bold()),
                Some(FlowStatus::Changed) => right.push(" Δ".fg(t.status_modified).bold()),
                _ => {}
            }
            if let Some(a) = &f.after {
                right.push(format!(" {} mod", a.changed_steps).fg(t.status_modified));
                if a.confidence < 0.995 {
                    right.push(format!(" {:.2}", a.confidence).fg(t.confidence));
                }
            }
            right.push(" ".into());
            two_sided(left, right, width)
        }
    }
}

fn layer_style(l: Layer) -> Style {
    let t = theme();
    let c = match l {
        Layer::View => t.layer_view,
        Layer::Controller => t.layer_controller,
        Layer::Service => t.layer_service,
        Layer::Repository | Layer::Entity => t.layer_repository,
        Layer::External => t.layer_external,
        Layer::Config => t.layer_config,
        Layer::Code => t.layer_code,
    };
    Style::new().fg(c)
}

fn via_glyph(v: Via) -> &'static str {
    match v {
        Via::Entry => "● ",
        Via::Calls => "→ ",
        Via::Dispatch => "⇢ ",
        Via::HttpCalls => "⇒ ",
        Via::Publishes => "↯ ",
        Via::Routes => "▹ ",
        Via::Member => "· ",
        Via::Persists => "▤ ",
    }
}

/// The selected flow as a tree, cursor on the selected step when focused.
pub fn render_flow(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.graph.focus == Focus::Content;
    let v = &app.graph.flows;
    let Some(flow) = v.selected() else {
        let msg = if !v.computed {
            "  computing flows…"
        } else if v.flows.is_empty() {
            "  No entry point reaches a changed symbol (HTTP endpoint, route, listener, job…)."
        } else {
            "  Select an entry point."
        };
        f.render_widget(
            Paragraph::new(vec![Line::default(), Line::from(msg.fg(theme().meta))])
                .block(pane(focused).title(" flow ")),
            area,
        );
        return;
    };
    let side = v.shown_side();
    let mut title = vec![
        format!(" {} ", flow.entry.label).bold(),
        Span::styled(
            format!("[{}] ", side.label()),
            Style::new().fg(match side {
                Side::After => theme().status_added,
                Side::Before => theme().status_deleted,
                Side::Diff => theme().status_modified,
            }),
        ),
    ];
    let relevant = flow.diff.as_ref().is_some_and(|d| d.is_relevant());
    if relevant {
        title.push(" b before/after ".fg(theme().meta));
    }
    let block = pane(focused)
        .title(Line::from(title))
        .title_bottom(Line::from(" x Mermaid · Enter symbol · e edit · gd diff ").fg(theme().meta));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width as usize;

    let mut head: Vec<Line<'static>> = Vec::new();
    if let Some(d) = &flow.diff {
        let color = if relevant {
            theme().status_modified
        } else {
            theme().meta
        };
        let mut text = d.summary();
        if !d.new_external.is_empty() {
            let names: Vec<&str> = d.new_external.iter().map(|r| r.name.as_str()).collect();
            text.push_str(&format!(" · new external: {}", names.join(", ")));
        }
        for l in wrap(&text, width.saturating_sub(2)).into_iter().take(2) {
            head.push(Line::from(Span::styled(
                format!(" {l}"),
                Style::new().fg(color),
            )));
        }
    } else if matches!(v.base, BaseStatus::Running(_)) {
        head.push(Line::from(
            " building the base revision for before / after…".fg(theme().meta),
        ));
    }

    let h = (inner.height as usize).saturating_sub(head.len()).max(1);
    let len = app.graph.flows.lines().len();
    let st = &mut app.graph.flows.step_pos;
    st.height = h;
    st.clamp(len);
    let pos = *st;
    let lines = app.graph.flows.lines();
    let mut out = head;
    let end = (pos.scroll + h).min(lines.len());
    let t = theme();
    for (i, l) in lines.iter().enumerate().take(end).skip(pos.scroll) {
        let s = l.step;
        let (sign, sign_style) = match l.change {
            Some(Change::Added) => ("+", Style::new().fg(t.status_added).bold()),
            Some(Change::Removed) => ("-", Style::new().fg(t.status_deleted).bold()),
            _ => (" ", Style::new()),
        };
        let name_style = if l.change == Some(Change::Removed) {
            Style::new().fg(t.status_deleted).crossed_out()
        } else if l.change == Some(Change::Added) {
            Style::new().fg(t.status_added)
        } else if s.changed {
            Style::new().fg(t.status_modified).bold()
        } else if s.reaches_changed {
            Style::new().fg(t.accent)
        } else {
            Style::new().fg(t.meta)
        };
        let mut left = vec![
            Span::styled(sign, sign_style),
            format!("{}{}", "  ".repeat(s.depth), via_glyph(s.via)).fg(t.accent),
            Span::styled(s.name.clone(), name_style),
            Span::styled(format!(" {}", s.layer.label()), layer_style(s.layer)),
        ];
        match s.terminal {
            Some(Terminal::Persistence) => left.push(" [db]".fg(t.db).bold()),
            Some(Terminal::External) => left.push(" [external]".fg(t.external).bold()),
            Some(Terminal::Event) => left.push(" [event]".fg(t.event).bold()),
            _ => {}
        }
        if let Some(d) = &s.detail {
            left.push(format!("  {d}").fg(t.meta));
        }
        match s.repeat {
            Some(Repeat::Cycle) => left.push(" ↻ cycle".fg(theme().meta)),
            Some(Repeat::Seen) => left.push(" ↑ above".fg(theme().meta)),
            None => {}
        }
        if l.rerouted {
            left.push(" rerouted".fg(t.status_modified).italic());
        }
        if s.hidden > 0 {
            left.push(format!(" +{}", s.hidden).fg(theme().meta));
        }
        if s.truncated {
            left.push(" …".fg(theme().meta));
        }
        let mut right = Vec::new();
        if s.confidence < 0.995 {
            right.push(format!("{:.2} ", s.confidence).fg(t.confidence));
        }
        right.push(if s.changed {
            "modified ".fg(t.status_modified)
        } else {
            "         ".into()
        });
        let line = Line::from(two_sided(left, right, width));
        out.push(cursor_line(line, i == pos.cursor && focused, width));
        if i == pos.cursor && !focused {
            let l = out.pop().expect("just pushed");
            out.push(l.style(Style::new().bg(theme().inactive_cursor_bg)));
        }
    }
    f.render_widget(Paragraph::new(out), inner);
}
