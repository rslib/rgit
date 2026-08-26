//! Data-driven theming.
//!
//! A [`Theme`] maps each semantic [`Style`] role to a concrete color. The active
//! theme is set once at startup (from config) into a process global, so the
//! render path can call [`resolve`]/[`accent`] without threading a theme handle
//! through every widget.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style as RStyle};
use rgit_model::Style;

use crate::config::ThemeConfig;

/// A full set of role colors.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub accent: Color,
    pub section_header: Color,
    pub field_label: Color,
    pub branch: Color,
    pub hash: Color,
    pub dim: Color,
    pub added: Color,
    pub modified: Color,
    pub deleted: Color,
    pub untracked: Color,
    /// Background of the row under the cursor.
    pub cursor_bg: Color,
    /// Background of a selected (marked) row.
    pub select_bg: Color,
    /// The whole-screen ground; panels and popups lighten from it.
    pub base_bg: Color,
}

impl Theme {
    /// The default "rust" palette: warm orange accent on a dark terminal.
    pub const fn rust() -> Self {
        Self {
            accent: Color::Rgb(251, 146, 60),
            section_header: Color::Rgb(251, 146, 60),
            field_label: Color::Rgb(229, 192, 123),
            branch: Color::Rgb(97, 175, 239),
            hash: Color::Rgb(229, 192, 123),
            dim: Color::DarkGray,
            added: Color::Rgb(121, 194, 103),
            modified: Color::Rgb(229, 192, 123),
            deleted: Color::Rgb(224, 108, 117),
            untracked: Color::Rgb(224, 108, 117),
            cursor_bg: Color::Rgb(42, 34, 26),
            select_bg: Color::Rgb(30, 42, 54),
            base_bg: Color::Rgb(0x0d, 0x0e, 0x11),
        }
    }

    /// A restrained, low-chroma palette for monochrome-leaning terminals.
    pub const fn mono() -> Self {
        Self {
            accent: Color::Rgb(197, 200, 198),
            section_header: Color::Rgb(197, 200, 198),
            field_label: Color::Rgb(150, 152, 150),
            branch: Color::Rgb(170, 190, 210),
            hash: Color::Rgb(150, 152, 150),
            dim: Color::DarkGray,
            added: Color::Rgb(150, 190, 150),
            modified: Color::Rgb(200, 190, 150),
            deleted: Color::Rgb(200, 150, 150),
            untracked: Color::Rgb(200, 150, 150),
            cursor_bg: Color::Rgb(38, 38, 38),
            select_bg: Color::Rgb(30, 40, 48),
            base_bg: Color::Rgb(0x0b, 0x0b, 0x0b),
        }
    }

    /// Catppuccin Mocha.
    pub const fn catppuccin() -> Self {
        Self {
            accent: Color::Rgb(0xcb, 0xa6, 0xf7),         // mauve
            section_header: Color::Rgb(0xcb, 0xa6, 0xf7), // mauve
            field_label: Color::Rgb(0xf9, 0xe2, 0xaf),    // yellow
            branch: Color::Rgb(0x89, 0xb4, 0xfa),         // blue
            hash: Color::Rgb(0xf9, 0xe2, 0xaf),           // yellow
            dim: Color::Rgb(0x6c, 0x70, 0x86),            // overlay0
            added: Color::Rgb(0xa6, 0xe3, 0xa1),          // green
            modified: Color::Rgb(0xf9, 0xe2, 0xaf),       // yellow
            deleted: Color::Rgb(0xf3, 0x8b, 0xa8),        // red
            untracked: Color::Rgb(0xf3, 0x8b, 0xa8),      // red
            cursor_bg: Color::Rgb(0x31, 0x32, 0x44),      // surface0
            select_bg: Color::Rgb(0x45, 0x47, 0x5a),
            base_bg: Color::Rgb(0x18, 0x18, 0x25), // surface1
        }
    }

