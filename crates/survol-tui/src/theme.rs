//! Colours of the diff: added and removed lines, their sign blocks and line
//! numbers, the changed words, the cursor and the selection.
//!
//! The default, "amethyst", is tuned for Catppuccin Mocha: added lines are a
//! dark violet, removed lines a dark amber, and every syntax colour keeps at
//! least 7:1 (4.5:1 for comments) on them. `[theme]` in the config overrides
//! any role with a `#rrggbb` value.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use ratatui::style::Color;
use survol_core::config::ThemeConfig;

/// Colour scheme of the code: an RGB syntect theme, or the terminal palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// The Catppuccin Mocha tmTheme (Neovim's `catppuccin-mocha`).
    CatppuccinMocha,
    /// syntect's `ansi` theme: the terminal's 16 colours, comments excepted
    /// (they would share the strings' green).
    Ansi,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub syntax: Syntax,
    pub added_bg: Color,
    pub removed_bg: Color,
    /// Words that differ between a removed line and its added counterpart.
    pub added_word_bg: Color,
    pub removed_word_bg: Color,
    /// The one-cell block holding the `+` / `-` sign.
    pub added_sign_bg: Color,
    pub removed_sign_bg: Color,
    pub added_sign: Color,
    pub removed_sign: Color,
    pub added_line_nr: Color,
    pub removed_line_nr: Color,
    /// Line numbers of context lines.
    pub line_nr: Color,
    /// Code of a hunk marked reviewed (instead of the terminal's faint).
    pub reviewed: Color,
    /// Comments with the `ansi` syntax theme.
    pub comment: Color,
    pub cursor_bg: Color,
    /// `V` selection.
    pub select_bg: Color,
    /// Cursor of a list that does not have the focus.
    pub inactive_cursor_bg: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            syntax: Syntax::CatppuccinMocha,
            added_bg: rgb(0x302145),
            removed_bg: rgb(0x322417),
            added_word_bg: rgb(0x402064),
            removed_word_bg: rgb(0x502700),
            added_sign_bg: rgb(0x623a91),
            removed_sign_bg: rgb(0x774500),
            added_sign: rgb(0xf3e9ff),
            removed_sign: rgb(0xffecc9),
            added_line_nr: rgb(0xb89ae5),
            removed_line_nr: rgb(0xdba569),
            line_nr: rgb(0x868ba3),
            reviewed: rgb(0xa6adc8),
            comment: rgb(0x9399b2),
            cursor_bg: Color::Rgb(60, 60, 80),
            select_bg: Color::Rgb(110, 90, 20),
            inactive_cursor_bg: Color::Rgb(40, 40, 50),
        }
    }
}

impl Theme {
    /// The default theme with the `[theme]` overrides applied.
    pub fn from_config(cfg: &ThemeConfig) -> Result<Self, String> {
        let mut t = Self::default();
        if let Some(s) = &cfg.syntax {
            t.syntax = match s.to_ascii_lowercase().as_str() {
                "catppuccin-mocha" | "mocha" => Syntax::CatppuccinMocha,
                "ansi" => Syntax::Ansi,
                other => {
                    return Err(format!(
                        "[theme] syntax: unknown `{other}` (catppuccin-mocha or ansi)"
                    ));
                }
            };
        }
        t.apply(&cfg.colors)?;
        Ok(t)
    }

    fn apply(&mut self, colors: &BTreeMap<String, String>) -> Result<(), String> {
        for (role, value) in colors {
            let slot = self
                .role(role)
                .ok_or_else(|| format!("[theme] unknown role `{role}`"))?;
            *slot = parse_hex(value)
                .ok_or_else(|| format!("[theme] {role}: `{value}` is not a #rrggbb colour"))?;
        }
        Ok(())
    }

    fn role(&mut self, name: &str) -> Option<&mut Color> {
        Some(match name {
            "added_bg" => &mut self.added_bg,
            "removed_bg" => &mut self.removed_bg,
            "added_word_bg" => &mut self.added_word_bg,
            "removed_word_bg" => &mut self.removed_word_bg,
            "added_sign_bg" => &mut self.added_sign_bg,
            "removed_sign_bg" => &mut self.removed_sign_bg,
            "added_sign" => &mut self.added_sign,
            "removed_sign" => &mut self.removed_sign,
            "added_line_nr" => &mut self.added_line_nr,
            "removed_line_nr" => &mut self.removed_line_nr,
            "line_nr" => &mut self.line_nr,
            "reviewed" => &mut self.reviewed,
            "comment" => &mut self.comment,
            "cursor_bg" => &mut self.cursor_bg,
            "select_bg" => &mut self.select_bg,
            "inactive_cursor_bg" => &mut self.inactive_cursor_bg,
            _ => return None,
        })
    }
}

/// `#rrggbb` (the `#` is optional).
pub fn parse_hex(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(h, 16).ok().map(rgb)
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Sets the theme once, at startup (later calls are ignored).
pub fn init(theme: Theme) {
    let _ = THEME.set(theme);
}

/// The current theme: the default until [`init`] is called.
pub fn theme() -> &'static Theme {
    THEME.get_or_init(Theme::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colours() {
        assert_eq!(parse_hex("#302145"), Some(Color::Rgb(0x30, 0x21, 0x45)));
        assert_eq!(parse_hex("ffecc9"), Some(Color::Rgb(0xff, 0xec, 0xc9)));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#30214g"), None);
    }

    #[test]
    fn overrides_roles_and_syntax() {
        let cfg = ThemeConfig {
            syntax: Some("ansi".into()),
            colors: [("added_bg".to_string(), "#112233".to_string())].into(),
        };
        let t = Theme::from_config(&cfg).unwrap();
        assert_eq!(t.syntax, Syntax::Ansi);
        assert_eq!(t.added_bg, Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(t.removed_bg, Theme::default().removed_bg);
    }

    #[test]
    fn rejects_unknown_roles_and_bad_values() {
        let bad = |k: &str, v: &str| ThemeConfig {
            syntax: None,
            colors: [(k.to_string(), v.to_string())].into(),
        };
        assert!(Theme::from_config(&bad("added", "#112233")).is_err());
        assert!(Theme::from_config(&bad("added_bg", "violet")).is_err());
        let syntax = ThemeConfig {
            syntax: Some("dracula".into()),
            ..Default::default()
        };
        assert!(Theme::from_config(&syntax).is_err());
    }
}
