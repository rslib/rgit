//! User configuration, loaded once at startup from a TOML file.
//!
//! Resolution order for the config path:
//! 1. `$RGIT_CONFIG` (an explicit file path), else
//! 2. `$XDG_CONFIG_HOME/rgit/config.toml`, else
//! 3. `$HOME/.config/rgit/config.toml`.
//!
//! A missing file is not an error - defaults apply. A malformed file falls back
//! to defaults and reports the parse error so startup is never blocked.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

/// The whole config tree. Every field is optional so a partial file is valid.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: ThemeConfig,
    pub ui: UiConfig,
    pub commit: CommitConfig,
    /// Keybinding profile: `"magit"` (default, single-letter actions + emacs
    /// nav) or `"vim"` (hjkl motion, visual select, Space leader). Unknown
    /// names fall back to the default.
    pub profile: Option<String>,
    /// Action-name to key overrides, e.g. `stage = "s"`. Consumed by the keymap
    /// once remapping lands; unknown action names are simply ignored there.
    pub keys: HashMap<String, String>,
}

/// Commit behavior.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommitConfig {
    /// Sign commits (`git commit -S`), honoring the user's git signing config.
    pub gpg_sign: bool,
}

/// Theme selection: a named built-in palette plus per-role color overrides.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    /// A built-in palette name (e.g. `"rust"`, `"mono"`). Unknown names use the
    /// default and are otherwise ignored.
    pub name: Option<String>,
    /// Hex overrides (`"#rrggbb"`) applied on top of the named palette.
    pub accent: Option<String>,
    pub section_header: Option<String>,
    pub field_label: Option<String>,
    pub branch: Option<String>,
    pub hash: Option<String>,
    pub dim: Option<String>,
    pub added: Option<String>,
    pub modified: Option<String>,
    pub deleted: Option<String>,
    pub untracked: Option<String>,
    pub cursor_bg: Option<String>,
    pub select_bg: Option<String>,
}

/// Behavioral toggles.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// How many commits the log view walks by default.
    pub log_limit: usize,
    /// Glyph palette: `ascii`, `unicode`, or `nerd`. Unset falls back to
    /// `nerd_fonts` for backward compatibility.
    pub glyphs: Option<GlyphMode>,
    /// Deprecated: `true` is equivalent to `glyphs = "nerd"`.
    pub nerd_fonts: bool,
    /// Accept mouse events (click-to-focus, wheel scroll).
    pub mouse: bool,
    /// Render read-only diffs side by side.
    pub side_by_side: bool,
    /// Which regions let the terminal background show through.
    pub transparent: Transparency,
}

/// Glyph palette choice, mapped onto [`rgit_model::GlyphMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GlyphMode {
    /// Pure ASCII everywhere.
    Ascii,
    /// Unicode symbols (chevrons, bars, arrows), no nerd-font icons.
    Unicode,
    /// Unicode plus nerd-font file/git/section icons.
    Nerd,
}

impl UiConfig {
    /// The effective glyph mode: the explicit `glyphs` setting, else derived
    /// from the legacy `nerd_fonts` toggle.
    pub fn glyph_mode(&self) -> rgit_model::GlyphMode {
        match self.glyphs {
            Some(GlyphMode::Ascii) => rgit_model::GlyphMode::Ascii,
            Some(GlyphMode::Unicode) => rgit_model::GlyphMode::Unicode,
            Some(GlyphMode::Nerd) => rgit_model::GlyphMode::Nerd,
            None if self.nerd_fonts => rgit_model::GlyphMode::Nerd,
            None => rgit_model::GlyphMode::Unicode,
        }
    }
}

/// How much of the theme's solid background to paint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transparency {
    /// Paint the whole app solid (matches the design).
    #[default]
    Off,
    /// Keep the header and action bar solid; let the body show through.
    Body,
    /// Paint nothing; the whole app is see-through.
    Full,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            log_limit: 200,
            glyphs: None,
            nerd_fonts: false,
            mouse: false,
            side_by_side: false,
            transparent: Transparency::Off,
        }
    }
}

/// The resolved config-file path, if any of the candidates exist.
pub fn config_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("RGIT_CONFIG") {
        let path = PathBuf::from(explicit);
        return path.exists().then_some(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let path = base.join("rgit").join("config.toml");
    path.exists().then_some(path)
}

/// Load the config, returning defaults (and an error string) when the file is
/// absent or malformed. The error is surfaced to the user, not swallowed.
pub fn load() -> (Config, Option<String>) {
    let Some(path) = config_path() else {
        return (Config::default(), None);
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(config) => (config, None),
            Err(e) => (
                Config::default(),
                Some(format!("config {}: {e}", path.display())),
            ),
        },
        Err(e) => (
            Config::default(),
            Some(format!("config {}: {e}", path.display())),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_partial_config() {
        let cfg: Config = toml::from_str(
            r##"
            [theme]
            name = "mono"
            accent = "#ff00ff"

            [ui]
            log_limit = 50
            "##,
        )
        .unwrap();
        assert_eq!(cfg.theme.name.as_deref(), Some("mono"));
        assert_eq!(cfg.theme.accent.as_deref(), Some("#ff00ff"));
        assert_eq!(cfg.ui.log_limit, 50);
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.ui.log_limit, 200);
        assert!(cfg.theme.name.is_none());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("[theme]\nnope = 1\n").is_err());
    }
}
