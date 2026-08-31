use crate::style::Span;

/// A stable identity for a section, used to preserve fold state across the
/// rebuild that follows every refresh. Ids are derived from position in the
/// git model (e.g. `unstaged/src/main.rs`), so the same section keeps its id.
pub type SectionId = String;

/// What a content node represents. Drives spacing and fold behavior in the
/// view, and is the extension point for new content kinds in later phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    HeadField,
    Section,
    File,
    Hunk,
    DiffLine,
    Stash,
    Commit,
    Info,
}

/// What a staging key (`s`/`u`) acts on when the cursor is at a node. `staged`
/// distinguishes a node in the Staged section (which `u` unstages) from one in
/// the Unstaged/Untracked sections (which `s` stages).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    File {
        path: String,
        staged: bool,
    },
    /// A hunk header or one of its diff lines. Shared by every diff view; the
    /// context fields say how it can be acted on.
    Hunk {
        path: String,
        /// New-side start line of the owning hunk (for hunk-level staging/fold).
        new_start: u32,
        /// This row's new-side file line (equals `new_start` on the hunk header),
        /// precomputed so the editor can open at it in any view without a lookup.
        line: u32,
        /// The working-tree side for staging: `Some(staged)` for the status view,
        /// `None` for a read-only diff (a commit or a diff between revs), which is
        /// foldable and editor-openable but not stageable.
        staged: Option<bool>,
    },
    Stash {
        index: usize,
    },
    /// A commit that can be opened (by short id) to view its diff.
    Commit {
        id: String,
    },
    /// A code-search hit: a file path and the 1-based line to open/preview at.
    CodeHit {
        path: String,
        line: usize,
    },
    /// A reference that can be checked out.
    Ref {
        name: String,
        kind: RefTarget,
    },
}

/// Which kind of reference a [`Target::Ref`] points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefTarget {
    Local,
    Remote,
    Tag,
}

/// One node of a buffer's content tree: pure data with no interactive state.
/// Fold state and the cursor live in the view's `Buffer`, keyed by [`id`].
#[derive(Debug, Clone)]
pub struct Section {
    pub id: SectionId,
    pub kind: NodeKind,
    pub spans: Vec<Span>,
    pub children: Vec<Section>,
    /// What `s`/`u` act on here; leaf lines inherit their hunk's target.
    pub target: Option<Target>,
    /// Whether this section is collapsed until the user expands it. The view
    /// treats its fold set as overrides of this default.
    pub default_folded: bool,
}

impl Section {
    pub fn leaf(id: impl Into<SectionId>, kind: NodeKind, spans: Vec<Span>) -> Self {
        Self {
            id: id.into(),
            kind,
            spans,
            children: Vec::new(),
            target: None,
            default_folded: false,
        }
    }

    pub fn branch(
        id: impl Into<SectionId>,
        kind: NodeKind,
        spans: Vec<Span>,
        children: Vec<Section>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            spans,
            children,
            target: None,
            default_folded: false,
        }
    }

    pub fn with_target(mut self, target: Target) -> Self {
        self.target = Some(target);
        self
    }

    /// Start collapsed until the user expands it.
    pub fn folded_by_default(mut self) -> Self {
        self.default_folded = true;
        self
    }

    pub fn is_foldable(&self) -> bool {
        !self.children.is_empty()
    }
}
