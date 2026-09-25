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
    /// Ahead/behind of the current branch against each remote that has it, as
    /// `(remote_ref, ahead, behind)` (e.g. `("origin/main", 2, 0)`). For the
    /// REMOTE overview across all remotes, not just the tracking upstream.
    pub remotes: Vec<(String, usize, usize)>,
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
    /// Refs pointing at this commit (git --decorate order: local, remote, tag).
    pub refs: Vec<CommitRef>,
    /// Not reachable from any remote-tracking branch, i.e. local-only.
    pub unpushed: bool,
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

/// A worktree of the repository (the main one, or a linked worktree), with
/// enough state to inspect it at a glance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub name: String,
    pub path: String,
    /// The checked-out branch, or `None` when detached.
    pub branch: Option<String>,
    /// Abbreviated HEAD commit id, or `None` for an unborn branch.
    pub head: Option<String>,
    /// Whether the worktree has uncommitted changes (tracked, staged, or new).
    pub dirty: bool,
    /// Whether the worktree is locked (`git worktree lock`).
    pub locked: bool,
    /// The primary worktree (the repository's own working directory).
    pub is_main: bool,
}

/// Filters for the commit log view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogOptions {
    /// Maximum number of commits to walk.
    pub limit: usize,
    /// Skip this many commits before collecting, for pagination.
    pub offset: usize,
    /// Include every ref's history, not just HEAD's.
    pub all: bool,
    /// Keep only commits whose author name/email contains this (case-insensitive).
    pub author: Option<String>,
    /// Start the walk from this revision instead of HEAD (a branch/tag/sha).
    pub rev: Option<String>,
    /// Keep only commits at or after this author time (unix seconds).
    pub since: Option<i64>,
    /// Keep only commits at or before this author time (unix seconds).
    pub until: Option<i64>,
    /// Keep only commits that touched this path (a pathspec prefix).
    pub path: Option<String>,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            limit: 200,
            offset: 0,
            all: false,
            author: None,
            rev: None,
            since: None,
            until: None,
            path: None,
        }
    }
}

/// One entry in a tree listing (the tree/file browser).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// The entry's base name.
    pub name: String,
    /// Full path from the repo root, slash-separated.
    pub path: String,
    /// A subdirectory (a tree) rather than a file (a blob).
    pub is_dir: bool,
    /// Blob size in bytes; 0 for directories.
    pub size: u64,
}

/// The newest tag in a repo, for the sidebar's release card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInfo {
    pub name: String,
    /// Relative age of the tagged commit.
    pub when: String,
    /// The tag's message, or the tagged commit's summary for a lightweight tag.
    pub message: String,
}

/// The most recent commit that touched a tree entry, for the "latest commit per
/// file" strip in the tree view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastCommit {
    pub short_id: String,
    pub summary: String,
    /// Relative age like `2h` or `3d`.
    pub when: String,
}

/// One matching line from a code search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepMatch {
    pub path: String,
    /// 1-based line number.
    pub line: usize,
    /// The matching line, trimmed and length-capped.
    pub text: String,
}

/// A scoped code search request.
#[derive(Debug, Clone, Default)]
pub struct GrepQuery {
    /// Text to match; literal unless `regex` is set.
    pub pattern: String,
    /// Treat `pattern` as a regular expression instead of a literal string.
    pub regex: bool,
    /// Keep only files whose repo-relative path contains this substring
    /// (case-insensitive).
    pub path: Option<String>,
    /// Keep only files with one of these extensions (lowercase, no leading
    /// dot); empty means any extension.
    pub exts: Vec<String>,
}

/// A file's contents at a revision, for the blob/file view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    /// Full path from the repo root.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Binary (non-text) content; `text` is None when set.
    pub is_binary: bool,
    /// UTF-8 text, lossily decoded; None for binary blobs.
    pub text: Option<String>,
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

/// A hunk of a tracked file owned by a lane, identified by a content anchor (a
/// hash of the hunk's lines) so it survives the line-number shifts that come
/// from editing elsewhere in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkRef {
    pub path: String,
    pub anchor: String,
}

/// One lane: a named bucket of uncommitted changes that commits to its own
/// branch. A path is owned either whole (in `paths`) or split into hunks (in
/// `hunks`), never both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    pub name: String,
    /// The real branch this lane commits to, created lazily on first commit.
    pub branch: String,
    /// Whole-file ownership.
    pub paths: Vec<String>,
    /// Hunk-level ownership of tracked files split across lanes.
    pub hunks: Vec<HunkRef>,
    /// Commits this lane has made above the fork point, newest first, as
    /// `(short_id, summary)`. Derived at read time (not persisted), for display.
    pub commits: Vec<(String, String)>,
    /// The lane this one is stacked on, if any: its commits build on that lane's
    /// branch instead of the shared fork point.
    pub parent: Option<String>,
}

/// The lanes overlay: a fork point and the lanes assigned over it. The default
/// lane (owning everything unassigned) is always `lanes[0]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanesState {
    /// The commit every lane forks from.
    pub base: String,
    pub lanes: Vec<Lane>,
}

/// How active a file has been across recent history: how many commits touched
/// it in the walked window and the author time (unix seconds) of the newest such
/// commit. Feeds churn- and recency-weighted search ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileActivity {
    pub commits: u32,
    pub last_epoch: i64,
}

