//! Colours of the TUI. The diff: added and removed lines, their sign blocks
//! and line numbers, the changed words, the cursor and the selection. The
//! chrome: header, panes, footer, popups, and the colours that carry a
//! meaning everywhere (status letters, ✓, drafts, discussions, errors…).
//!
//! The default, "amethyst", is tuned for Catppuccin Mocha: added lines are a
//! dark violet, removed lines a dark amber, and every syntax colour keeps at
//! least 7:1 (4.5:1 for comments) on them. Its chrome ("mauve powerline")
//! follows the same rule: what is added is violet (mauve), what is removed is
//! amber (peach), green only means done, red only an error. `[theme]` in the
//! config overrides any role with a `#rrggbb` value.

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

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// Catppuccin Mocha.
const FLAMINGO: Color = rgb(0xf2cdcd);
const PINK: Color = rgb(0xf5c2e7);
const MAUVE: Color = rgb(0xcba6f7);
const RED: Color = rgb(0xf38ba8);
const MAROON: Color = rgb(0xeba0ac);
const PEACH: Color = rgb(0xfab387);
const YELLOW: Color = rgb(0xf9e2af);
const GREEN: Color = rgb(0xa6e3a1);
const TEAL: Color = rgb(0x94e2d5);
const SAPPHIRE: Color = rgb(0x74c7ec);
const BLUE: Color = rgb(0x89b4fa);
const LAVENDER: Color = rgb(0xb4befe);
const TEXT: Color = rgb(0xcdd6f4);
const SUBTEXT0: Color = rgb(0xa6adc8);
const OVERLAY2: Color = rgb(0x9399b2);
const OVERLAY1: Color = rgb(0x7f849c);
const SURFACE1: Color = rgb(0x45475a);
const SURFACE0: Color = rgb(0x313244);
const MANTLE: Color = rgb(0x181825);
const CRUST: Color = rgb(0x11111b);
/// Line numbers of context lines, also the quietest meaningful text.
const NR: Color = rgb(0x868ba3);

