use crate::diff::FileDiff;

/// A snapshot of everything the status buffer renders, built from one refresh.
#[derive(Debug, Clone, Default)]
pub struct RepoStatus {
    pub head: Head,
    pub entries: Vec<StatusEntry>,
    /// Worktree-vs-index diffs, keyed by path via [`FileDiff::path`].
    pub unstaged: Vec<FileDiff>,
    /// Index-vs-HEAD diffs.
    pub staged: Vec<FileDiff>,
    pub stashes: Vec<Stash>,
    pub recent: Vec<Commit>,
    /// An in-progress sequencer operation (merge, rebase, ...), if any.
    pub state: RepoState,
}

/// A sequencer operation in progress, mirroring git's on-disk state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepoState {
    #[default]
    Clean,
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

impl RepoState {
    /// A short label for the status header, or `None` when clean.
    pub fn label(self) -> Option<&'static str> {
        match self {
            RepoState::Clean => None,
            RepoState::Merge => Some("merging"),
            RepoState::Rebase => Some("rebasing"),
            RepoState::CherryPick => Some("cherry-picking"),
            RepoState::Revert => Some("reverting"),
            RepoState::Bisect => Some("bisecting"),
        }
    }
}

/// How far a reset moves the index and worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

impl RepoStatus {
    pub fn unstaged_diff(&self, path: &str) -> Option<&FileDiff> {
        self.unstaged.iter().find(|d| d.path == path)
    }

    pub fn staged_diff(&self, path: &str) -> Option<&FileDiff> {
        self.staged.iter().find(|d| d.path == path)
    }
}

/// The state of `HEAD` and its upstream tracking.
#[derive(Debug, Clone, Default)]
pub struct Head {
    /// Branch name, or `None` when `HEAD` is detached.
    pub branch: Option<String>,
    pub detached: bool,
    /// Short commit id, or `None` on an unborn branch (no commits yet).
    pub oid: Option<String>,
    pub summary: Option<String>,
    /// Relative age of the tip commit ("3 hours ago"), for the header.
    pub when: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
}

/// One path reported by `git status`, carrying its index and worktree states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    /// Original path for a rename or copy.
    pub orig_path: Option<String>,
    /// State of the index relative to `HEAD` (the staged change).
    pub index: StatusCode,
    /// State of the working tree relative to the index (the unstaged change).
    pub worktree: StatusCode,
}

impl StatusEntry {
    pub fn is_untracked(&self) -> bool {
        self.worktree == StatusCode::Untracked
    }

    pub fn is_staged(&self) -> bool {
        !matches!(self.index, StatusCode::Unmodified | StatusCode::Untracked)
    }

    pub fn is_unstaged(&self) -> bool {
        !matches!(
            self.worktree,
            StatusCode::Unmodified | StatusCode::Untracked
        )
    }
}

/// A single-file change class, matching the porcelain v2 status letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
}

impl StatusCode {
    pub fn from_porcelain(c: u8) -> Self {
        match c {
            b'M' => Self::Modified,
            b'A' => Self::Added,
            b'D' => Self::Deleted,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'T' => Self::TypeChanged,
            b'U' => Self::Unmerged,
            b'?' => Self::Untracked,
            _ => Self::Unmodified,
        }
    }

    /// A magit-style label for the change, or `None` when there is no change.
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Unmodified => None,
            Self::Modified => Some("modified"),
            Self::Added => Some("new file"),
            Self::Deleted => Some("deleted"),
            Self::Renamed => Some("renamed"),
            Self::Copied => Some("copied"),
            Self::TypeChanged => Some("typechange"),
            Self::Unmerged => Some("unmerged"),
            Self::Untracked => Some("untracked"),
        }
    }

    /// A single-letter code for a compact file row.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Unmodified => " ",
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Unmerged => "U",
            Self::Untracked => "?",
        }
    }
}

impl Head {
    /// Display name for the branch line: branch name, or a short detached id.
    pub fn describe(&self) -> String {
        if let Some(branch) = &self.branch {
            branch.clone()
        } else if self.detached {
            match &self.oid {
                Some(oid) => format!("detached@{oid}"),
                None => "detached".to_owned(),
            }
        } else {
            "(no branch)".to_owned()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub short_id: String,
    pub summary: String,
    /// Relative age ("3 hours ago").
    pub when: String,
}

/// An entry in the stash stack, `index` 0 being the most recent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    pub index: usize,
    pub message: String,
}

/// A configured remote and its fetch URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

/// A linked worktree of the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub name: String,
    pub path: String,
}

/// Filters for the commit log view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogOptions {
    /// Maximum number of commits to walk.
    pub limit: usize,
    /// Include every ref's history, not just HEAD's.
    pub all: bool,
    /// Keep only commits whose author name/email contains this (case-insensitive).
    pub author: Option<String>,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            limit: 200,
            all: false,
            author: None,
        }
    }
}

/// One entry in the operation log (undo stack), newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpLogEntry {
    /// The operation that produced the state after this snapshot, e.g. `commit`.
    pub label: String,
    /// Where HEAD was: a branch name, or `detached <sha>`.
    pub head: String,
    /// A short relative age like `2h`.
    pub when: String,
    /// Short id of the snapshot commit.
    pub short_id: String,
}

/// One commit in the smartlog: your local/draft commits plus the trunk tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartlogEntry {
    pub short_id: String,
    pub summary: String,
    pub author: String,
    /// A short relative age like `2h`.
    pub when: String,
    /// Branch names pointing at this commit (local, and the trunk marker).
    pub refs: Vec<String>,
    /// Whether HEAD is here.
    pub is_head: bool,
    /// Whether this is the trunk tip (the base your work diverges from).
    pub is_trunk: bool,
    /// The commit's stable change id, if it carries one. Unlike the oid, this
    /// survives amend and rebase, so it identifies the logical change.
    pub change_id: Option<String>,
}

/// The result of a restack. A conflict on one branch no longer aborts the whole
/// operation: the branch is left untouched at its old base for the user to
/// resolve, and the rest of the stack still moves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestackOutcome {
    /// Human lines like `feature -> main` for each branch that was rebased.
    pub restacked: Vec<String>,
    /// Branches whose rebase hit a conflict; skipped and left at their old base.
    pub conflicted: Vec<String>,
}

impl RestackOutcome {
    pub fn is_empty(&self) -> bool {
        self.restacked.is_empty() && self.conflicted.is_empty()
    }
}

/// A commit in the log view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub short_id: String,
    pub summary: String,
    pub author: String,
    /// A short relative age like `2h` or `3d`.
    pub when: String,
    /// Full commit oid, for graph lane bookkeeping.
    pub oid: String,
    /// Full parent oids, first-parent first.
    pub parents: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

/// A reference in the refs view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    pub name: String,
    pub kind: RefKind,
    pub is_head: bool,
}

/// One line of a file annotated with the commit that last touched it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub short_id: String,
    pub author: String,
    pub line: String,
}

/// A commit's metadata and its diff against its first parent.
#[derive(Debug, Clone)]
pub struct CommitDetails {
    pub id: String,
    pub author: String,
    pub email: String,
    pub when: String,
    pub message: String,
    pub files: Vec<FileDiff>,
}

/// Progress reported by a network operation, for the operation console and its
/// progress bar. `Line` is a git-style output line; `Transfer` is object counts.
#[derive(Debug, Clone)]
pub enum OpProgress {
    Line(String),
    Transfer { received: usize, total: usize },
}
