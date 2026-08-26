use crate::error::GitError;
use crate::model::RepoStatus;

/// The read/mutation surface the TUI drives, kept abstract so a gix-native or
/// libgit2 implementation can back each operation independently.
///
/// Send + Sync so a handle can be shared with blocking refresh tasks.
pub trait GitBackend: Send + Sync {
    /// The repository's working directory, e.g. to watch for changes.
    fn workdir(&self) -> &std::path::Path;

    /// A full status snapshot for the status buffer: head, changed paths,
    /// per-file diffs, and recent commits.
    fn status(&self) -> Result<RepoStatus, GitError>;

    /// Stage every change in the working tree.
    fn stage_all(&self) -> Result<(), GitError>;

    /// Unstage everything back to HEAD.
    fn unstage_all(&self) -> Result<(), GitError>;

    /// Stage a whole path (add to the index, or record its deletion).
    fn stage_file(&self, path: &str) -> Result<(), GitError>;

    /// Unstage a whole path (reset its index entry to HEAD).
    fn unstage_file(&self, path: &str) -> Result<(), GitError>;

    /// Stage one hunk of a path, identified by its new-side start line.
    fn stage_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError>;

    /// Unstage one hunk of a path, identified by its new-side start line.
    fn unstage_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError>;

    /// Stage only the given line indices (within the hunk at `new_start`).
    fn stage_lines(&self, path: &str, new_start: u32, lines: &[usize]) -> Result<(), GitError>;

    /// Unstage only the given line indices (within the hunk at `new_start`).
    fn unstage_lines(&self, path: &str, new_start: u32, lines: &[usize]) -> Result<(), GitError>;

    /// Discard a path's unstaged changes: restore a tracked file from the index,
    /// or delete an untracked file.
    fn discard_file(&self, path: &str) -> Result<(), GitError>;

    /// Discard one unstaged hunk from the working tree.
    fn discard_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError>;

    /// Discard the given unstaged line indices from the working tree.
    fn discard_lines(&self, path: &str, new_start: u32, lines: &[usize]) -> Result<(), GitError>;

    /// Path of the commit-message file the editor should open.
    fn commit_msg_path(&self) -> std::path::PathBuf;

    /// Create a commit from the index with `message`, running the pre-commit and
    /// commit-msg hooks. Errors if a hook fails or nothing is staged.
    fn commit(&self, message: &str) -> Result<(), GitError>;

    /// Replace HEAD with a new commit from the index, keeping HEAD's parents.
    fn amend(&self, message: &str) -> Result<(), GitError>;

    /// The directory git resolves hooks from (`core.hooksPath` or
    /// `<git-dir>/hooks`), so the caller can run and stream them itself.
    fn hooks_dir(&self) -> std::path::PathBuf;

    /// Commit from the index without running any hooks (the caller ran them).
    fn commit_no_verify(&self, message: &str) -> Result<(), GitError>;

    /// Amend HEAD without running any hooks (the caller ran them).
    fn amend_no_verify(&self, message: &str) -> Result<(), GitError>;

    /// Git-style output lines describing the current HEAD commit, e.g.
    /// `["[main abc1234] subject", " 2 files changed, 5 insertions(+)"]`, to
    /// echo into the operation console and the log after committing.
    fn commit_report(&self) -> Vec<String>;

    /// The full message of the HEAD commit, to prefill an amend editor.
    fn head_message(&self) -> Option<String>;

    /// The staged changes as a unified patch, e.g. to summarize for a message.
    fn staged_patch(&self) -> Result<String, GitError>;

    /// Amend HEAD with the current index but keep its existing message (no
    /// editor) - magit's "extend".
    fn commit_extend(&self) -> Result<(), GitError>;

    /// Fetch the current branch's remote, updating remote-tracking refs.
    /// `report` receives git-style progress lines and transfer counts.
    fn fetch(&self, report: &dyn Fn(crate::OpProgress)) -> Result<(), GitError>;

    /// Fetch and fast-forward the current branch; errors if not fast-forwardable.
    fn pull(&self, report: &dyn Fn(crate::OpProgress)) -> Result<(), GitError>;