    /// Gruvbox Dark.
    pub const fn gruvbox() -> Self {
        Self {
            accent: Color::Rgb(0xfe, 0x80, 0x19),         // orange
            section_header: Color::Rgb(0xfe, 0x80, 0x19), // orange
            field_label: Color::Rgb(0xfa, 0xbd, 0x2f),    // yellow
            branch: Color::Rgb(0x83, 0xa5, 0x98),         // blue
            hash: Color::Rgb(0xfa, 0xbd, 0x2f),           // yellow
            dim: Color::Rgb(0x92, 0x83, 0x74),            // gray
            added: Color::Rgb(0xb8, 0xbb, 0x26),          // green
            modified: Color::Rgb(0xfa, 0xbd, 0x2f),       // yellow
            deleted: Color::Rgb(0xfb, 0x49, 0x34),        // red
            untracked: Color::Rgb(0xfb, 0x49, 0x34),      // red
            cursor_bg: Color::Rgb(0x3c, 0x38, 0x36),      // bg1
            select_bg: Color::Rgb(0x50, 0x49, 0x45),
            base_bg: Color::Rgb(0x1d, 0x20, 0x21), // bg2
        }
    }

    /// Nord.
    pub const fn nord() -> Self {
        Self {
            accent: Color::Rgb(0x88, 0xc0, 0xd0),         // nord8 frost
            section_header: Color::Rgb(0x88, 0xc0, 0xd0), // nord8 frost
            field_label: Color::Rgb(0xeb, 0xcb, 0x8b),    // nord13 yellow
            branch: Color::Rgb(0x81, 0xa1, 0xc1),         // nord9 blue
            hash: Color::Rgb(0xeb, 0xcb, 0x8b),           // nord13 yellow
            dim: Color::Rgb(0x4c, 0x56, 0x6a),            // nord3
            added: Color::Rgb(0xa3, 0xbe, 0x8c),          // nord14 green
            modified: Color::Rgb(0xeb, 0xcb, 0x8b),       // nord13 yellow
            deleted: Color::Rgb(0xbf, 0x61, 0x6a),        // nord11 red
            untracked: Color::Rgb(0xbf, 0x61, 0x6a),      // nord11 red
            cursor_bg: Color::Rgb(0x3b, 0x42, 0x52),      // nord1
            select_bg: Color::Rgb(0x43, 0x4c, 0x5e),
            base_bg: Color::Rgb(0x2e, 0x34, 0x40), // nord2
        }
    }

    /// Dracula.
    pub const fn dracula() -> Self {
        Self {
            accent: Color::Rgb(0xff, 0x79, 0xc6),         // pink
            section_header: Color::Rgb(0xff, 0x79, 0xc6), // pink
            field_label: Color::Rgb(0xf1, 0xfa, 0x8c),    // yellow
            branch: Color::Rgb(0x8b, 0xe9, 0xfd),         // cyan
            hash: Color::Rgb(0xf1, 0xfa, 0x8c),           // yellow
            dim: Color::Rgb(0x62, 0x72, 0xa4),            // comment
            added: Color::Rgb(0x50, 0xfa, 0x7b),          // green
            modified: Color::Rgb(0xf1, 0xfa, 0x8c),       // yellow
            deleted: Color::Rgb(0xff, 0x55, 0x55),        // red
            untracked: Color::Rgb(0xff, 0x55, 0x55),      // red
            cursor_bg: Color::Rgb(0x34, 0x37, 0x46),      // current line
            select_bg: Color::Rgb(0x44, 0x47, 0x5a),
            base_bg: Color::Rgb(0x21, 0x22, 0x2c), // selection
        }
    }

