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
    /// Where a stopped rebase is, when one is in progress.
    pub rebase: Option<RebaseProgress>,
}

/// Where a stopped rebase is, from git's `rebase-merge` state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebaseProgress {
    /// The branch being rebased, if any.
    pub branch: Option<String>,
    /// The abbreviated commit it is rebased onto.
    pub onto: String,
    /// Todo lines already done; the last is where it stopped.
    pub done: Vec<String>,
    /// Todo lines still to do.
    pub todo: Vec<String>,
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
    /// Update the files that differ from the target, refusing to overwrite
    /// local changes to them, and keep other local changes (git's `--keep`).
    Keep,
    /// Reset the index and the files that differ from the target, keeping
    /// unstaged changes to other files (git's `--merge`).
    Merge,
}

/// How a branch switch treats local changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CheckoutMode {
    /// Keep local changes, refusing to overwrite them.
    #[default]
    Safe,
    /// Throw local changes away (git's `-f`).
    Force,
    /// Carry local changes over with a three-way merge, leaving conflicts
    /// marked (git's `-m`); `diff3` adds the base to the markers.
    Merge { diff3: bool },
}

/// How [`crate::GitBackend::remove_paths`] removes paths (git's `rm` flags).
#[derive(Debug, Clone, Copy, Default)]
pub struct RmOptions {
    /// Remove from the index only, keeping the working-tree files.
    pub cached: bool,
    /// Remove folders recursively.
    pub recursive: bool,
    /// Skip git's up-to-date checks.
    pub force: bool,
    /// Report what would be removed without removing it.
    pub dry_run: bool,
    /// Succeed even when a pathspec matches nothing.
    pub ignore_unmatch: bool,
}

/// How [`crate::GitBackend::commit_with`] builds a commit beyond its message.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    /// Replace HEAD, keeping its parents and author.
    pub amend: bool,
    /// Skip the pre-commit and commit-msg hooks.
    pub no_verify: bool,
    /// Commit even when the tree does not change.
    pub allow_empty: bool,
    /// Append a `Signed-off-by` trailer for the committer.
    pub signoff: bool,
    /// The author as `Name <email>`, or a pattern naming an existing author,
    /// instead of the committer.
    pub author: Option<String>,
    /// Take the author (name, email and date) from this commit (git's `-C`).
    pub author_from: Option<String>,
    /// Make the committer the author, dated now, even with `amend` or
    /// `author_from`.
    pub reset_author: bool,
    /// The author date, as unix seconds and a UTC offset in minutes.
    pub date: Option<(i64, i32)>,
    /// Commit only these paths, as they are in the working tree, on top of
    /// HEAD; other staged changes stay staged (git's `commit <paths>`).
    pub paths: Vec<String>,
}

/// One ref change of an `update-ref` transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefUpdate {
    pub name: String,
    /// The new value; `None` (or all zeros) deletes the ref, unless `verify`.
    pub new: Option<String>,
    /// The value the ref must hold first; all zeros: it must not exist.
    pub old: Option<String>,
    /// Only check `old`, change nothing.
    pub verify: bool,
    /// Change this ref itself even if it is symbolic (`option no-deref`).
    pub no_deref: bool,
    /// A `symref-*` command: `new_target` makes the ref symbolic, and
    /// `old_target` is the target it must have first.
    pub symref: bool,
    pub new_target: Option<String>,
    pub old_target: Option<String>,
}

/// Options for a cherry-pick or revert of one or more commits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickOptions {
    /// Revert the commits instead of cherry-picking them.
    pub revert: bool,
    /// Apply to the index and working tree without committing (git's `-n`).
    pub no_commit: bool,
    /// Append `(cherry picked from commit ...)` to the message (git's `-x`).
    pub record_origin: bool,
    /// For a merge commit, the 1-based parent to diff against (git's `-m`).
    pub mainline: Option<u32>,
    /// `ours` or `theirs`: the side taken on conflicting hunks (git's `-X`).
    pub strategy_option: Option<String>,
    /// Open the editor on each message (git's `-e`).
    pub edit: bool,
    /// Append a `Signed-off-by` trailer for the committer (git's `-s`).
    pub signoff: bool,
    /// Keep commits that were empty to begin with (git's `--allow-empty`).
    pub allow_empty: bool,
    /// What to do with a commit whose change is already in HEAD (git's `--empty`).
    pub empty: EmptyCommit,
    /// Fast-forward over a commit whose parent is HEAD (git's `--ff`).
    pub ff: bool,
    /// Name a reverted commit as `abbrev (subject, date)` (git's `--reference`).
    pub reference: bool,
    /// The merge strategy (git's `-s`); `ours` keeps HEAD's tree.
    pub strategy: Option<String>,
    /// How to clean up each message: strip, whitespace, verbatim, scissors or
    /// default (git's `--cleanup`).
    pub cleanup: Option<String>,
}

