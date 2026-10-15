//! User configuration, built once at startup from the layered rgit settings.
//!
//! A missing file is not an error - defaults apply. A bad file or key is
//! reported as a warning so startup is never blocked.

use rgit_config::Settings;
use serde::Deserialize;
use std::collections::HashMap;

/// The whole config tree. Every field is optional so a partial file is valid.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeConfig,
    /// Forge account/profile selection. Credentials remain in the OS keychain.
    pub forge: rgit_forge::ForgeConfig,
    pub ui: UiConfig,
    pub commit: CommitConfig,
    /// The active keybinding profile: a built-in (`"magit"`/`"emacs"` or
    /// `"vim"`) or a custom profile named under `[profiles.<name>]`. Defaults to
    /// magit.
    pub profile: Option<String>,
    /// Per-profile key overrides, grouped by profile name: `[keys.emacs]`,
    /// `[keys.vim]`, or `[keys.<custom>]`. Each entry maps an action name to a
    /// key spec - a bare character, or a chord with `C-`/`^` (ctrl), `M-`/`A-`
    /// (alt), the `C-x` prefix, or a named key (`Up`, `RET`, `Tab`, `Spc`, ...).
    /// A custom profile inherits its base's bindings, then applies its own. An
    /// unknown action or an unparseable key is ignored, never fatal.
    pub keys: HashMap<String, HashMap<String, String>>,
    /// User-defined profiles: `[profiles.<name>] extends = "emacs"` bases a new
    /// profile on a built-in (or another custom profile), which its own
    /// `[keys.<name>]` then customizes.
    pub profiles: HashMap<String, ProfileDef>,
}

/// One custom profile: a base to inherit from, then its own `[keys.<name>]`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ProfileDef {
    /// The profile this one is based on (`"emacs"`/`"magit"`, `"vim"`, or
    /// another custom name). Defaults to magit when omitted.
    pub extends: Option<String>,
}

impl Config {
    /// Resolve the configured profile into the built-in base it rests on and the
    /// flat action->key overrides to apply (a custom profile's inheritance chain
    /// merged base-first, so the most-derived binding wins). Cycles are broken.
    pub fn resolved_keymap(&self) -> (String, HashMap<String, String>) {
        let is_builtin = |n: &str| matches!(n, "magit" | "emacs" | "vim");
        let active = self.profile.clone().unwrap_or_else(|| "magit".to_owned());

        // Walk the extends chain from the active profile toward a built-in.
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut name = active;
        while seen.insert(name.clone()) {
            chain.push(name.clone());
            if is_builtin(&name) {
                break;
            }
            match self.profiles.get(&name).and_then(|p| p.extends.clone()) {
                Some(base) => name = base,
                None => break, // a custom profile with no base falls back to magit
            }
        }

        let base = chain
            .iter()
            .rev()
            .find(|n| is_builtin(n))
            .cloned()
            .unwrap_or_else(|| "magit".to_owned());

        // Apply overrides base-first so derived profiles win.
        let mut overrides = HashMap::new();
        for profile in chain.iter().rev() {
            if let Some(map) = self.keys.get(profile) {
                overrides.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
        (base, overrides)
    }
}

/// Commit behavior.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CommitConfig {
    /// Sign commits (`git commit -S`), honoring the user's git signing config.
    pub gpg_sign: bool,
    /// After an amend, reword, or extend, rebase any branches stacked on the
    /// rewritten commit onto its new tip (jj/Sapling style). On by default; set
    /// to `false` to keep amend a purely local operation.
    pub auto_restack: bool,
}

impl Default for CommitConfig {
    fn default() -> Self {
        Self {
            gpg_sign: false,
            auto_restack: true,
        }
    }
}

/// Theme selection: a named built-in palette plus per-role color overrides.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
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
#[serde(default)]
pub struct UiConfig {
    /// How many commits the log view walks by default.
    pub log_limit: usize,
    /// Show the side preview pane in the status and log views. Off gives a
    /// single full-width column; `--no-preview` also forces this off.
    pub preview: bool,
    /// Keep the operation console (push/pull/fetch/commit output) open after a
    /// successful run until a key is pressed, so all output stays readable. When
    /// false (default) a successful run closes the console automatically; a
    /// failed run always stays open regardless.
    pub console_hold: bool,
    /// Glyph palette: `ascii`, `unicode`, or `nerd`. Unset falls back to
    /// `nerd_fonts` for backward compatibility.
    pub glyphs: Option<GlyphMode>,
    /// Deprecated: `true` is equivalent to `glyphs = "nerd"`.
    pub nerd_fonts: bool,
    /// Capture mouse events (wheel scroll, click-to-focus, double-click). On by
    /// default (like lazygit) so the wheel scrolls rgit rather than the host
    /// terminal/editor. Set false to keep the terminal's native text selection.
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
            preview: true,
            console_hold: false,
            glyphs: None,
            nerd_fonts: false,
            mouse: true,
            side_by_side: false,
            transparent: Transparency::Off,
        }
    }
}