    /// Tokyo Night.
    pub const fn tokyonight() -> Self {
        Self {
            accent: Color::Rgb(0x7a, 0xa2, 0xf7),         // blue
            section_header: Color::Rgb(0x7a, 0xa2, 0xf7), // blue
            field_label: Color::Rgb(0xe0, 0xaf, 0x68),    // orange
            branch: Color::Rgb(0x7d, 0xcf, 0xff),         // cyan
            hash: Color::Rgb(0xe0, 0xaf, 0x68),           // orange
            dim: Color::Rgb(0x56, 0x5f, 0x89),            // comment
            added: Color::Rgb(0x9e, 0xce, 0x6a),          // green
            modified: Color::Rgb(0xe0, 0xaf, 0x68),       // orange
            deleted: Color::Rgb(0xf7, 0x76, 0x8e),        // red
            untracked: Color::Rgb(0xf7, 0x76, 0x8e),      // red
            cursor_bg: Color::Rgb(0x29, 0x2e, 0x42),      // bg highlight
            select_bg: Color::Rgb(0x33, 0x46, 0x7c),
            base_bg: Color::Rgb(0x16, 0x16, 0x1e), // selection
        }
    }

    /// Rose Pine Moon.
    pub const fn rose_pine() -> Self {
        Self {
            accent: Color::Rgb(0xc4, 0xa7, 0xe7),         // iris
            section_header: Color::Rgb(0xc4, 0xa7, 0xe7), // iris
            field_label: Color::Rgb(0xf6, 0xc1, 0x77),    // gold
            branch: Color::Rgb(0x9c, 0xcf, 0xd8),         // foam
            hash: Color::Rgb(0xf6, 0xc1, 0x77),           // gold
            dim: Color::Rgb(0x6e, 0x6a, 0x86),            // muted
            added: Color::Rgb(0x3e, 0x8f, 0xb0),          // pine
            modified: Color::Rgb(0xf6, 0xc1, 0x77),       // gold
            deleted: Color::Rgb(0xeb, 0x6f, 0x92),        // love
            untracked: Color::Rgb(0xea, 0x9a, 0x97),      // rose
            cursor_bg: Color::Rgb(0x44, 0x41, 0x5a),      // highlight-med
            select_bg: Color::Rgb(0x39, 0x35, 0x52),
            base_bg: Color::Rgb(0x23, 0x21, 0x36), // overlay
        }
    }

