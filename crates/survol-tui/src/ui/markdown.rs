//! Markdown to styled, word-wrapped terminal lines: review notes, the thread
//! popup and the answers of the LLM share it.
//!
//! Paragraphs, emphasis, inline code, links, lists, headings, block quotes,
//! rules, tables (one line per row) and fenced code blocks (highlighted with
//! syntect, cut into lines of the width rather than wrapped at words). A
//! single newline breaks the line, as in GitLab comments. Byte
//! ranges of the source can be marked with a style (the `[path:line]` links
//! of an answer): [`render_marked`] tells on which line each mark landed.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::highlight::{Highlighter, expand_tabs};
use crate::theme::theme;

/// Styled lines of the markdown `text`, wrapped to `width` columns, on top of
/// `base`. Code blocks are highlighted when `hl` is given.
pub fn render(
    text: &str,
    width: usize,
    base: Style,
    hl: Option<&Highlighter>,
) -> Vec<Line<'static>> {
    render_marked(text, width, base, hl, &[]).0
}

/// [`render`], with the byte ranges `marks` of `text` drawn in their style;
/// also the first line of each mark (`None` when not shown, in a code block).
pub fn render_marked(
    text: &str,
    width: usize,
    base: Style,
    hl: Option<&Highlighter>,
    marks: &[(Range<usize>, Style)],
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    let mut w = Writer::new(width.max(10), base, hl, marks.len());
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for (ev, range) in Parser::new_ext(text, opts).into_offset_iter() {
        match ev {
            Event::Start(tag) => w.start(tag),
            Event::End(tag) => w.end(tag),
            Event::Text(t) => {
                if w.code.is_some() {
                    w.code_text(&t);
                } else {
                    w.text(&t, locate(text, range, &t), marks, None);
                }
            }
            Event::Code(t) => {
                let code = Style::new().fg(theme().code);
                w.text(&t, locate(text, range, &t), marks, Some(code));
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                w.text(&t, None, marks, Some(Style::new().fg(theme().code)));
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                w.text(&t, None, marks, Some(Style::new().fg(theme().meta)));
            }
            Event::FootnoteReference(t) => w.text(&format!("[^{t}]"), None, marks, None),
            // Comments are written line by line: keep their lines, as GitLab
            // and GitHub comments do.
            Event::SoftBreak => w.newline(),
            Event::HardBreak => w.newline(),
            Event::Rule => {
                w.block_start();
                w.flush_word();
                let rule = "─".repeat(w.width.min(40));
                w.lines.push(Line::from(Span::styled(
                    rule,
                    Style::new().fg(theme().meta),
                )));
                w.need_blank = true;
            }
            Event::TaskListMarker(done) => {
                w.text(if done { "[x]" } else { "[ ]" }, None, marks, None);
                w.space();
            }
        }
    }
    w.finish();
    (w.lines, w.mark_lines)
}

/// Byte offset in `src` of the text `t` of an event spanning `range`: the
/// text may be shorter than its source (backticks, escapes).
fn locate(src: &str, range: Range<usize>, t: &str) -> Option<usize> {
    let s = src.get(range.clone())?;
    s.find(t).map(|off| range.start + off)
}

/// What a container (list item, block quote) puts before its lines.
struct Prefix {
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    used: bool,
}

struct Writer<'h> {
    width: usize,
    base: Style,
    hl: Option<&'h Highlighter>,
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    cur_w: usize,
    /// The prefixes of the current line are written.
    started: bool,
    /// Text after the prefixes.
    content: bool,
    /// Pieces of the word being read (no whitespace between them).
    word: Vec<(String, Style, Option<usize>)>,
    pending_space: bool,
    prefixes: Vec<Prefix>,
    /// Inline styles, innermost last.
    styles: Vec<Style>,
    /// Next number of each open list (`None`: bullets).
    lists: Vec<Option<u64>>,
    /// Destination of each open link.
    links: Vec<String>,
    /// Language and text of the code block being read.
    code: Option<(String, String)>,
    need_blank: bool,
    /// First block of a list item: no blank line before it.
    item_fresh: bool,
    /// Cells written in the current table row.
    cells: usize,
    mark_lines: Vec<Option<usize>>,
}

impl<'h> Writer<'h> {
    fn new(width: usize, base: Style, hl: Option<&'h Highlighter>, marks: usize) -> Self {
        Self {
            width,
            base,
            hl,
            lines: Vec::new(),
            cur: Vec::new(),
            cur_w: 0,
            started: false,
            content: false,
            word: Vec::new(),
            pending_space: false,
            prefixes: Vec::new(),
            styles: Vec::new(),
            lists: Vec::new(),
            links: Vec::new(),
            code: None,
            need_blank: false,
            item_fresh: false,
            cells: 0,
            mark_lines: vec![None; marks],
        }
    }

