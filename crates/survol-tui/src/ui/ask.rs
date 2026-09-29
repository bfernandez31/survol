//! Rendering of the questions to the LLM: input, waiting spinner, answer
//! with its links, history.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use survol_core::ask::Answer;

use super::{ACCENT, markdown, wrap};
use crate::highlight::Highlighter;
use crate::theme::theme;
use crate::views::ask::{AnswerView, AskInput, HistoryView, Pending};

/// A centered rectangle of at most `w` × `h`.
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

pub fn popup_block(title: String) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(title).bold())
}

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

pub fn spinner(elapsed_ms: u128) -> &'static str {
    SPINNER[(elapsed_ms / 120) as usize % SPINNER.len()]
}

pub fn render_input(f: &mut Frame, area: Rect, input: &AskInput, model: &str) {
    let sugg = input.suggestions();
    let rect = centered(area, 90, sugg.len() as u16 + 9);
    let width = rect.width.saturating_sub(4) as usize;
    let mut lines = vec![
        Line::from(vec![" about ".dim(), input.label.clone().bold()]),
        Line::default(),
    ];
    let text = format!("{}▏", input.text);
    for (i, l) in wrap(&text, width.saturating_sub(3)).into_iter().enumerate() {
        let lead = if i == 0 { " > " } else { "   " };
        lines.push(Line::from(vec![lead.fg(ACCENT).bold(), l.into()]));
    }
    lines.push(Line::default());
    lines.push(Line::from(
        " Suggestions (↑ ↓ to pick, or its number):".dim(),
    ));
    for (i, s) in sugg.iter().enumerate() {
        let style = if input.suggestion == Some(i) {
            Style::new().bg(theme().cursor_bg).bold()
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            format!("  {}. ", i + 1).fg(ACCENT),
            Span::styled(s.to_string(), style),
        ]));
    }
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            popup_block(" ask the LLM ".into()).title_bottom(
                Line::from(format!(" Enter ask · Esc cancel · model: {model} ")).dim(),
            ),
        ),
        rect,
    );
}

pub fn render_pending(f: &mut Frame, area: Rect, p: &Pending) {
    let rect = centered(area, 80, 8);
    let width = rect.width.saturating_sub(4) as usize;
    let elapsed = p.since.elapsed();
    let mut lines = vec![
        Line::from(vec![
            format!(" {} ", spinner(elapsed.as_millis()))
                .fg(ACCENT)
                .bold(),
            format!("asking about {}… {}s", p.label, elapsed.as_secs()).bold(),
        ]),
        Line::default(),
    ];
    for l in wrap(&p.question, width.saturating_sub(4)) {
        lines.push(Line::from(format!("   {l}").italic()));
    }
    lines.push(Line::default());
    lines.push(Line::from(
        " Esc: keep reviewing, the answer comes back with A".dim(),
    ));
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(popup_block(" ask the LLM ".into())),
        rect,
    );
}

/// The answer rendered from markdown, its references styled; also the line
/// of the selected reference.
pub fn answer_lines(
    a: &Answer,
    selected: Option<usize>,
    width: usize,
    hl: Option<&Highlighter>,
) -> (Vec<Line<'static>>, Option<usize>) {
    let marks: Vec<(std::ops::Range<usize>, Style)> = a
        .refs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let style = if !r.valid {
                Style::new()
                    .fg(Color::Red)
                    .add_modifier(Modifier::CROSSED_OUT)
            } else if selected == Some(i) {
                Style::new().fg(Color::Black).bg(ACCENT).bold()
            } else {
                Style::new().fg(ACCENT).add_modifier(Modifier::UNDERLINED)
            };
            (r.start..r.end, style)
        })
        .collect();
    let (lines, at) = markdown::render_marked(&a.text, width.max(20), Style::new(), hl, &marks);
    let link_line = selected.and_then(|i| at.get(i).copied().flatten());
    (lines, link_line)
}

