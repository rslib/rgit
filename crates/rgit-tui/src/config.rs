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
    pub forge: ForgeConfig,
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

/// Forge account/profile selection. Credentials remain in the OS keychain.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ForgeConfig {
    pub default_profile: Option<String>,
    pub profiles: HashMap<String, ForgeProfile>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ForgeProfile {
    pub provider: Option<String>,
    pub account: Option<String>,
    pub host: Option<String>,
}

impl ForgeConfig {
    pub fn context(&self) -> (Option<String>, Option<String>) {
        let profile = self
            .default_profile
            .as_ref()
            .and_then(|name| self.profiles.get(name));
        (
            profile.and_then(|profile| profile.account.clone()),
            profile.and_then(|profile| profile.host.clone()),
        )
    }
}

/// One custom profile: a base to inherit from, then its own `[keys.<name>]`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("[theme]\nnope = 1\n").is_err());
    }
}