/// Declares the colour roles once: the fields of [`Theme`], their defaults
/// and their `[theme]` names (the field names).
macro_rules! theme {
    ($($(#[$doc:meta])* $name:ident = $default:expr,)*) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct Theme {
            pub syntax: Syntax,
            /// Powerline arrows between the header's blocks (they need a
            /// Nerd Font); otherwise the blocks are separated by a space.
            pub powerline: bool,
            $($(#[$doc])* pub $name: Color,)*
        }

        impl Default for Theme {
            fn default() -> Self {
                Self {
                    syntax: Syntax::CatppuccinMocha,
                    powerline: true,
                    $($name: $default,)*
                }
            }
        }

        impl Theme {
            /// The colour roles, as `[theme]` keys.
            #[cfg(test)]
            const ROLES: &'static [&'static str] = &[$(stringify!($name)),*];

            fn role(&mut self, name: &str) -> Option<&mut Color> {
                Some(match name {
                    $(stringify!($name) => &mut self.$name,)*
                    _ => return None,
                })
            }
        }
    };
}

theme! {
    // ----- diff
    added_bg = rgb(0x302145),
    removed_bg = rgb(0x322417),
    /// Words that differ between a removed line and its added counterpart.
    added_word_bg = rgb(0x402064),
    removed_word_bg = rgb(0x502700),
    /// The one-cell block holding the `+` / `-` sign.
    added_sign_bg = rgb(0x623a91),
    removed_sign_bg = rgb(0x774500),
    added_sign = rgb(0xf3e9ff),
    removed_sign = rgb(0xffecc9),
    /// Also the `+a` of file headers and the whole file's scrollbar marks.
    added_line_nr = rgb(0xb89ae5),
    removed_line_nr = rgb(0xdba569),
    /// Line numbers of context lines.
    line_nr = NR,
    /// Code of a hunk marked reviewed (instead of the terminal's faint).
    reviewed = SUBTEXT0,
    /// Comments with the `ansi` syntax theme.
    comment = OVERLAY2,
    cursor_bg = rgb(0x3a3552),
    /// `V` selection.
    select_bg = rgb(0x443a5c),
    /// Cursor of a list that does not have the focus.
    inactive_cursor_bg = rgb(0x27273a),

    // ----- chrome
    /// Text drawn on the chrome's own backgrounds (title block, file header
    /// band); elsewhere the terminal's text colour is used.
    text = TEXT,
    /// Secondary text: counters, hints, paths, reviewed files (instead of
    /// the terminal's faint, whose contrast depends on the terminal).
    meta = OVERLAY2,
    /// Dark text on the coloured blocks (badge, tabs, header, popup title).
    block_fg = CRUST,
    header_bg = MANTLE,
    badge_bg = PINK,
    tab_fg = SUBTEXT0,
    tab_bg = SURFACE0,
    tab_active_bg = MAUVE,
    /// The review's range or merge request in the header.
    title_bg = SURFACE0,
    /// Header blocks: reviewed hunks (in progress, all done), grouping,
    /// graph, threads, drafts.
    progress_bg = YELLOW,
    progress_done_bg = GREEN,
    grouping_bg = TEAL,
    graph_bg = BLUE,
    threads_bg = FLAMINGO,
    drafts_bg = PEACH,
    /// Pane borders; the focused pane's border and title.
    border = SURFACE1,
    border_focus = MAUVE,
    footer_bg = MANTLE,
    /// Keys in the footer and the help.
    key = MAUVE,
    /// Arrows, symbols, group numbers, the selected answer link.
    accent = MAUVE,
    /// Directories of the file list.
    dir = BLUE,
    /// Band behind a file header in the diff.
    file_header_bg = SURFACE0,
    /// Separator lines.
    rule = SURFACE1,
    /// `@@` hunk headers, reviewed or not.
    hunk = LAVENDER,
    hunk_reviewed = OVERLAY1,
    /// Markdown: inline code, links, bullets, quote bar and code gutter.
    code = TEAL,
    link = BLUE,
    bullet = MAUVE,
    gutter = SURFACE1,
    popup_bg = MANTLE,
    popup_border = MAUVE,
    /// Block behind a popup's title.
    popup_title_bg = MAUVE,
    /// Layers of a Stack group, sections of the Graph view.
    layer = LAVENDER,
    /// Graph relations and their counts.
    relation = BLUE,

    // ----- meaning, the same in every view
    /// File status letters A / M / D / R and C; also new / gone, + / −,
    /// after / before in the flows.
    status_added = MAUVE,
    status_modified = YELLOW,
    status_deleted = PEACH,
    status_renamed = TEAL,
    /// ✓ reviewed, complete progress.
    ok = GREEN,
    warn = YELLOW,
    error = RED,
    /// Bar and head of a draft note (its body keeps the text colour).
    draft = YELLOW,
    /// Discussions of the forge: bar, `@author`, replies.
    discussion = PINK,
    resolved = NR,
    confidence = NR,
    /// Mechanical Stack groups (renames, formatting…).
    mechanical = OVERLAY2,
    /// Graph symbols: intact, in the diff.
    intact = NR,
    in_diff = SUBTEXT0,
    /// Flow terminals: `[db]`, `[external]`, `[event]`.
    db = SAPPHIRE,
    external = MAROON,
    event = LAVENDER,
    /// Architectural layers of the flow steps.
    layer_view = PINK,
    layer_controller = BLUE,
    layer_service = TEAL,
    layer_repository = SAPPHIRE,
    layer_external = MAROON,
    layer_config = SUBTEXT0,
    layer_code = OVERLAY1,
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
        if let Some(p) = cfg.powerline {
            t.powerline = p;
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
            powerline: Some(false),
            colors: [
                ("added_bg".to_string(), "#112233".to_string()),
                ("border_focus".to_string(), "#445566".to_string()),
            ]
            .into(),
        };
        let t = Theme::from_config(&cfg).unwrap();
        assert_eq!(t.syntax, Syntax::Ansi);
        assert!(!t.powerline);
        assert_eq!(t.added_bg, Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(t.border_focus, Color::Rgb(0x44, 0x55, 0x66));
        assert_eq!(t.removed_bg, Theme::default().removed_bg);
        assert!(Theme::default().powerline);
    }

    #[test]
    fn rejects_unknown_roles_and_bad_values() {
        let bad = |k: &str, v: &str| ThemeConfig {
            colors: [(k.to_string(), v.to_string())].into(),
            ..Default::default()
        };
        assert!(Theme::from_config(&bad("added", "#112233")).is_err());
        assert!(Theme::from_config(&bad("added_bg", "violet")).is_err());
        let syntax = ThemeConfig {
            syntax: Some("dracula".into()),
            ..Default::default()
        };
        assert!(Theme::from_config(&syntax).is_err());
    }

    #[test]
    fn every_role_is_settable() {
        let mut t = Theme::default();
        for r in Theme::ROLES {
            assert!(t.role(r).is_some(), "{r}");
        }
        assert!(Theme::ROLES.len() > 60);
    }
}
