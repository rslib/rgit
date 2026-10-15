/// The type a key's value must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Str,
    /// A string that must be one of these values.
    Enum(&'static [&'static str]),
}

impl Kind {
    pub(crate) fn expected(self) -> String {
        match self {
            Kind::Bool => "bool".to_owned(),
            Kind::Int => "integer".to_owned(),
            Kind::Str => "string".to_owned(),
            Kind::Enum(values) => format!("one of {}", values.join(", ")),
        }
    }

    pub(crate) fn accepts(self, value: &toml::Value) -> bool {
        match (self, value) {
            (Kind::Bool, toml::Value::Boolean(_)) => true,
            (Kind::Int, toml::Value::Integer(_)) => true,
            (Kind::Str, toml::Value::String(_)) => true,
            (Kind::Enum(values), toml::Value::String(s)) => values.contains(&s.as_str()),
            _ => false,
        }
    }
}

/// One known setting. A `*` segment in `pattern` matches any one segment.
#[derive(Debug, Clone, Copy)]
pub struct KeyDef {
    pub pattern: &'static str,
    pub kind: Kind,
    pub default: Option<&'static str>,
    pub help: &'static str,
    /// Whether the committed shared repo file may set the key.
    pub shared: bool,
}

impl KeyDef {
    pub(crate) fn matches(&self, key: &str) -> bool {
        let mut pattern = self.pattern.split('.');
        let mut key = key.split('.');
        loop {
            match (pattern.next(), key.next()) {
                (None, None) => return true,
                (Some(p), Some(k)) if p == "*" || p == k => {}
                _ => return false,
            }
        }
    }

    pub(crate) fn is_concrete(&self) -> bool {
        !self.pattern.split('.').any(|s| s == "*")
    }
}

const fn key(
    pattern: &'static str,
    kind: Kind,
    default: Option<&'static str>,
    help: &'static str,
) -> KeyDef {
    KeyDef {
        pattern,
        kind,
        default,
        help,
        shared: true,
    }
}

const fn unshared(def: KeyDef) -> KeyDef {
    KeyDef {
        shared: false,
        ..def
    }
}

pub(crate) const OUTPUT_FORMATS: &[&str] = &["human", "json", "toon"];
const GLYPHS: &[&str] = &["ascii", "unicode", "nerd"];
const TRANSPARENCY: &[&str] = &["off", "body", "full"];