    /// Push the current branch to its upstream remote. `force` overwrites
    /// unconditionally; `force_with_lease` overwrites only if the remote still
    /// matches our remote-tracking ref; `set_upstream` records tracking.
    fn push(
        &self,
        force: bool,
        force_with_lease: bool,
        set_upstream: bool,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// Local branch names, sorted.
    fn local_branches(&self) -> Result<Vec<String>, GitError>;

    /// Check out an existing local branch.
    fn checkout_branch(&self, name: &str) -> Result<(), GitError>;

    /// Create a branch at HEAD and check it out.
    fn create_branch(&self, name: &str) -> Result<(), GitError>;

    /// Stash the working tree and index (including untracked files).
    fn stash_push(&self) -> Result<(), GitError>;

    /// Stash the working tree and index under a descriptive `message`.
    fn stash_push_message(&self, message: &str) -> Result<(), GitError>;

    /// Pop the stash at `index` (apply it and drop it).
    fn stash_pop(&self, index: usize) -> Result<(), GitError>;

    /// Apply the stash at `index` without dropping it.
    fn stash_apply(&self, index: usize) -> Result<(), GitError>;

    /// Drop the stash at `index` without applying it.
    fn stash_drop(&self, index: usize) -> Result<(), GitError>;

    /// The commit log, newest first, filtered and bounded by `opts`.
    fn log(&self, opts: &crate::LogOptions) -> Result<Vec<crate::LogEntry>, GitError>;

    /// A commit's metadata and its diff against its first parent, resolved from
    /// a revision (e.g. a short id).
    fn commit_details(&self, rev: &str) -> Result<crate::CommitDetails, GitError>;

    /// The diff between two revisions' trees (`from` as the old side).
    fn diff_refs(&self, from: &str, to: &str) -> Result<Vec<crate::FileDiff>, GitError>;

    /// Blame a working-tree file: each line with the commit that last touched it.
    fn blame(&self, path: &str) -> Result<Vec<crate::BlameLine>, GitError>;

    /// All references: local branches, remote branches, and tags.
    fn refs(&self) -> Result<Vec<crate::RefEntry>, GitError>;

    /// Check out a revision (tag or remote branch) as a detached HEAD.
    fn checkout_detached(&self, rev: &str) -> Result<(), GitError>;

    /// Rebase the current branch onto `rev` in-process; aborts on conflict.
    /// `report` receives git-style progress lines (`Applying: ...`).
    fn rebase_onto(&self, rev: &str, report: &dyn Fn(crate::OpProgress)) -> Result<(), GitError>;

    /// Commits reachable from HEAD but not from `base`, oldest first (rebase-todo
    /// order): each `(short_id, subject)`.
    fn commits_between(&self, base: &str) -> Result<Vec<(String, String)>, GitError>;

    /// Abort an in-progress rebase.
    fn rebase_abort(&self) -> Result<(), GitError>;

    /// Continue an in-progress rebase after resolving conflicts. Shells out to
    /// `git` (libgit2 cannot drive a CLI-started rebase).
    fn rebase_continue(&self) -> Result<(), GitError>;

    /// Skip the current commit of an in-progress rebase. Shells out to `git`.
    fn rebase_skip(&self) -> Result<(), GitError>;

    /// Start an interactive rebase (`git rebase -i [onto]`), inheriting the
    /// terminal so git can open the todo editor. Needs a TTY.
    fn rebase_interactive(&self, onto: Option<&str>) -> Result<(), GitError>;

    /// Undo the last HEAD move (`reset --keep HEAD@{1}`), preserving uncommitted
    /// work. Shells out to `git` (libgit2 has no `--keep`).
    fn undo(&self) -> Result<(), GitError>;

    /// Run a `git bisect` subcommand (`start <bad> <good>`, `good`, `bad`,
    /// `reset`, ...). Shells out to `git` (libgit2 has no bisect).
    fn bisect(&self, args: &[String]) -> Result<String, GitError>;

    /// Remove every untracked file and directory (`git clean -fd`).
    fn clean(&self) -> Result<(), GitError>;

    /// Remove a tracked path from the index and the working tree (`git rm`).
    fn remove_path(&self, path: &str) -> Result<(), GitError>;

    /// Rename/move a tracked path (`git mv`).
    fn move_path(&self, from: &str, to: &str) -> Result<(), GitError>;

    /// Describe a revision relative to the nearest tag (`git describe`).
    fn describe(&self, rev: &str) -> Result<String, GitError>;

    /// Run any `git` subcommand and return its stdout - the escape hatch for
    /// operations rgit does not model natively (submodule, notes, grep, gc, ...).
    fn git(&self, args: &[String]) -> Result<String, GitError>;

    /// Reset HEAD (and, per mode, the index and worktree) to `rev`.
    fn reset(&self, rev: &str, mode: crate::ResetMode) -> Result<(), GitError>;

    /// Cherry-pick `rev` onto HEAD, committing when there are no conflicts.
    fn cherry_pick(&self, rev: &str) -> Result<(), GitError>;

    /// Revert `rev` on HEAD, committing when there are no conflicts.
    fn revert(&self, rev: &str) -> Result<(), GitError>;

    /// Merge `rev` into the current branch (fast-forward or a merge commit).
    /// `report` receives git-style progress lines (`Updating`, `Fast-forward`,
    /// `CONFLICT ...`).
    fn merge(&self, rev: &str, report: &dyn Fn(crate::OpProgress)) -> Result<(), GitError>;

    /// Resolve a conflicted path by taking our side (`ours`) or theirs, writing
    /// that version to the worktree and staging it.
    fn resolve_conflict(&self, path: &str, ours: bool) -> Result<(), GitError>;

    /// Create a tag at HEAD (annotated when `message` is non-empty).
    fn create_tag(&self, name: &str, message: &str) -> Result<(), GitError>;

    /// Delete a tag.
    fn delete_tag(&self, name: &str) -> Result<(), GitError>;

    /// Configured remotes with their fetch URLs, sorted by name.
    fn remotes(&self) -> Result<Vec<crate::Remote>, GitError>;

    /// Add a remote named `name` pointing at `url`.
    fn add_remote(&self, name: &str, url: &str) -> Result<(), GitError>;

    /// Remove the remote named `name`.
    fn remove_remote(&self, name: &str) -> Result<(), GitError>;

    /// Linked worktrees, sorted by name.
    fn worktrees(&self) -> Result<Vec<crate::Worktree>, GitError>;

    /// Create a linked worktree named `name` at `path` (on a new branch `name`).
    fn add_worktree(&self, name: &str, path: &str) -> Result<(), GitError>;

    /// Remove the linked worktree named `name`.
    fn remove_worktree(&self, name: &str) -> Result<(), GitError>;

    /// Delete a local branch.
    fn delete_branch(&self, name: &str) -> Result<(), GitError>;

    /// Rename a local branch.
    fn rename_branch(&self, old: &str, new: &str) -> Result<(), GitError>;
}