/// Collapse a [`FileActivity`] map into a per-path history weight in `[0, 1]`,
/// blending recency (newer edits weigh more, ~30-day half-life) and churn (more
/// commits weigh more, saturating). Callers fold this into search ranking so hot
/// files surface above cold ones at equal semantic relevance.
pub fn activity_weights(
    activity: &std::collections::HashMap<String, FileActivity>,
) -> std::collections::HashMap<String, f32> {
    const HALF_LIFE_SECS: f32 = 30.0 * 24.0 * 60.0 * 60.0;
    const CHURN_MIDPOINT: f32 = 4.0;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    activity
        .iter()
        .map(|(path, a)| {
            let age = (now - a.last_epoch).max(0) as f32;
            let recency = (-age / HALF_LIFE_SECS * std::f32::consts::LN_2).exp();
            let churn = a.commits as f32 / (a.commits as f32 + CHURN_MIDPOINT);
            (path.clone(), 0.6 * recency + 0.4 * churn)
        })
        .collect()
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
    /// Refs pointing at this commit (git --decorate order: local, remote, tag).
    pub refs: Vec<CommitRef>,
    /// Not reachable from any remote-tracking branch, i.e. local-only.
    pub unpushed: bool,
}

/// A ref decorating a log entry: a branch, upstream, or tag that points at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRef {
    pub name: String,
    pub kind: RefKind,
    /// True when this is the local branch HEAD is currently on.
    pub head: bool,
}

/// A display token for a commit's refs. Refs that share a branch name collapse
/// into one [`Deco::Group`] so `main origin/main upstream/main` renders as
/// `{local,origin,upstream}/main` instead of repeating the name three times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deco {
    Local(String),
    Remote {
        remote: String,
        branch: String,
    },
    Tag(String),
    Group {
        branch: String,
        /// The local branch of this name is present.
        local: bool,
        /// Remotes that carry a branch of this name.
        remotes: Vec<String>,
    },
}

/// Collapse a commit's refs for display: refs sharing a branch name fold into a
/// single `{local,<remote>,...}/branch` token; unique branches and tags render on
/// their own. Order follows the input (HEAD's local branch first), tags last.
pub fn group_decorations(refs: &[CommitRef]) -> Vec<Deco> {
    struct Agg {
        local: bool,
        remotes: Vec<String>,
    }
    // Order-preserving suffix -> aggregate; first occurrence fixes position.
    fn slot(branches: &mut Vec<(String, Agg)>, key: &str) -> usize {
        if let Some(i) = branches.iter().position(|(k, _)| k == key) {
            i
        } else {
            branches.push((
                key.to_owned(),
                Agg {
                    local: false,
                    remotes: Vec::new(),
                },
            ));
            branches.len() - 1
        }
    }
    let mut branches: Vec<(String, Agg)> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    for r in refs {
        match r.kind {
            RefKind::Tag => tags.push(r.name.clone()),
            RefKind::Local => {
                let i = slot(&mut branches, &r.name);
                branches[i].1.local = true;
            }
            RefKind::Remote => {
                let (remote, branch) = match r.name.split_once('/') {
                    Some((rm, b)) => (rm.to_owned(), b.to_owned()),
                    None => (String::new(), r.name.clone()),
                };
                let i = slot(&mut branches, &branch);
                branches[i].1.remotes.push(remote);
            }
        }
    }
    let mut out = Vec::new();
    for (branch, agg) in branches {
        let count = usize::from(agg.local) + agg.remotes.len();
        if count >= 2 {
            out.push(Deco::Group {
                branch,
                local: agg.local,
                remotes: agg.remotes,
            });
        } else if agg.local {
            out.push(Deco::Local(branch));
        } else {
            out.push(Deco::Remote {
                remote: agg.remotes.into_iter().next().unwrap_or_default(),
                branch,
            });
        }
    }
    out.extend(tags.into_iter().map(Deco::Tag));
    out
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
    /// Abbreviated commit id, for compact display.
    pub id: String,
    /// Full 40-hex commit id, for the detail header and copy-paste.
    pub full_id: String,
    pub author: String,
    pub email: String,
    pub when: String,
    /// Absolute dates in `git log` default form, e.g. `Thu Sep 24 23:24:06 2026 -0500`.
    pub author_date: String,
    pub committer: String,
    pub committer_email: String,
    pub commit_date: String,
    /// `(short id, subject)` per parent, first parent first.
    pub parents: Vec<(String, String)>,
    /// Branches and tags pointing at this commit.
    pub refs: Vec<String>,
    /// Local branches whose tip this commit contains.
    pub merged: Vec<String>,
    /// Local branches that contain this commit.
    pub contained: Vec<String>,
    /// The nearest tag reachable from this commit and the distance to it.
    pub follows: Option<(String, usize)>,
    pub message: String,
    pub files: Vec<FileDiff>,
}

/// One changed file in a commit, with line counts but no hunks - cheap to list
/// even for a commit that touches many files. The hunks load on demand.
#[derive(Debug, Clone)]
pub struct CommitFile {
    pub path: String,
    /// The source path when the file was renamed or copied in this commit;
    /// `None` otherwise. `path` is always the new side.
    pub old_path: Option<String>,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
}

/// A commit's metadata and the list of files it changed, without any hunk bodies.
/// The full diff for a single file is fetched separately, so a large commit does
/// not render every diff up front.
#[derive(Debug, Clone)]
pub struct CommitOverview {
    pub id: String,
    pub full_id: String,
    pub author: String,
    pub email: String,
    pub when: String,
    pub message: String,
    pub files: Vec<CommitFile>,
}

/// Progress reported by a network operation, for the operation console and its
/// progress bar. `Line` is a git-style output line; `Transfer` is object counts.
#[derive(Debug, Clone)]
pub enum OpProgress {
    Line(String),
    Transfer { received: usize, total: usize },
}