/// What a cherry-pick does with a commit whose change is already in HEAD.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EmptyCommit {
    /// Stop so it can be committed or skipped by hand.
    #[default]
    Stop,
    /// Leave it out.
    Drop,
    /// Commit it with no change.
    Keep,
}

/// Options for a merge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeOptions {
    /// Always create a merge commit (git's `--no-ff`).
    pub no_ff: bool,
    /// Refuse unless the merge can fast-forward (git's `--ff-only`).
    pub ff_only: bool,
    /// Stage the merged changes without a merge commit or MERGE_HEAD (git's `--squash`).
    pub squash: bool,
    /// Merge but stop before committing (git's `--no-commit`).
    pub no_commit: bool,
    /// The merge commit message (git's `-m`).
    pub message: Option<String>,
    /// `ours` or `theirs`: the side taken on conflicting hunks (git's `-X`).
    pub strategy_option: Option<String>,
    /// The merge strategy (git's `-s`); `ours` keeps HEAD's tree.
    pub strategy: Option<String>,
    /// Merge histories with no common ancestor (git's `--allow-unrelated-histories`).
    pub allow_unrelated: bool,
    /// Add up to this many merged commit subjects to the message (git's `--log`).
    pub log: Option<usize>,
    /// Open the editor on the message (git's `-e`).
    pub edit: bool,
    /// Skip the pre-merge-commit and commit-msg hooks (git's `--no-verify`).
    pub no_verify: bool,
    /// Append a `Signed-off-by` trailer for the committer (git's `--signoff`).
    pub signoff: bool,
    /// Report a diffstat of what the merge brought in (git's `--stat`).
    pub stat: bool,
    /// Stash local changes first and reapply them after (git's `--autostash`);
    /// `None` follows `merge.autoStash`.
    pub autostash: Option<bool>,
    /// The branch the default message says it merges into (git's `--into-name`).
    pub into_name: Option<String>,
    /// How to clean up the message: strip, whitespace, verbatim, scissors or
    /// default (git's `--cleanup`).
    pub cleanup: Option<String>,
}

/// `rebase` options, as git's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebaseOptions {
    /// Replay onto this base instead of the upstream (git's `--onto`).
    pub onto: Option<String>,
    /// Open the todo editor on the terminal (git's `-i`).
    pub interactive: bool,
    /// Rebase every commit down to the root (git's `--root`).
    pub root: bool,
    /// Move `fixup!`/`squash!` commits next to their targets (git's `--autosquash`).
    pub autosquash: bool,
    /// Shell commands to run after each commit (git's `--exec`).
    pub exec: Vec<String>,
    /// Move branches that point into the rebased commits too (git's `--update-refs`).
    pub update_refs: bool,
    /// `ours` or `theirs`: the side taken on conflicting hunks (git's `-X`).
    pub strategy_option: Option<String>,
    /// Check out this branch first (git's `<upstream> <branch>`).
    pub branch: Option<String>,
    /// Do not autosquash, overriding rebase.autoSquash (git's `--no-autosquash`).
    pub no_autosquash: bool,
    /// Replay every commit, even ones that could be kept (git's `-f`).
    pub force: bool,
    /// Narrow the upstream with its reflog (git's `--[no-]fork-point`); the
    /// default is on only when no upstream is given.
    pub fork_point: Option<bool>,
    /// Replay onto the merge base of the upstream and the branch (git's `--keep-base`).
    pub keep_base: bool,
    /// Give each commit its author date as the committer date.
    pub committer_date_is_author_date: bool,
    /// Give each commit the current time as its author date.
    pub reset_author_date: bool,
    /// Recreate merges (git's `-r`); `true` rebases cousins too.
    pub rebase_merges: Option<bool>,
    /// `drop`, `keep` or `stop`: a commit that becomes empty (git's `--empty`).
    pub empty: Option<String>,
    /// Replay commits already upstream too (git's `--reapply-cherry-picks`).
    pub reapply_cherry_picks: bool,
    /// Add a Signed-off-by trailer to each commit.
    pub signoff: bool,
    /// Stash local changes first and restore them after.
    pub autostash: bool,
    /// Skip the pre-rebase hook.
    pub no_verify: bool,
    /// Print nothing on success.
    pub quiet: bool,
    /// Show a diffstat of what changed upstream.
    pub verbose: bool,
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
    Ignored,
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
            b'!' => Self::Ignored,
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
            Self::Ignored => Some("ignored"),
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
            Self::Ignored => "!",
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
    /// The full HEAD commit id, `None` when unborn.
    pub oid: Option<String>,
    /// Why it is locked, when a reason was given.
    pub lock_reason: Option<String>,
    /// Why `worktree prune` would remove it, as git words it.
    pub prunable: Option<String>,
}

