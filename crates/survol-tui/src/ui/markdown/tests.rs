use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use super::{render, render_marked};
use crate::highlight::Highlighter;

fn plain(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn span_style(lines: &[Line], text: &str) -> Style {
    lines
        .iter()
        .flat_map(|l| &l.spans)
        .find(|s| s.content == text)
        .unwrap_or_else(|| panic!("no span `{text}` in {:?}", plain(lines)))
        .style
}

#[test]
fn paragraphs_wrap_with_inline_styles() {
    let md = "**Conformance** — the check reads `findById` *unlocked*.\n\nSecond paragraph.";
    let lines = render(md, 24, Style::new(), None);
    assert_eq!(
        plain(&lines),
        [
            "Conformance — the check",
            "reads findById unlocked.",
            "",
            "Second paragraph.",
        ]
    );
    assert!(
        span_style(&lines, "Conformance")
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert!(
        span_style(&lines, "unlocked")
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    assert_eq!(
        span_style(&lines, "findById").fg,
        Some(crate::theme::theme().code)
    );
    // No line is wider than asked.
    assert!(plain(&lines).iter().all(|l| l.chars().count() <= 24));
}

#[test]
fn base_style_is_kept_under_inline_styles() {
    let base = Style::new().fg(Color::Yellow);
    let lines = render("a **b**", 20, base, None);
    let b = span_style(&lines, "b");
    assert_eq!(b.fg, Some(Color::Yellow));
    assert!(b.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn lists_hang_and_nest() {
    let md = "Steps:\n\n- read the row then lock it\n- check\n  1. token\n  2. filter\n\nDone.";
    let lines = render(md, 18, Style::new(), None);
    assert_eq!(
        plain(&lines),
        [
            "Steps:",
            "",
            "• read the row",
            "  then lock it",
            "• check",
            "  1. token",
            "  2. filter",
            "",
            "Done.",
        ]
    );
}

#[test]
fn headings_quotes_and_rules() {
    let md = "# Title\n\n> The order read → lock\n> is not commutable.\n\n---\nafter";
    let lines = render(md, 40, Style::new(), None);
    assert_eq!(
        plain(&lines),
        [
            "Title",
            "",
            "▌ The order read → lock",
            "▌ is not commutable.",
            "",
            &"─".repeat(40),
            "",
            "after",
        ]
    );
    let title = span_style(&lines, "Title");
    assert!(title.add_modifier.contains(Modifier::BOLD));
    assert!(
        span_style(&lines, "is")
            .add_modifier
            .contains(Modifier::ITALIC)
    );
}

#[test]
fn code_blocks_are_highlighted_and_cut_not_wrapped() {
    let hl = Highlighter::new();
    let md = "Use:\n\n```java\nint x = compute(aVeryLongArgumentName);\n```\n";
    let lines = render(md, 24, Style::new(), Some(&hl));
    assert_eq!(
        plain(&lines),
        [
            "Use:",
            "",
            "│ int x = compute(aVeryL",
            "│ ongArgumentName);",
        ]
    );
    // The keyword got a colour of its own.
    let int = lines[2]
        .spans
        .iter()
        .find(|s| s.content.contains("int"))
        .unwrap();
    assert!(int.style.fg.is_some(), "{:?}", lines[2]);
    // GitLab suggestions keep their text.
    let lines = render(
        "```suggestion:-0+0\nfoo();\n```",
        30,
        Style::new(),
        Some(&hl),
    );
    assert_eq!(plain(&lines), ["│ foo();"]);
}

#[test]
fn links_show_their_destination() {
    let lines = render(
        "see [the doc](https://x.io/a) or <https://y.io>",
        60,
        Style::new(),
        None,
    );
    assert_eq!(
        plain(&lines),
        ["see the doc‹https://x.io/a› or https://y.io"]
    );
    assert!(
        span_style(&lines, "the")
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
}

#[test]
fn tables_are_one_line_per_row() {
    let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    assert_eq!(
        plain(&render(md, 30, Style::new(), None)),
        ["a │ b", "1 │ 2"]
    );
}

#[test]
fn marks_are_styled_and_located() {
    let text = "Saves the pet [src/Owner.java:12] then calls `[Nope.java:3]`.";
    let r = |s: &str| {
        let start = text.find(s).unwrap();
        start..start + s.len()
    };
    let link = Style::new().bg(Color::Cyan);
    let bad = Style::new().add_modifier(Modifier::CROSSED_OUT);
    let (lines, at) = render_marked(
        text,
        24,
        Style::new(),
        None,
        &[(r("src/Owner.java:12"), link), (r("Nope.java:3"), bad)],
    );
    assert_eq!(
        plain(&lines),
        [
            "Saves the pet",
            "[src/Owner.java:12] then",
            "calls [Nope.java:3]."
        ]
    );
    assert_eq!(at, [Some(1), Some(2)]);
    assert_eq!(
        span_style(&lines, "src/Owner.java:12").bg,
        Some(Color::Cyan)
    );
    assert!(
        span_style(&lines, "Nope.java:3")
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
}
