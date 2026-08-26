/// A semantic style role. The TUI maps each role to concrete theme colors, so
/// the model stays independent of any rendering backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    SectionHeader,
    FieldLabel,
    Branch,
    Hash,
    Dim,
    Added,
    Modified,
    Deleted,
    Untracked,
    /// The changed run within a modified line, added and removed side.
    WordAdded,
    WordDeleted,
    /// A concrete syntax-highlight color, carried through from the highlighter.
    Rgb(u8, u8, u8),
    /// Faint washes used as a line background on added / removed diff lines.
    AddedBg,
    DeletedBg,
}

/// A run of text carrying a foreground style role and an optional background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    pub bg: Option<Style>,
}

impl Span {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            bg: None,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(text, Style::Plain)
    }

    /// Set a background wash on the span (e.g. added / removed diff lines).
    pub fn with_bg(mut self, bg: Style) -> Self {
        self.bg = Some(bg);
        self
    }
}