/// `git worktree add`'s flags beyond the path and commit-ish.
#[derive(Debug, Clone, Default)]
pub struct WorktreeAddArgs {
    /// Create this branch at the commit-ish (git's -b), or with `reset`
    /// create or reset it (-B).
    pub new_branch: Option<String>,
    pub reset: bool,
    /// Check out a detached HEAD.
    pub detach: bool,
    /// Start an unborn branch with an empty index.
    pub orphan: bool,
    /// Check out a branch even when another worktree has it.
    pub force: bool,
    /// Leave the working tree and index empty.
    pub no_checkout: bool,
    /// Lock the new worktree, with an optional reason.
    pub lock: bool,
    pub reason: Option<String>,
    /// Set a new branch's upstream to the start point (`Some(true)`), never
    /// (`Some(false)`), or when it is a remote-tracking branch (`None`).
    pub track: Option<bool>,
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
    /// Start the walk from these revisions instead of HEAD: a branch/tag/sha,
    /// `^rev` to exclude, or a range `A..B` / `A...B`.
    pub revs: Vec<String>,
    /// Keep only commits at or after this committer time (unix seconds).
    pub since: Option<i64>,
    /// Keep only commits at or before this committer time (unix seconds).
    pub until: Option<i64>,
    /// Keep only commits that touched one of these pathspecs.
    pub paths: Vec<String>,
    /// Keep only commits whose message matches one of these regexes.
    pub grep: Vec<String>,
    /// Match `grep` case-insensitively.
    pub grep_ignore_case: bool,
    /// Follow only the first parent of merges.
    pub first_parent: bool,
    /// `Some(true)` keeps only merges, `Some(false)` drops them.
    pub merges: Option<bool>,
    /// Oldest first (applied after `limit`, as in git).
    pub reverse: bool,
    /// Follow the single path in `paths` across renames.
    pub follow: bool,
    /// With `paths`, name each commit's parents as its nearest shown
    /// ancestors, as git does for `--graph`.
    pub rewrite_parents: bool,
    /// Keep only commits whose committer name/email contains this (case-insensitive).
    pub committer: Option<String>,
    /// Keep only commits that change how often this string occurs (git's -S).
    pub occurrences: Option<String>,
    /// Keep only commits adding or removing a line that matches this regex (git's -G).
    pub changes_matching: Option<String>,
    /// Ref globs to walk too (`refs/heads/*` for `--branches`).
    pub globs: Vec<String>,
    /// The order commits come in.
    pub order: LogOrder,
    /// Keep only the commits of one side of a symmetric range: `Some(true)`
    /// the left (`--left-only`), `Some(false)` the right.
    pub side: Option<bool>,
    /// Between the sides of a symmetric range, drop commits with the same
    /// patch as one on the other side (`Some(true)`, `--cherry-pick`) or mark
    /// them `=` (`Some(false)`, `--cherry-mark`).
    pub cherry: Option<bool>,
    /// After the commits, the excluded commits they have as parents, marked `-`.
    pub boundary: bool,
    /// Only commits that descend from a range's excluded ends.
    pub ancestry_path: bool,
    /// Only commits that refs point at (and those touching `paths`).
    pub simplify_by_decoration: bool,
    /// Walk every parent of a merge under path limits, not just a same one.
    pub full_history: bool,
    /// With `full_history`, leave out merges the shown history does not need.
    pub simplify_merges: bool,
    /// Show every walked commit under path limits, not only those changing them.
    pub sparse: bool,
    /// Show the given commits only, by date (`Some(true)`) or as given.
    pub no_walk: Option<bool>,
    /// Walk `HEAD...MERGE_HEAD` (or the other in-progress pick's head),
    /// limited to the conflicted paths.
    pub merge: bool,
    /// Require every `grep` pattern to match, not any.
    pub all_match: bool,
    /// Keep commits whose message does not match `grep`.
    pub invert_grep: bool,
    /// Record which starting ref reached each commit (`--source`).
    pub source: bool,
}

