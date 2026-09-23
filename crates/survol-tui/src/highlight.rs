//! Lazy syntax highlighting of hunks with syntect.
//!
//! Uses the `ansi` theme so colours follow the terminal palette (light or dark).

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use survol_core::model::{Diff, Hunk};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use two_face::theme::EmbeddedThemeName;

pub type Spans = Vec<(Style, String)>;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
    /// Highlighted lines per hunk id, filled on first display.
    cache: HashMap<usize, Vec<Spans>>,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntaxes: two_face::syntax::extra_newlines(),
            theme: two_face::theme::extra()
                .get(EmbeddedThemeName::Ansi)
                .clone(),
            cache: HashMap::new(),
        }
    }

    pub fn hunk(&mut self, diff: &Diff, hunk: usize) -> &[Spans] {
        if !self.cache.contains_key(&hunk) {
            let h = &diff.hunks[hunk];
            let spans = self.highlight(&diff.files[h.file].path, h);
            self.cache.insert(hunk, spans);
        }
        &self.cache[&hunk]
    }

    fn syntax(&self, path: &str) -> Option<&SyntaxReference> {
        let name = path.rsplit('/').next().unwrap_or(path);
        let ext = name.rsplit_once('.').map_or(name, |(_, e)| e);
        self.syntaxes
            .find_syntax_by_extension(ext)
            .or_else(|| self.syntaxes.find_syntax_by_extension(name))
    }

    fn highlight(&self, path: &str, hunk: &Hunk) -> Vec<Spans> {
        // One pass over the hunk in display order: the removed and added sides
        // share the parser state, which is good enough for a diff.
        self.lines(path, hunk.lines.iter().map(|l| l.text.as_str()))
    }

    /// Highlights consecutive `lines` of the file `path` (plain text when the
    /// language is unknown).
    pub fn lines<'a>(
        &self,
        path: &str,
        lines: impl Iterator<Item = &'a str> + Clone,
    ) -> Vec<Spans> {
        let plain = || {
            lines
                .clone()
                .map(|l| vec![(Style::default(), expand_tabs(l))])
                .collect()
        };
        // Pathological lines (minified code) are not worth highlighting.
        let Some(syntax) = self.syntax(path) else {
            return plain();
        };
        if lines.clone().any(|l| l.len() > 2000) {
            return plain();
        }
        let mut h = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in lines.clone() {
            let text = format!("{}\n", expand_tabs(line));
            match h.highlight_line(&text, &self.syntaxes) {
                Ok(regions) => out.push(
                    regions
                        .into_iter()
                        .map(|(s, t)| (convert(s), t.trim_end_matches('\n').to_string()))
                        .filter(|(_, t)| !t.is_empty())
                        .collect(),
                ),
                Err(_) => return plain(),
            }
        }
        out
    }
}

pub fn expand_tabs(s: &str) -> String {
    if s.contains('\t') {
        s.replace('\t', "    ")
    } else {
        s.to_string()
    }
}

/// Converts the `ansi` theme encoding: alpha 0 means "palette index in red",
/// alpha 1 means "terminal default".
fn convert(s: syntect::highlighting::Style) -> Style {
    let c = s.foreground;
    let fg = match c.a {
        0 => Some(match c.r {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            n => Color::Indexed(n),
        }),
        1 => None,
        _ => Some(Color::Rgb(c.r, c.g, c.b)),
    };
    let mut style = Style::default();
    if let Some(fg) = fg {
        style = style.fg(fg);
    }
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    style
}