    /// Resolve a built-in palette by name; unknown names fall back to `rust`.
    pub fn named(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "mono" | "monochrome" => Self::mono(),
            "catppuccin" | "catppuccin-mocha" => Self::catppuccin(),
            "gruvbox" | "gruvbox-dark" => Self::gruvbox(),
            "nord" => Self::nord(),
            "dracula" => Self::dracula(),
            "tokyonight" | "tokyo-night" | "tokyonight-night" => Self::tokyonight(),
            "rose-pine" | "rosepine" | "rose-pine-moon" | "rosepine-moon" => Self::rose_pine(),
            "terminal" | "ansi" => Self::terminal(),
            _ => Self::rust(),
        }
    }

    /// Adopt the terminal's own 16-color scheme: roles map to ANSI colors and
    /// the background is the terminal default, so rgit matches whatever palette
    /// the terminal is configured with.
    pub fn terminal() -> Self {
        Self {
            accent: Color::Yellow,
            section_header: Color::Yellow,
            field_label: Color::Yellow,
            branch: Color::Blue,
            hash: Color::Cyan,
            dim: Color::DarkGray,
            added: Color::Green,
            modified: Color::Yellow,
            deleted: Color::Red,
            untracked: Color::Red,
            cursor_bg: Color::Indexed(8), // bright black: a subtle row highlight
            select_bg: Color::Indexed(8),
            base_bg: Color::Reset, // the terminal's own background
        }
    }

    /// Build a theme from config: start from the named palette (or the default),
    /// then apply any per-role hex overrides that parse.
    pub fn from_config(cfg: &ThemeConfig) -> Self {
        let mut theme = cfg.name.as_deref().map(Self::named).unwrap_or_default();
        let set = |slot: &mut Color, hex: &Option<String>| {
            if let Some(color) = hex.as_deref().and_then(parse_hex) {
                *slot = color;
            }
        };
        set(&mut theme.accent, &cfg.accent);
        set(&mut theme.section_header, &cfg.section_header);
        set(&mut theme.field_label, &cfg.field_label);
        set(&mut theme.branch, &cfg.branch);
        set(&mut theme.hash, &cfg.hash);
        set(&mut theme.dim, &cfg.dim);
        set(&mut theme.added, &cfg.added);
        set(&mut theme.modified, &cfg.modified);
        set(&mut theme.deleted, &cfg.deleted);
        set(&mut theme.untracked, &cfg.untracked);
        set(&mut theme.cursor_bg, &cfg.cursor_bg);
        set(&mut theme.select_bg, &cfg.select_bg);
        theme
    }

    /// Map a semantic model style role to a concrete terminal style.
    pub fn style(&self, style: Style) -> RStyle {
        match style {
            Style::Plain => RStyle::default(),
            Style::SectionHeader => RStyle::default()
                .fg(self.section_header)
                .add_modifier(Modifier::BOLD),
            Style::FieldLabel => RStyle::default().fg(self.field_label),
            Style::Branch => RStyle::default().fg(self.branch),
            Style::Hash => RStyle::default().fg(self.hash),
            Style::Dim => RStyle::default().fg(self.dim),
            Style::Added => RStyle::default().fg(self.added),
            Style::Modified => RStyle::default().fg(self.modified),
            Style::Deleted => RStyle::default().fg(self.deleted),
            Style::Untracked => RStyle::default().fg(self.untracked),
            // A fixed dark wash behind the changed run reads on any dark palette.
            Style::WordAdded => RStyle::default()
                .fg(self.added)
                .bg(Color::Rgb(0x2f, 0x51, 0x36)),
            Style::WordDeleted => RStyle::default()
                .fg(self.deleted)
                .bg(Color::Rgb(0x5a, 0x2b, 0x32)),
            Style::Rgb(r, g, b) => RStyle::default().fg(Color::Rgb(r, g, b)),
            Style::AddedBg => RStyle::default().bg(Color::Rgb(0x13, 0x24, 0x18)),
            Style::DeletedBg => RStyle::default().bg(Color::Rgb(0x28, 0x18, 0x1c)),
        }
    }

    /// The representative color for a role, used when a role is applied as a
    /// span background rather than a foreground.
    pub fn color(&self, style: Style) -> Color {
        match style {
            Style::AddedBg => Color::Rgb(0x13, 0x24, 0x18),
            Style::DeletedBg => Color::Rgb(0x28, 0x18, 0x1c),
            Style::Rgb(r, g, b) => Color::Rgb(r, g, b),
            other => match self.style(other).fg {
                Some(c) => c,
                None => Color::Reset,
            },
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        // With no theme configured, adopt the terminal's own colors.
        Self::terminal()
    }
}

/// Parse `#rrggbb` (or `rrggbb`) into a color. Returns `None` on any malformed
/// input so a bad override is simply ignored.
fn parse_hex(hex: &str) -> Option<Color> {
    let h = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Install the active theme. Called once at startup; later calls are ignored.
pub fn init(theme: Theme) {
    let _ = THEME.set(theme);
}

/// The active theme, or the default if none was installed.
pub fn current() -> Theme {
    *THEME.get().unwrap_or(&Theme::rust())
}

/// Map a semantic model style role to a concrete terminal style.
pub fn resolve(style: Style) -> RStyle {
    current().style(style)
}

pub fn accent() -> Color {
    current().accent
}

/// The theme's solid background, painted behind the app unless transparent.
pub fn base_bg() -> Color {
    current().base_bg
}

/// Map the palette onto syntax roles so diff highlighting matches the theme.
pub fn syntax_colors() -> rgit_model::SyntaxColors {
    let t = current();
    // syntect needs concrete RGB; approximate the ANSI roles for the terminal
    // theme so highlighting still tracks the palette's intent.
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Red => (0xcd, 0x31, 0x31),
        Color::Green => (0x31, 0xcd, 0x50),
        Color::Yellow => (0xcd, 0xa8, 0x31),
        Color::Blue => (0x51, 0x7d, 0xcd),
        Color::Magenta => (0xb8, 0x51, 0xcd),
        Color::Cyan => (0x31, 0xb8, 0xcd),
        Color::Gray => (0xd0, 0xd0, 0xd0),
        Color::DarkGray => (0x80, 0x80, 0x80),
        _ => (0xcc, 0xcc, 0xcc),
    };
    rgit_model::SyntaxColors {
        // A light neutral for uncolored code, lifted from the background hue.
        fg: rgb(lighten(t.base_bg, 0x90)),
        comment: rgb(t.dim),
        keyword: rgb(t.accent),
        string: rgb(t.added),
        number: rgb(t.hash),
        function: rgb(t.field_label),
        type_name: rgb(t.branch),
    }
}