/// The order a log walk lists commits in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogOrder {
    /// Newest committer date first, as commits are reached.
    #[default]
    Walk,
    /// No parent before all its children, a merge's branches kept together.
    Topo,
    /// No parent before all its children, else by committer date.
    Date,
    /// No parent before all its children, else by author date.
    AuthorDate,
}

/// Which two sides [`crate::GitBackend::diff`] compares, and how.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffSpec {
    /// Old side: a revision, or a range `A..B` / `A...B` when `to` is None.
    /// None with `to` set diffs `to` against the empty tree.
    pub from: Option<String>,
    /// New side revision; None means the working tree (the index when `cached`).
    pub to: Option<String>,
    /// Compare against the index instead of the working tree.
    pub cached: bool,
    /// Limit to these pathspecs.
    pub paths: Vec<String>,
    /// Lines of context (git's -U); None keeps git's 3.
    pub context: Option<u32>,
    /// Ignore all whitespace (git's -w).
    pub ignore_all_space: bool,
    /// Ignore changes in amount of whitespace (git's -b).
    pub ignore_space_change: bool,
    /// Swap the two sides (git's -R).
    pub reverse: bool,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            limit: 200,
            offset: 0,
            all: false,
            author: None,
            revs: Vec::new(),
            since: None,
            until: None,
            paths: Vec::new(),
            grep: Vec::new(),
            grep_ignore_case: false,
            first_parent: false,
            merges: None,
            reverse: false,
            follow: false,
            rewrite_parents: false,
            committer: None,
            occurrences: None,
            changes_matching: None,
            globs: Vec::new(),
            order: LogOrder::Walk,
            side: None,
            cherry: None,
            boundary: false,
            ancestry_path: false,
            simplify_by_decoration: false,
            full_history: false,
            simplify_merges: false,
            sparse: false,
            no_walk: None,
            merge: false,
            all_match: false,
            invert_grep: false,
            source: false,
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
    /// git's mark: `<`/`>` for a symmetric range's side, `-` for a boundary
    /// commit, `=` for one whose patch the other side has too.
    pub mark: Option<char>,
    /// The starting ref that reached this commit, with `source` set.
    pub source: Option<String>,
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
    /// The full commit id; empty for a line not committed yet.
    pub id: String,
    pub email: String,
    /// The line's number, and the file's path, in that commit.
    pub orig_line: usize,
    pub orig_path: String,
    /// The commit is where blame stopped: a root commit, as git marks with `^`.
    pub boundary: bool,
    /// The line's number in the blamed file.
    pub final_line: usize,
    /// Passed through an ignored revision (`--ignore-rev`), or left with
    /// one because no line before it matched.
    pub ignored: bool,
    pub unblamable: bool,
    /// The commit and path the suspect was compared with, if any.
    pub previous: Option<(String, String)>,
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