pub fn render_answer(f: &mut Frame, area: Rect, v: &mut AnswerView, hl: &Highlighter) {
    let rect = centered(
        area,
        (area.width * 9 / 10).clamp(40, 120),
        (area.height * 9 / 10).max(10),
    );
    let block = popup_block(format!(" {} ", v.answer.label)).title_bottom(
        Line::from(" Tab/n link · Enter go (graph) · d diff · e editor · j/k scroll · A history · Esc close ")
            .dim(),
    );
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    let width = inner.width.saturating_sub(2) as usize;

    let mut head: Vec<Line> = wrap(&format!("Q: {}", v.answer.question), width)
        .into_iter()
        .map(|l| Line::from(format!(" {l}").bold()))
        .collect();
    let a = &v.answer;
    let valid = a.refs.len() - a.unknown_refs();
    let mut info = vec![Span::from(format!(" {} link(s)", valid)).fg(ACCENT)];
    if a.unknown_refs() > 0 {
        info.push(
            format!(" · {} unknown reference(s), struck out", a.unknown_refs()).fg(Color::Red),
        );
    }
    if let Some(m) = &a.model {
        info.push(format!(" · {m}").dim());
    }
    if a.from_cache {
        info.push(" · cached".dim());
    }
    head.push(Line::from(info));
    head.push(Line::from("─".repeat(inner.width as usize).dim()));
    let head_h = (head.len() as u16).min(inner.height.saturating_sub(2));
    let top = Rect::new(inner.x, inner.y, inner.width, head_h);
    let body = Rect::new(
        inner.x,
        inner.y + head_h,
        inner.width,
        inner.height - head_h,
    );
    f.render_widget(Paragraph::new(head), top);

    let (lines, link_line) = answer_lines(&v.answer, v.link, width, Some(hl));
    v.height = body.height as usize;
    v.total_lines = lines.len();
    if v.link_line != link_line {
        // A new link was selected: bring it into view.
        if let Some(l) = link_line
            && (l < v.scroll || l >= v.scroll + v.height)
        {
            v.scroll = l.saturating_sub(v.height / 3);
        }
        v.link_line = link_line;
    }
    v.scroll = v.scroll.min(lines.len().saturating_sub(v.height));
    let shown: Vec<Line> = lines
        .into_iter()
        .skip(v.scroll)
        .take(v.height)
        .map(|mut l| {
            l.spans.insert(0, " ".into());
            l
        })
        .collect();
    f.render_widget(Paragraph::new(shown), body);
}

/// `3m ago`, `2h ago`, `4d ago`.
fn ago(secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let d = now.saturating_sub(secs);
    match d {
        0..60 => "now".into(),
        60..3600 => format!("{}m ago", d / 60),
        3600..86400 => format!("{}h ago", d / 3600),
        _ => format!("{}d ago", d / 86400),
    }
}

pub fn render_history(f: &mut Frame, area: Rect, h: &HistoryView, history: &[Answer]) {
    let rows = history.len().max(1) as u16;
    let rect = centered(area, 110, rows + 2);
    let width = rect.width.saturating_sub(2) as usize;
    let mut lines = Vec::new();
    if history.is_empty() {
        lines.push(Line::from(" no question yet".dim()));
    }
    for (i, a) in history.iter().rev().enumerate() {
        let mut spans = vec![
            format!(" {:>7}  ", ago(a.asked_at)).dim(),
            format!("{}  ", a.label).fg(ACCENT),
            a.question.clone().into(),
        ];
        let used: usize = spans.iter().map(Span::width).sum();
        if used < width {
            spans.push(" ".repeat(width - used).into());
        }
        let mut line = Line::from(spans);
        if i == h.sel {
            line = line.patch_style(Style::new().bg(theme().cursor_bg));
        }
        lines.push(line);
    }
    let scroll = h.sel.saturating_sub(rect.height.saturating_sub(4) as usize) as u16;
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)).block(
            popup_block(format!(" questions of this review ({}) ", history.len()))
                .title_bottom(Line::from(" Enter open · j/k move · Esc close ").dim()),
        ),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use survol_core::ask::{CodeRef, Subject};
    use survol_core::model::Side;

    #[test]
    fn wraps_answers_and_styles_references() {
        let text = "Saves the pet [src/Owner.java:12] then calls [Nope.java:3].\n\n- a **bullet** that wraps over the line width";
        let r = |s: &str, valid| {
            let start = text.find(s).unwrap();
            CodeRef {
                start,
                end: start + s.len(),
                raw_path: String::new(),
                path: String::new(),
                line: 1,
                end_line: None,
                side: Side::New,
                valid,
            }
        };
        let a = Answer {
            subject: Subject::Group(0),
            label: String::new(),
            question: String::new(),
            text: text.into(),
            refs: vec![r("src/Owner.java:12", true), r("Nope.java:3", false)],
            model: None,
            head_sha: String::new(),
            prompt_version: 1,
            key: String::new(),
            asked_at: 0,
            from_cache: false,
        };
        let (lines, link) = answer_lines(&a, Some(0), 24, None);
        let plain: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(
            plain,
            [
                "Saves the pet",
                "[src/Owner.java:12] then",
                "calls [Nope.java:3].",
                "",
                "• a bullet that wraps",
                "  over the line width",
            ]
        );
        assert_eq!(link, Some(1));
        let span = lines[1]
            .spans
            .iter()
            .find(|s| s.content == "src/Owner.java:12")
            .unwrap();
        assert_eq!(span.style.bg, Some(ACCENT));
        let bad = lines[2]
            .spans
            .iter()
            .find(|s| s.content == "Nope.java:3")
            .unwrap();
        assert!(bad.style.add_modifier.contains(Modifier::CROSSED_OUT));
    }
}