    fn style(&self) -> Style {
        self.styles.iter().fold(self.base, |s, x| s.patch(*x))
    }

    // ----- lines ----------------------------------------------------------

    /// Writes the prefixes of the containers at the start of a line.
    fn start_line(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        for p in &mut self.prefixes {
            let spans = if p.used { &p.rest } else { &p.first };
            p.used = true;
            self.cur_w += spans.iter().map(Span::width).sum::<usize>();
            self.cur.extend(spans.iter().cloned());
        }
    }

    fn break_line(&mut self) {
        self.lines.push(Line::from(std::mem::take(&mut self.cur)));
        self.cur_w = 0;
        self.started = false;
        self.content = false;
        self.pending_space = false;
    }

    /// Ends the current line, if it has text.
    fn newline(&mut self) {
        self.flush_word();
        if self.content {
            self.break_line();
        }
    }

    fn space(&mut self) {
        self.flush_word();
        if self.content {
            self.pending_space = true;
        }
    }

    /// An empty line between blocks, keeping the bars of block quotes.
    fn blank(&mut self) {
        let spans: Vec<Span<'static>> = self
            .prefixes
            .iter()
            .filter(|p| p.first == p.rest)
            .flat_map(|p| p.rest.iter().cloned())
            .collect();
        self.lines.push(Line::from(spans));
    }

    /// Before a block: end the line, and a blank line after a previous block.
    fn block_start(&mut self) {
        self.newline();
        if self.need_blank && !self.lines.is_empty() && !self.item_fresh {
            self.blank();
        }
        self.need_blank = false;
        self.item_fresh = false;
    }

    fn block_end(&mut self) {
        self.newline();
        self.need_blank = true;
    }

    // ----- words ----------------------------------------------------------

    /// Adds `t` (starting at byte `at` of the source, when known) with the
    /// current style, or `over` it.
    fn text(
        &mut self,
        t: &str,
        at: Option<usize>,
        marks: &[(Range<usize>, Style)],
        over: Option<Style>,
    ) {
        let mut style = self.style();
        if let Some(o) = over {
            style = style.patch(o);
        }
        for (i, ch) in t.char_indices() {
            if ch.is_whitespace() {
                self.space();
                continue;
            }
            let mark = at.and_then(|a| marks.iter().position(|(r, _)| r.contains(&(a + i))));
            let s = mark.map_or(style, |m| style.patch(marks[m].1));
            match self.word.last_mut() {
                Some((text, ls, lm)) if *ls == s && *lm == mark => text.push(ch),
                _ => self.word.push((ch.to_string(), s, mark)),
            }
        }
    }

