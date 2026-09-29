//! Lazy syntax highlighting of hunks with syntect.
//!
//! Uses the Catppuccin Mocha theme by default, or the `ansi` one (the
//! terminal palette) when `[theme] syntax = "ansi"`; see [`crate::theme`].

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use survol_core::model::{Diff, Hunk};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, StyleModifier, Theme, ThemeItem};
use syntect::parsing::{ScopeStack, SyntaxReference, SyntaxSet};
use two_face::theme::EmbeddedThemeName;

use crate::intraline::{self, Words};
use crate::theme::{self, Syntax};

pub type Spans = Vec<(Style, String)>;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
    /// Highlighted lines per hunk id, filled on first display.
    cache: HashMap<usize, Vec<Spans>>,
    /// Changed words of each line per hunk id, filled on first display.
    words: HashMap<usize, Vec<Words>>,
}

impl Highlighter {
    pub fn new() -> Self {
        Self::with_syntax(theme::theme().syntax)
    }

    fn with_syntax(syntax: Syntax) -> Self {
        let themes = two_face::theme::extra();
        let theme = match syntax {
            Syntax::CatppuccinMocha => themes.get(EmbeddedThemeName::CatppuccinMocha).clone(),
            Syntax::Ansi => {
                let mut t = themes.get(EmbeddedThemeName::Ansi).clone();
                recolor_comments(&mut t);
                t
            }
        };
        Self {
            syntaxes: two_face::syntax::extra_newlines(),
            theme,
            cache: HashMap::new(),
            words: HashMap::new(),
        }
    }

    /// Highlighted lines of a hunk and the changed words of each line.
    pub fn hunk_words(&mut self, diff: &Diff, hunk: usize) -> (&[Spans], &[Words]) {
        self.words.entry(hunk).or_insert_with(|| {
            let lines = &diff.hunks[hunk].lines;
            let text: Vec<String> = lines.iter().map(|l| expand_tabs(&l.text)).collect();
            intraline::hunk(lines.iter().zip(&text).map(|(l, t)| (l.kind, t.as_str())))
        });
        self.hunk(diff, hunk);
        (&self.cache[&hunk], &self.words[&hunk])
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
        self.highlight_with(self.syntax(path), lines)
    }

    /// Highlights a code block tagged `lang` (`java`, `ts`, `rust`, `yaml`...).
    pub fn code<'a>(&self, lang: &str, lines: impl Iterator<Item = &'a str> + Clone) -> Vec<Spans> {
        let syntax = (!lang.is_empty())
            .then(|| self.syntaxes.find_syntax_by_token(lang))
            .flatten();
        self.highlight_with(syntax, lines)
    }

    fn highlight_with<'a>(
        &self,
        syntax: Option<&SyntaxReference>,
        lines: impl Iterator<Item = &'a str> + Clone,
    ) -> Vec<Spans> {
        let plain = || {
            lines
                .clone()
                .map(|l| vec![(Style::default(), expand_tabs(l))])
                .collect()
        };
        // Pathological lines (minified code) are not worth highlighting.
        let Some(syntax) = syntax else {
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

/// The `ansi` theme gives comments the strings' green: give them the
/// theme's comment colour, in italics, by recolouring every rule that
/// matches a comment and adding a `comment` rule first (ties go to the
/// earliest rule).
fn recolor_comments(t: &mut Theme) {
    let Color::Rgb(r, g, b) = theme::theme().comment else {
        return;
    };
    let style = StyleModifier {
        foreground: Some(syntect::highlighting::Color { r, g, b, a: 255 }),
        background: None,
        font_style: Some(FontStyle::ITALIC),
    };
    let comment: ScopeStack = "comment.line".parse().expect("valid scope");
    for item in &mut t.scopes {
        if item.scope.does_match(comment.as_slice()).is_some() {
            item.style = style;
        }
    }
    t.scopes.insert(
        0,
        ThemeItem {
            scope: "comment".parse().expect("valid selector"),
            style,
        },
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fg_of(h: &Highlighter, src: &str, token: &str) -> Option<Color> {
        let spans = h.lines("x.ts", std::iter::once(src));
        spans[0]
            .iter()
            .find(|(_, t)| t.contains(token))
            .and_then(|(s, _)| s.fg)
    }

    #[test]
    fn mocha_comments_differ_from_strings() {
        let h = Highlighter::with_syntax(Syntax::CatppuccinMocha);
        let src = "const a = 'txt'; // note";
        assert_eq!(fg_of(&h, src, "note"), Some(Color::Rgb(0x93, 0x99, 0xb2)));
        assert_eq!(fg_of(&h, src, "txt"), Some(Color::Rgb(0xa6, 0xe3, 0xa1)));
    }

    #[test]
    fn ansi_comments_are_recoloured() {
        let h = Highlighter::with_syntax(Syntax::Ansi);
        let src = "const a = 'txt'; // note";
        assert_eq!(fg_of(&h, src, "note"), Some(theme::theme().comment));
        assert_eq!(fg_of(&h, src, "txt"), Some(Color::Green));
    }
}