/// Every key rgit reads. A key outside this list is reported as unknown.
pub static KEYS: &[KeyDef] = &[
    key(
        "output.format",
        Kind::Enum(OUTPUT_FORMATS),
        None,
        "Output format for commands: human, json or toon",
    ),
    key(
        "output.compact",
        Kind::Bool,
        Some("false"),
        "Print compact output",
    ),
    key(
        "commit.change_id",
        Kind::Bool,
        Some("false"),
        "Add a change id to new commits",
    ),
    key(
        "workflow.preset",
        Kind::Str,
        None,
        "Workflow preset for this repository",
    ),
    key("theme.name", Kind::Str, None, "Built-in palette name"),
    key("theme.accent", Kind::Str, None, "Accent color, #rrggbb"),
    key(
        "theme.section_header",
        Kind::Str,
        None,
        "Section header color, #rrggbb",
    ),
    key(
        "theme.field_label",
        Kind::Str,
        None,
        "Field label color, #rrggbb",
    ),
    key("theme.branch", Kind::Str, None, "Branch color, #rrggbb"),
    key("theme.hash", Kind::Str, None, "Commit id color, #rrggbb"),
    key(
        "theme.dim",
        Kind::Str,
        None,
        "Secondary text color, #rrggbb",
    ),
    key("theme.added", Kind::Str, None, "Added line color, #rrggbb"),
    key(
        "theme.modified",
        Kind::Str,
        None,
        "Modified file color, #rrggbb",
    ),
    key(
        "theme.deleted",
        Kind::Str,
        None,
        "Deleted line color, #rrggbb",
    ),
    key(
        "theme.untracked",
        Kind::Str,
        None,
        "Untracked file color, #rrggbb",
    ),
    key(
        "theme.cursor_bg",
        Kind::Str,
        None,
        "Cursor row background, #rrggbb",
    ),
    key(
        "theme.select_bg",
        Kind::Str,
        None,
        "Selection background, #rrggbb",
    ),
    key(
        "ui.log_limit",
        Kind::Int,
        Some("200"),
        "Commits the log view walks by default",
    ),
    key(
        "ui.preview",
        Kind::Bool,
        Some("true"),
        "Show the side preview pane",
    ),
    key(
        "ui.console_hold",
        Kind::Bool,
        Some("false"),
        "Keep the console open after a successful run",
    ),
    key(
        "ui.glyphs",
        Kind::Enum(GLYPHS),
        None,
        "Glyph palette: ascii, unicode or nerd",
    ),
    key(
        "ui.nerd_fonts",
        Kind::Bool,
        Some("false"),
        "Deprecated: true means glyphs = nerd",
    ),
    key("ui.mouse", Kind::Bool, Some("true"), "Capture mouse events"),
    key(
        "ui.side_by_side",
        Kind::Bool,
        Some("false"),
        "Render read-only diffs side by side",
    ),
    key(
        "ui.transparent",
        Kind::Enum(TRANSPARENCY),
        Some("off"),
        "Regions that show the terminal background: off, body or full",
    ),
    key(
        "commit.gpg_sign",
        Kind::Bool,
        Some("false"),
        "Sign commits with git -S",
    ),
    key(
        "commit.auto_restack",
        Kind::Bool,
        Some("true"),
        "Rebase stacked branches after a rewrite",
    ),
    key("profile", Kind::Str, None, "Active keybinding profile"),
    key(
        "profiles.*.extends",
        Kind::Str,
        None,
        "Profile this custom profile is based on",
    ),
    key(
        "keys.*.*",
        Kind::Str,
        None,
        "Key spec for an action in a profile",
    ),
    unshared(key(
        "forge.default_profile",
        Kind::Str,
        None,
        "Forge profile used when none is named",
    )),
    unshared(key(
        "forge.profiles.*.provider",
        Kind::Str,
        None,
        "Forge provider of a profile",
    )),
    unshared(key(
        "forge.profiles.*.account",
        Kind::Str,
        None,
        "Forge account of a profile",
    )),
    unshared(key(
        "forge.profiles.*.host",
        Kind::Str,
        None,
        "Forge host of a profile",
    )),
    unshared(key(
        "ai.*.command",
        Kind::Str,
        None,
        "Shell command for this AI tool; gets the prompt on stdin",
    )),
    key(
        "ai.*.timeout_secs",
        Kind::Int,
        Some("120"),
        "Seconds to wait for this AI tool",
    ),
    key(
        "ai.*.draft_prompt",
        Kind::Str,
        None,
        "Draft prompt text for this AI tool",
    ),
    key(
        "ai.*.draft_prompt_file",
        Kind::Str,
        None,
        "File with the draft prompt for this AI tool",
    ),
    key(
        "ai.*.review_prompt",
        Kind::Str,
        None,
        "Review prompt text for this AI tool",
    ),
    key(
        "ai.*.review_prompt_file",
        Kind::Str,
        None,
        "File with the review prompt for this AI tool",
    ),
    key(
        "commit.ai.draft",
        Kind::Str,
        None,
        "AI tool that drafts commit messages",
    ),
    key(
        "commit.ai.review",
        Kind::Str,
        None,
        "AI tool that reviews commit messages",
    ),
    key(
        "commit.ai.draft_prompt",
        Kind::Str,
        None,
        "Draft prompt text for commit messages",
    ),
    key(
        "commit.ai.draft_prompt_file",
        Kind::Str,
        None,
        "File with the draft prompt for commit messages",
    ),
    key(
        "commit.ai.review_prompt",
        Kind::Str,
        None,
        "Review prompt text for commit messages",
    ),
    key(
        "commit.ai.review_prompt_file",
        Kind::Str,
        None,
        "File with the review prompt for commit messages",
    ),
    key(
        "commit.ai.rules",
        Kind::Str,
        None,
        "Commit message rules text",
    ),
    key(
        "commit.ai.rules_file",
        Kind::Str,
        None,
        "File with the commit message rules",
    ),
    key(
        "commit.ai.max_diff_bytes",
        Kind::Int,
        Some("100000"),
        "Largest diff in bytes sent to an AI tool",
    ),
];

/// Settings that take text inline or from a file: (inline suffix, file suffix).
/// Both forms are one setting; the later layer wins.
pub(crate) const PAIRS: &[(&str, &str)] = &[
    ("draft_prompt", "draft_prompt_file"),
    ("review_prompt", "review_prompt_file"),
    ("rules", "rules_file"),
];

/// The other form of a paired key, and whether `key` is the inline form.
pub(crate) fn partner(key: &str) -> Option<(String, bool)> {
    let (head, last) = key.rsplit_once('.')?;
    PAIRS.iter().find_map(|(inline, file)| {
        if last == *inline {
            Some((format!("{head}.{file}"), true))
        } else if last == *file {
            Some((format!("{head}.{inline}"), false))
        } else {
            None
        }
    })
}

pub(crate) fn lookup(key: &str) -> Option<&'static KeyDef> {
    KEYS.iter().find(|def| def.matches(key))
}