    /// Puts the word read so far on the line, on the next one if it does not
    /// fit, cut if longer than a line.
    fn flush_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        let word = std::mem::take(&mut self.word);
        let word_w: usize = word
            .iter()
            .flat_map(|(t, _, _)| t.chars())
            .map(|c| c.width().unwrap_or(0))
            .sum();
        let space = usize::from(self.pending_space && self.content);
        if self.content && self.cur_w + space + word_w > self.width {
            self.break_line();
        }
        self.start_line();
        if self.pending_space && self.content {
            self.cur.push(Span::styled(" ", self.base));
            self.cur_w += 1;
        }
        self.pending_space = false;
        for (text, style, mark) in word {
            let mut chunk = String::new();
            for ch in text.chars() {
                let cw = ch.width().unwrap_or(0);
                if self.content && self.cur_w + cw > self.width {
                    if !chunk.is_empty() {
                        self.put(std::mem::take(&mut chunk), style, mark);
                    }
                    self.break_line();
                    self.start_line();
                }
                chunk.push(ch);
                self.cur_w += cw;
                self.content = true;
            }
            if !chunk.is_empty() {
                self.put(chunk, style, mark);
            }
        }
    }

    fn put(&mut self, text: String, style: Style, mark: Option<usize>) {
        if let Some(m) = mark {
            let line = self.lines.len();
            let first = &mut self.mark_lines[m];
            *first = Some(first.map_or(line, |l| l.min(line)));
        }
        self.cur.push(Span::styled(text, style));
        self.content = true;
    }

    // ----- blocks ---------------------------------------------------------

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.block_start(),
            Tag::Heading { level, .. } => {
                self.block_start();
                let mut s = Style::new().fg(theme().accent).add_modifier(Modifier::BOLD);
                if level == HeadingLevel::H1 {
                    s = s.add_modifier(Modifier::UNDERLINED);
                }
                self.styles.push(s);
            }
            Tag::BlockQuote(_) => {
                self.block_start();
                let bar = vec![Span::styled("▌ ", Style::new().fg(theme().gutter))];
                self.prefixes.push(Prefix {
                    first: bar.clone(),
                    rest: bar,
                    used: false,
                });
                self.styles
                    .push(Style::new().add_modifier(Modifier::ITALIC));
            }
            Tag::CodeBlock(kind) => {
                self.block_start();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split(|c: char| c.is_whitespace() || c == ',' || c == ':')
                        .next()
                        .unwrap_or("")
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(start) => {
                if self.lists.is_empty() {
                    self.block_start();
                } else {
                    self.newline();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.newline();
                let bullet = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => "• ".to_string(),
                };
                let w = bullet.chars().count();
                self.prefixes.push(Prefix {
                    first: vec![Span::styled(bullet, Style::new().fg(theme().bullet))],
                    rest: vec![Span::raw(" ".repeat(w))],
                    used: false,
                });
                self.item_fresh = true;
                self.need_blank = false;
            }
            Tag::Emphasis => self
                .styles
                .push(Style::new().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.styles.push(Style::new().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self
                .styles
                .push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.links.push(dest_url.to_string());
                self.styles.push(
                    Style::new()
                        .fg(theme().link)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Image { dest_url, .. } => {
                self.links.push(dest_url.to_string());
                self.styles.push(Style::new().fg(theme().meta));
                self.text("[image:", None, &[], None);
                self.space();
            }
            Tag::Table(_) => self.block_start(),
            Tag::TableHead => {
                self.cells = 0;
                self.styles.push(Style::new().add_modifier(Modifier::BOLD));
            }
            Tag::TableRow => self.cells = 0,
            Tag::TableCell => {
                if self.cells > 0 {
                    self.space();
                    self.text("│", None, &[], Some(Style::new().fg(theme().meta)));
                    self.space();
                }
                self.cells += 1;
            }
            Tag::HtmlBlock => self.block_start(),
            Tag::FootnoteDefinition(name) => {
                self.block_start();
                self.text(&format!("[^{name}]:"), None, &[], None);
                self.space();
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock | TagEnd::FootnoteDefinition => self.block_end(),
            TagEnd::Heading(_) => {
                self.styles.pop();
                self.block_end();
            }
            TagEnd::BlockQuote(_) => {
                self.newline();
                self.prefixes.pop();
                self.styles.pop();
                self.need_blank = true;
            }
            TagEnd::CodeBlock => {
                if let Some((lang, text)) = self.code.take() {
                    self.code_block(&lang, &text);
                }
                self.need_blank = true;
            }
            TagEnd::List(_) => {
                self.newline();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.need_blank = true;
                }
            }
            TagEnd::Item => {
                self.newline();
                self.prefixes.pop();
                self.item_fresh = false;
                self.need_blank = false;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                let url = self.links.pop().unwrap_or_default();
                // Show where it goes unless the text already says it.
                let shown: String = self.cur.iter().map(|s| s.content.as_ref()).collect();
                let shown = shown
                    + &self
                        .word
                        .iter()
                        .map(|(t, _, _)| t.as_str())
                        .collect::<String>();
                if !url.is_empty() && !shown.ends_with(url.as_str()) {
                    self.text(
                        &format!("‹{url}›"),
                        None,
                        &[],
                        Some(Style::new().fg(theme().meta)),
                    );
                }
            }
            TagEnd::Image => {
                self.links.pop();
                self.text("]", None, &[], None);
                self.styles.pop();
            }
            TagEnd::Table => self.block_end(),
            TagEnd::TableHead => {
                self.styles.pop();
                self.newline();
            }
            TagEnd::TableRow => self.newline(),
            _ => {}
        }
    }

    fn code_text(&mut self, t: &str) {
        if let Some((_, buf)) = &mut self.code {
            buf.push_str(t);
        }
    }

    /// The lines of a code block, highlighted, cut to the width.
    fn code_block(&mut self, lang: &str, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let spans = match self.hl {
            Some(hl) => hl.code(lang, text.split('\n')),
            None => text
                .split('\n')
                .map(|l| vec![(Style::default(), expand_tabs(l))])
                .collect(),
        };
        let gutter = Style::new().fg(theme().gutter);
        for line in spans {
            self.start_line();
            self.cur.push(Span::styled("│ ", gutter));
            self.cur_w += 2;
            let start_w = self.cur_w;
            for (style, t) in line {
                let mut chunk = String::new();
                for ch in t.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if self.cur_w + cw > self.width && self.cur_w > start_w {
                        self.cur
                            .push(Span::styled(std::mem::take(&mut chunk), style));
                        self.content = true;
                        self.break_line();
                        self.start_line();
                        self.cur.push(Span::styled("│ ", gutter));
                        self.cur_w += 2;
                    }
                    chunk.push(ch);
                    self.cur_w += cw;
                }
                if !chunk.is_empty() {
                    self.cur.push(Span::styled(chunk, style));
                }
            }
            self.content = true;
            self.break_line();
        }
    }

    fn finish(&mut self) {
        self.newline();
    }
}

#[cfg(test)]
mod tests;