/// The concrete color for a role used as a span background.
pub fn bg_color(style: Style) -> Color {
    current().color(style)
}

/// Lighten a color toward white by `amount` per channel (saturating).
fn lighten(color: Color, amount: u8) -> Color {
    match color {
        Color::Rgb(r, g, b) => Color::Rgb(
            r.saturating_add(amount),
            g.saturating_add(amount),
            b.saturating_add(amount),
        ),
        other => other,
    }
}

/// The preview pane's raised surface: one tier up from the ground, so the two
/// panes read as stacked surfaces without a heavy divider.
pub fn surface_bg() -> Color {
    lighten(current().base_bg, 0x08)
}

/// The floating-popup background: two tiers up from the ground.
pub fn overlay_bg() -> Color {
    lighten(current().base_bg, 0x14)
}

/// Blend `a` toward `b` by `t` in `[0,255]` (0 = all `a`, 255 = all `b`).
fn blend(a: Color, b: Color, t: u8) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let mix =
                |x: u8, y: u8| ((x as u16 * (255 - t as u16) + y as u16 * t as u16) / 255) as u8;
            Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
        }
        (a, _) => a,
    }
}

/// The frame and pane-divider color: a muted accent, blended most of the way to
/// the background so the borders read as tinted without shouting.
pub fn border() -> Color {
    let t = current();
    match t.base_bg {
        Color::Rgb(..) => blend(t.accent, t.base_bg, 0xB0),
        // The terminal theme has no RGB to blend; a dim border reads cleanly.
        _ => t.dim,
    }
}

pub fn cursor_bg() -> Color {
    current().cursor_bg
}

pub fn select_bg() -> Color {
    current().select_bg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors() {
        assert_eq!(parse_hex("#ff9200"), Some(Color::Rgb(255, 146, 0)));
        assert_eq!(parse_hex("ff9200"), Some(Color::Rgb(255, 146, 0)));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("nope"), None);
    }

    #[test]
    fn config_overrides_named_palette() {
        let cfg = ThemeConfig {
            name: Some("mono".into()),
            accent: Some("#123456".into()),
            ..Default::default()
        };
        let theme = Theme::from_config(&cfg);
        assert_eq!(theme.accent, Color::Rgb(0x12, 0x34, 0x56));
        // untouched roles keep the mono palette
        assert_eq!(theme.branch, Theme::mono().branch);
    }

    #[test]
    fn named_resolves_builtins_and_aliases() {
        assert_eq!(Theme::named("rose-pine").accent, Theme::rose_pine().accent);
        assert_eq!(
            Theme::named("catppuccin").accent,
            Theme::catppuccin().accent
        );
        assert_eq!(
            Theme::named("tokyo-night").accent,
            Theme::tokyonight().accent
        );
        // unknown names fall back to the default rather than erroring
        assert_eq!(Theme::named("no-such-theme").accent, Theme::rust().accent);
    }
}