impl Config {
    /// Build the config from the merged settings. The string lists every
    /// settings warning and a table that does not fit, one per line; a table
    /// that does not fit gives the defaults.
    pub fn from_settings(settings: &Settings) -> (Config, Option<String>) {
        let mut problems: Vec<String> = settings.warnings().iter().map(|w| w.to_string()).collect();
        let config = match toml::Value::Table(settings.table().clone()).try_into::<Config>() {
            Ok(config) => config,
            Err(error) => {
                problems.push(format!("config: {error}"));
                Config::default()
            }
        };
        let message = (!problems.is_empty()).then(|| problems.join("\n"));
        (config, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_profile_keys_apply_to_the_active_profile() {
        let cfg: Config = toml::from_str(
            r##"
            profile = "vim"
            [keys.emacs]
            set-mark = "M-Spc"
            [keys.vim]
            copy = "Y"
            "##,
        )
        .unwrap();
        let (base, keys) = cfg.resolved_keymap();
        assert_eq!(base, "vim");
        // Only the vim overrides apply while vim is active.
        assert_eq!(keys.get("copy").map(String::as_str), Some("Y"));
        assert!(!keys.contains_key("set-mark"));
    }

    #[test]
    fn a_custom_profile_inherits_its_base_then_overrides() {
        let cfg: Config = toml::from_str(
            r##"
            profile = "mine"
            [profiles.mine]
            extends = "emacs"
            [keys.emacs]
            set-mark = "M-Spc"
            stage = "s"
            [keys.mine]
            stage = "S"
            "##,
        )
        .unwrap();
        let (base, keys) = cfg.resolved_keymap();
        // "emacs" is the magit built-in under another name; keymap::init treats
        // both the same.
        assert_eq!(base, "emacs");
        // Inherited from the emacs base...
        assert_eq!(keys.get("set-mark").map(String::as_str), Some("M-Spc"));
        // ...and the derived profile wins where they overlap.
        assert_eq!(keys.get("stage").map(String::as_str), Some("S"));
    }

    #[test]
    fn no_profile_defaults_to_magit_with_no_overrides() {
        let (base, keys) = Config::default().resolved_keymap();
        assert_eq!(base, "magit");
        assert!(keys.is_empty());
    }

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
        // Auto-restack is on unless explicitly disabled.
        assert!(cfg.commit.auto_restack);
    }

    #[test]
    fn auto_restack_can_be_disabled() {
        let cfg: Config = toml::from_str("[commit]\nauto_restack = false\n").unwrap();
        assert!(!cfg.commit.auto_restack);
        assert!(!cfg.commit.gpg_sign);
    }
}

#[cfg(test)]
mod layered_tests {
    use super::*;

    fn settings(global: &str, private: Option<&str>) -> (tempfile::TempDir, Settings) {
        let dir = tempfile::TempDir::new().unwrap();
        let write = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let files = rgit_config::Files {
            global: Some(write("global.toml", global)),
            shared: None,
            private: private.map(|text| write("private.toml", text)),
        };
        let settings = Settings::load_with(files, &|_| None);
        (dir, settings)
    }

    #[test]
    fn layered_private_file_beats_global() {
        let (_dir, settings) = settings("[ui]\nlog_limit = 100\n", Some("[ui]\nlog_limit = 50\n"));
        let (config, error) = Config::from_settings(&settings);
        assert_eq!(config.ui.log_limit, 50);
        assert!(error.is_none());
    }

    #[test]
    fn layered_unknown_key_warns_and_keeps_others() {
        let (_dir, settings) = settings("[ui]\nnope = 1\nlog_limit = 7\n", None);
        let (config, error) = Config::from_settings(&settings);
        assert_eq!(config.ui.log_limit, 7);
        assert!(error.expect("warning").contains("unknown key ui.nope"));
    }
}