/// `git fetch` flags beyond the remote and refspecs.
#[derive(Debug, Clone, Default)]
pub struct FetchArgs {
    /// Fetch every remote (`--all`).
    pub all: bool,
    /// Drop remote-tracking refs that no longer exist upstream (`--prune`).
    pub prune: bool,
    /// Fetch every tag as well (`--tags`).
    pub tags: bool,
    /// Limit history to this many commits (`--depth`); 0 fetches all of it.
    pub depth: i32,
    /// Report what would change without updating any ref (`--dry-run`).
    pub dry_run: bool,
    /// Fetch the whole history of a shallow repository (`--unshallow`).
    pub unshallow: bool,
    /// Deepen a shallow history by this many commits (`--deepen`).
    pub deepen: i32,
    /// Limit history to commits after this date (`--shallow-since`).
    pub shallow_since: Option<String>,
    /// Also fetch every tag and, with `prune`, drop local tags gone from the
    /// remote (`--prune-tags`).
    pub prune_tags: bool,
    /// Follow no tags (`--no-tags`).
    pub no_tags: bool,
    /// Update refs even when not a fast-forward (`--force`).
    pub force: bool,
    /// Map command-line refspecs through these instead of the configured
    /// ones (`--refmap`); an empty list updates no remote-tracking ref.
    pub refmap: Option<Vec<String>>,
    /// Record the fetched branch as the current branch's upstream (`--set-upstream`).
    pub set_upstream: bool,
    /// Fetch these remotes and groups instead of one (`--multiple`, `remote update`).
    pub remotes: Vec<String>,
    /// Fetch this many remotes at once (`--jobs`); 0 follows `fetch.parallel`.
    pub jobs: usize,
    /// Add to FETCH_HEAD instead of replacing it (`--append`).
    pub append: bool,
}

/// `git pull` flags beyond the remote and branch.
#[derive(Debug, Clone, Default)]
pub struct PullArgs {
    /// Rebase (`Some(true)`) or merge (`Some(false)`); `None` follows `pull.rebase`.
    pub rebase: Option<bool>,
    /// Refuse anything but a fast-forward (`--ff-only`).
    pub ff_only: bool,
    /// Always make a merge commit (`--no-ff`).
    pub no_ff: bool,
    /// Stage the result without committing or recording a merge (`--squash`).
    pub squash: bool,
    /// Merge but stop before committing (`--no-commit`).
    pub no_commit: bool,
    /// Stash local changes around the pull (`--autostash`); `None` follows
    /// `rebase.autoStash` / `merge.autoStash`.
    pub autostash: Option<bool>,
    /// `ours` or `theirs` for conflicting hunks (`-X`).
    pub strategy_option: Option<String>,
    /// Fetch every remote first (`--all`).
    pub all: bool,
    /// Limit fetched history to this many commits (`--depth`).
    pub depth: i32,
}

/// `git push` flags beyond the remote and refspecs.
#[derive(Debug, Clone, Default)]
pub struct PushArgs {
    /// Overwrite remote refs unconditionally (`--force`).
    pub force: bool,
    /// Overwrite only if the remote still matches our remote-tracking ref.
    pub force_with_lease: bool,
    /// Record each pushed branch's upstream (`-u`).
    pub set_upstream: bool,
    /// Push every local branch (`--all`).
    pub all: bool,
    /// Push every local tag too (`--tags`).
    pub tags: bool,
    /// Report what would be pushed without sending it (`--dry-run`).
    pub dry_run: bool,
    /// Also push annotated tags that point into the pushed history (`--follow-tags`).
    pub follow_tags: bool,
    /// Push nothing unless every ref can be updated (`--atomic`).
    pub atomic: bool,
    /// Delete remote refs that no local ref maps to (`--prune`).
    pub prune: bool,
    /// Make the remote's refs match every local ref (`--mirror`).
    pub mirror: bool,
    /// Strings passed to the server's hooks (`-o`/`--push-option`).
    pub push_options: Vec<String>,
    /// Skip the pre-push hook (`--no-verify`).
    pub no_verify: bool,
    /// Report in git's machine-readable format (`--porcelain`).
    pub porcelain: bool,
    /// Also report refs that are already up to date (`--verbose`).
    pub verbose: bool,
    /// `check`, `on-demand`, `only` or `no` for the submodule commits pushed
    /// (`--recurse-submodules`); `None` follows `push.recurseSubmodules`.
    pub recurse_submodules: Option<String>,
}

/// A submodule as `git submodule status` reports it.
#[derive(Debug, Clone)]
pub struct SubmoduleInfo {
    pub name: String,
    /// Path from the top of the superproject (nested ones include their parents).
    pub path: String,
    pub url: Option<String>,
    /// The branch `.gitmodules` names, if any.
    pub branch: Option<String>,
    /// The commit the superproject records.
    pub recorded: Option<String>,
    /// The commit checked out in the submodule, when it is.
    pub checked_out: Option<String>,
    /// git's status column: ` ` in sync, `-` not initialized, `+` another commit
    /// checked out, `U` merge conflicts.
    pub state: char,
    /// The checked-out commit as `git describe` names it.
    pub describe: Option<String>,
}

/// A `git submodule` subcommand that changes something.
#[derive(Debug, Clone)]
pub enum SubmoduleOp {
    Add {
        url: String,
        path: Option<String>,
        branch: Option<String>,
        name: Option<String>,
    },
    Init {
        paths: Vec<String>,
    },
    Update {
        paths: Vec<String>,
        init: bool,
        recursive: bool,
        remote: bool,
        /// Submodules cloned and checked out at once (`--jobs`); `None`
        /// follows `submodule.fetchJobs`.
        jobs: Option<usize>,
    },
    Sync {
        paths: Vec<String>,
        recursive: bool,
    },
    Deinit {
        paths: Vec<String>,
        force: bool,
        all: bool,
    },
    SetUrl {
        path: String,
        url: String,
    },
    /// `branch: None` goes back to the remote's default branch.
    SetBranch {
        path: String,
        branch: Option<String>,
    },
    AbsorbGitDirs {
        paths: Vec<String>,
    },
    /// git's own `submodule summary` arguments.
    Summary {
        args: Vec<String>,
    },
}

/// `git init` options beyond the path.
#[derive(Debug, Clone, Default)]
pub struct InitArgs {
    /// Name of the first branch (`-b`).
    pub initial_branch: Option<String>,
    /// Make a bare repository (`--bare`).
    pub bare: bool,
    /// Copy hooks and other files from this folder (`--template`).
    pub template: Option<String>,
    /// `group`, `all`/`world`/`everybody`, `umask`/`false` or an octal mode (`--shared`).
    pub shared: Option<String>,
    /// Put the repository here and link it from the working tree (`--separate-git-dir`).
    pub separate_git_dir: Option<String>,
}

/// `git clone` options beyond the URL and target directory.
#[derive(Debug, Clone, Default)]
pub struct CloneArgs {
    /// Check out this branch instead of the remote's HEAD (`-b`).
    pub branch: Option<String>,
    /// Shallow-clone this many commits (`--depth`); 0 clones all history.
    pub depth: i32,
    /// Make a bare repository (`--bare`).
    pub bare: bool,
    /// Name the remote this instead of `origin` (`-o`).
    pub origin: Option<String>,
    /// Clone submodules too (`--recurse-submodules`).
    pub recurse_submodules: bool,
    /// Fetch only the checked-out branch (`--single-branch`).
    pub single_branch: bool,
    /// Leave the working tree empty (`--no-checkout`).
    pub no_checkout: bool,
    /// A bare copy of every ref, kept as a mirror (`--mirror`).
    pub mirror: bool,
    /// Follow no tags (`--no-tags`).
    pub no_tags: bool,
    /// `key=value` settings written to the new repository (`-c`).
    pub config: Vec<String>,
    /// Fetch every branch even when shallow (`--no-single-branch`).
    pub no_single_branch: bool,
    /// Borrow objects from these local repositories (`--reference`).
    pub reference: Vec<String>,
    /// Borrow from these when they are repositories (`--reference-if-able`).
    pub reference_if_able: Vec<String>,
    /// Copy the borrowed objects and drop the link (`--dissociate`).
    pub dissociate: bool,
    /// Borrow the source's objects instead of copying them (`--shared`).
    pub shared: bool,
    /// A partial clone (`--filter`), which only git can make.
    pub filter: Option<String>,
    /// Check out only the top-level files (`--sparse`).
    pub sparse: bool,
    /// Copy hooks and other files from this folder (`--template`).
    pub template: Option<String>,
    /// Keep the history after this date (`--shallow-since`).
    pub shallow_since: Option<String>,
    /// Put the repository here and link it from the working tree (`--separate-git-dir`).
    pub separate_git_dir: Option<String>,
}
