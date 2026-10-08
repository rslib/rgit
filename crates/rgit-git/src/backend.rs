use crate::error::GitError;
use crate::model::{RepoStatus, StatusEntry};

/// A remote's refs as (name, id), and the branch its HEAD names.
pub type RemoteHeads = (Vec<(String, String)>, Option<String>);

/// The read/mutation surface the TUI drives, kept abstract so a gix-native or
/// libgit2 implementation can back each operation independently.
///
/// The refs a fetch-pack fetched (id, name) and the names the remote lacks.
pub type FetchedRefs = (Vec<(String, String)>, Vec<String>);

/// Send + Sync so a handle can be shared with blocking refresh tasks.
pub trait GitBackend: Send + Sync {
    /// The repository's working directory, e.g. to watch for changes.
    fn workdir(&self) -> &std::path::Path;

    /// A status snapshot for the status buffer: head, changed paths, and
    /// recent commits. Per-file diffs come back unloaded (`FileDiff::loaded`
    /// is false) so the read stays fast on a tree with many changes.
    fn status(&self) -> Result<RepoStatus, GitError>;

    /// `status` with every file diff materialized, for the background fill
    /// that follows the first paint.
    fn status_full(&self) -> Result<RepoStatus, GitError> {
        self.status()
    }

    /// The second of the index file's last write: git's racy-clean mark.
    fn index_second(&self) -> Option<i64> {
        None
    }

    /// After a write that started when the index was last written at `since`,
    /// mark entries staged in or after that second whose file changed since
    /// (size 0), as git does, so git re-reads them rather than trusting stat.
    fn smudge_racy(&self, _since: Option<i64>) -> Result<(), GitError> {
        Ok(())
    }

    /// `git checkout -m -- <paths>` / `--conflict=<style>`: put back the
    /// conflicts of resolved paths from the index's resolve-undo record,
    /// rewrite each conflicted file with markers in `style` (merge, diff3
    /// or zdiff3; merge.conflictStyle by default), and check out the other
    /// paths from the index. Returns (conflicts recreated, paths updated,
    /// errors).
    fn checkout_merge(
        &self,
        paths: &[String],
        style: Option<&str>,
    ) -> Result<(usize, usize, Vec<String>), GitError>;

    /// Stage the tracked files under `paths` again with the clean filters
    /// and line endings applied afresh (`git add --renormalize`).
    fn renormalize(&self, paths: &[String]) -> Result<(), GitError>;

    /// Set or clear the executable bit of the index entries under `paths`
    /// (`git add --chmod`); returns the paths that are not regular files.
    fn index_chmod(&self, paths: &[String], executable: bool) -> Result<Vec<String>, GitError>;

    /// git's own status output, long, short or porcelain, for `opts`.
    fn status_text(&self, opts: &crate::StatusOpts) -> Result<crate::StatusReport, GitError>;

    /// The plain patch of `git diff-files -p` (no `rev`), or of `git
    /// diff-index [-R] [--cached] <rev> -p`, limited to `paths`, with
    /// `context` lines (default 3): what the `-p` modes ask about.
    fn patch_diff(
        &self,
        rev: Option<&str>,
        cached: bool,
        reverse: bool,
        context: Option<u32>,
        paths: &[String],
    ) -> Result<Vec<u8>, GitError>;

    /// Ignored files and folders in the working tree (`git status --ignored`).
    fn ignored(&self) -> Result<Vec<StatusEntry>, GitError>;

    /// Stage every change in the working tree.
    fn stage_all(&self) -> Result<(), GitError>;

    /// Unstage everything back to HEAD.
    fn unstage_all(&self) -> Result<(), GitError>;

    /// Stage a whole path, folder or glob (add what exists, record deletions).
    fn stage_file(&self, path: &str) -> Result<(), GitError>;

    /// Unstage a whole path, folder or glob (reset its index entries to HEAD).
    fn unstage_file(&self, path: &str) -> Result<(), GitError>;

    /// git's `add`: stage new, changed and deleted files under `paths` (every
    /// path when empty), or with `update` only tracked ones. `force` adds
    /// ignored files too.
    fn add(&self, paths: &[String], update: bool, force: bool) -> Result<(), GitError>;

    /// Restore `paths` from `source` (a revision, or the index when `None`) into
    /// the index (`staged`) and/or the working tree (`worktree`). `overlay`
    /// leaves paths the source lacks alone (checkout's rule); otherwise they are
    /// removed (restore's rule).
    fn restore(
        &self,
        paths: &[String],
        source: Option<&str>,
        staged: bool,
        worktree: bool,
        overlay: bool,
    ) -> Result<(), GitError>;

    /// Stage one hunk of a path, identified by its new-side start line.
    fn stage_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError>;

    /// Unstage one hunk of a path, identified by its new-side start line.
    fn unstage_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError>;

    /// Stage only the given line indices (within the hunk at `new_start`).
    fn stage_lines(&self, path: &str, new_start: u32, lines: &[usize]) -> Result<(), GitError>;

    /// Unstage only the given line indices (within the hunk at `new_start`).
    fn unstage_lines(&self, path: &str, new_start: u32, lines: &[usize]) -> Result<(), GitError>;

    /// Discard a path's unstaged changes: restore tracked files from the index,
    /// or delete an untracked file. Untracked files in a folder stay.
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

    /// Commit (or amend) with the given options; see [`crate::CommitOptions`].
    fn commit_with(&self, message: &str, opts: &crate::CommitOptions) -> Result<(), GitError>;

    /// Change any commit's message (keeping its tree and parents) and replay its
    /// descendants on the current branch, then restack stacked children. The rev
    /// must be an ancestor of HEAD on the first-parent chain.
    fn reword(&self, rev: &str, message: &str) -> Result<(), GitError>;

    /// Undo the last `n` commits, keeping their changes staged in the working
    /// tree (a soft reset to `HEAD~n`). Op-log-safe.
    fn uncommit(&self, n: usize) -> Result<(), GitError>;

    /// Fold a commit into its parent (joining both messages) and replay its
    /// descendants, then restack. A conflict aborts cleanly. The rev must be an
    /// ancestor of HEAD on the first-parent chain and not the root commit.
    fn squash(&self, rev: &str) -> Result<(), GitError>;

    /// Split a commit in two by path: the first commit gets the given paths'
    /// changes, the second gets the rest, then replay descendants. Op-log-safe.
    fn split(&self, rev: &str, paths: &[String]) -> Result<(), GitError>;

    /// Fold every commit after `from` up to HEAD into a single commit on `from`
    /// (HEAD's tree, joined messages). `from` must be an ancestor of HEAD.
    /// Op-log-safe.
    fn squash_range(&self, from: &str) -> Result<(), GitError>;

    /// Move `rev` to just before or after `target` in the current branch's linear
    /// history, replaying the affected commits in the new order. Both must be on
    /// the first-parent chain. A conflict aborts cleanly. Op-log-safe.
    fn reorder(&self, rev: &str, target: &str, before: bool) -> Result<(), GitError>;

    /// Delete local branches fully merged into `base` (tip is an ancestor of
    /// base), except the current branch and `base` itself. Returns the names
    /// deleted. Op-log-safe.
    fn prune_merged(&self, base: &str) -> Result<Vec<String>, GitError>;

    /// Fetch, fast-forward every non-current local branch to its upstream where
    /// safe (a strict fast-forward), then restack the stack onto the updated
    /// bases. Op-log-safe.
    fn sync(&self, report: &dyn Fn(crate::OpProgress)) -> Result<crate::RestackOutcome, GitError>;

    /// Push every branch in the current stack (bottom-up, force-with-lease) and
    /// open or update a pull request per branch (base = its stack parent), via
    /// the forge CLI. Returns a note per branch.
    fn submit_stack(&self, report: &dyn Fn(crate::OpProgress)) -> Result<Vec<String>, GitError>;

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

    /// The message a stopped merge, squash, cherry-pick or revert prepared
    /// (SQUASH_MSG or MERGE_MSG, without comment lines), for a plain `commit`.
    fn prepared_message(&self) -> Option<String>;

    /// The staged changes as a unified patch, e.g. to summarize for a message.
    fn staged_patch(&self) -> Result<String, GitError>;

    /// Amend HEAD with the current index but keep its existing message (no
    /// editor) - magit's "extend".
    fn commit_extend(&self) -> Result<(), GitError>;

    /// Fetch from a remote, updating remote-tracking refs. `remote` names one
    /// (else the branch's upstream remote); `refspecs` fetch just those refs
    /// (else the remote's configured ones). `report` receives git-style
    /// progress lines and transfer counts.
    fn fetch(
        &self,
        remote: Option<&str>,
        refspecs: &[String],
        args: &crate::FetchArgs,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// Fetch and integrate a branch into the current one, like `git pull`.
    /// `remote`/`branch` default to the upstream. `args.rebase` forces a rebase
    /// (`Some(true)`) or a merge (`Some(false)`); `None` follows `pull.rebase`.
    /// `args.ff_only` (or `pull.ff=only`) refuses anything but a fast-forward.
    fn pull(
        &self,
        remote: Option<&str>,
        branch: Option<&str>,
        args: &crate::PullArgs,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// Push the current branch to its upstream remote. `force` overwrites
    /// unconditionally; `force_with_lease` overwrites only if the remote still
    /// matches our remote-tracking ref; `set_upstream` records tracking.
    /// Push the current branch. `remote` picks the target remote by name; `None`
    /// pushes to the branch's configured upstream (the default).
    fn push(
        &self,
        remote: Option<&str>,
        force: bool,
        force_with_lease: bool,
        set_upstream: bool,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// Push `refspecs` (git syntax: `branch`, `src:dst`, `:dst`, `+src:dst`) to
    /// `remote` (else the upstream's remote). With no refspecs it pushes the
    /// current branch, or every branch with `args.all`.
    fn push_to(
        &self,
        remote: Option<&str>,
        refspecs: &[String],
        args: &crate::PushArgs,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// `git fetch-pack`: fetch `refs` (full names, or every ref with `all`)
    /// from the repository at `url` into the object store, updating no ref.
    /// Returns each fetched ref's id and name in the remote's order, and the
    /// names it does not have.
    fn fetch_pack(
        &self,
        url: &str,
        refs: &[String],
        all: bool,
        depth: Option<i32>,
    ) -> Result<FetchedRefs, GitError>;

    /// `git send-pack`: push `refspecs` (with none, the branches both sides
    /// have) to the repository at `url`, with no remote config, hooks or
    /// remote-tracking refs; the report goes to `report`.
    fn send_pack(
        &self,
        url: &str,
        refspecs: &[String],
        args: &crate::PushArgs,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// Push all local tags to `remote` (or the upstream's remote), like
    /// `git push --tags`.
    fn push_tags(
        &self,
        remote: Option<&str>,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// Delete `branch` on `remote` (or the upstream's remote), like
    /// `git push --delete`.
    fn push_delete(
        &self,
        remote: Option<&str>,
        branch: &str,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// The submodules with their `git submodule status` state; nested ones
    /// too when `recursive`.
    fn submodules(&self, recursive: bool) -> Result<Vec<crate::SubmoduleInfo>, GitError>;

    /// Run a `git submodule` subcommand that changes something, reporting
    /// git's lines.
    fn submodule(
        &self,
        op: &crate::SubmoduleOp,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// Local branch names, sorted.
    fn local_branches(&self) -> Result<Vec<String>, GitError>;

    /// Remote-tracking branch names (e.g. `origin/main`), sorted. For `git
    /// branch -r` / `-a`.
    fn remote_branches(&self) -> Result<Vec<String>, GitError>;

    /// Check out an existing local branch.
    fn checkout_branch(&self, name: &str) -> Result<(), GitError>;

    /// Create a branch at HEAD and check it out.
    fn create_branch(&self, name: &str) -> Result<(), GitError>;

    /// Create a branch at `start` and check it out; `force` resets an existing
    /// one. The upstream is set to `start` when it is a remote-tracking branch
    /// or `track` is set.
    fn branch_from(
        &self,
        name: &str,
        start: &str,
        force: bool,
        track: bool,
    ) -> Result<(), GitError>;

    /// The branch (or, if detached, the commit) checked out before the current
    /// one: git's `@{-1}`.
    fn previous_checkout(&self) -> Result<String, GitError>;

    /// Create branch `name` at `start` without checking it out (git's `branch
    /// <name> <start>`), tracking `start` when it is a remote-tracking branch.
    /// `force` moves an existing branch.
    fn create_branch_at(&self, name: &str, start: &str, force: bool) -> Result<(), GitError>;

    /// Set local branch `name`'s upstream (a remote-tracking or local branch),
    /// or unset it with `None` (git's `branch -u` / `--unset-upstream`).
    fn set_upstream(&self, name: &str, upstream: Option<&str>) -> Result<(), GitError>;

    /// Local branch `name`'s upstream and how far the branch is ahead of and
    /// behind it, or `None` when it tracks nothing.
    fn branch_upstream(&self, name: &str) -> Result<Option<(String, usize, usize)>, GitError>;

    /// Stash the working tree and index (including untracked files). Returns the
    /// git-style `Saved working directory and index state WIP on ...` line.
    /// Stash the working tree and index. With `include_untracked` (git's `-u`),
    /// also stash untracked files; without it, leave them (git's default).
    fn stash_push(&self, include_untracked: bool) -> Result<String, GitError>;

    /// Stash the working tree and index under a descriptive `message`. Returns
    /// the git-style `Saved working directory and index state ...` line.
    fn stash_push_message(
        &self,
        message: &str,
        include_untracked: bool,
    ) -> Result<String, GitError>;

    /// Stash like `git stash push`: under `message` when given, untracked files
    /// too with `include_untracked` (ignored ones too with `all`), the index
    /// kept in place with `keep_index`, and only `paths` when any are given.
    /// Returns git's `Saved ...` line.
    fn stash_push_opts(
        &self,
        message: Option<&str>,
        include_untracked: bool,
        all: bool,
        keep_index: bool,
        paths: &[String],
    ) -> Result<String, GitError>;

    /// Stash part of the changes and take it out of the working tree: with
    /// `picked` None, only the staged changes (`git stash push --staged`);
    /// else the picked hunks, a patch against HEAD (`git stash push -p`),
    /// resetting the index of `paths` too unless `keep_index`. Returns
    /// git's `Saved ...` line.
    fn stash_push_part(
        &self,
        message: Option<&str>,
        picked: Option<&[u8]>,
        keep_index: bool,
        paths: &[String],
    ) -> Result<String, GitError>;

    /// A stash commit of the local changes, neither stored nor removed from
    /// the working tree (`git stash create`); None when nothing changed.
    fn stash_create(&self, message: Option<&str>) -> Result<Option<String>, GitError>;

    /// Put a stash commit on top of the stash list (`git stash store`).
    fn stash_store(&self, rev: &str, message: Option<&str>) -> Result<(), GitError>;

    /// Pop the stash at `index` (apply it and drop it).
    fn stash_pop(&self, index: usize) -> Result<(), GitError>;

    /// Apply the stash at `index` without dropping it.
    fn stash_apply(&self, index: usize) -> Result<(), GitError>;

    /// Apply the stash at `index`, restoring its staged changes to the index
    /// too with `restore_index` (git's `--index`), and drop it after with
    /// `drop` (git's `stash pop`).
    fn stash_apply_opts(
        &self,
        index: usize,
        restore_index: bool,
        drop: bool,
    ) -> Result<(), GitError>;

    /// Drop the stash at `index` without applying it.
    fn stash_drop(&self, index: usize) -> Result<(), GitError>;

    /// The commit log, newest first, filtered and bounded by `opts`.
    fn log(&self, opts: &crate::LogOptions) -> Result<Vec<crate::LogEntry>, GitError>;

    /// A smartlog: the commits reachable from local branches but not from the
    /// trunk (your draft work), plus the trunk tip, newest first, annotated with
    /// branch names and HEAD/trunk markers.
    fn smartlog(&self) -> Result<Vec<crate::SmartlogEntry>, GitError>;

    /// A commit's metadata and its diff against its first parent, resolved from
    /// a revision (e.g. a short id).
    fn commit_details(&self, rev: &str) -> Result<crate::CommitDetails, GitError>;

    /// A commit's metadata and changed-file list with line counts but no hunks,
    /// so a commit touching many files lists cheaply.
    fn commit_overview(&self, rev: &str) -> Result<crate::CommitOverview, GitError>;

    /// The full diff for one file in a commit (against its first parent). None if
    /// the file is not part of the commit's diff.
    fn commit_file_diff(&self, rev: &str, path: &str) -> Result<Option<crate::FileDiff>, GitError>;

    /// The diff between two revisions' trees (`from` as the old side).
    fn diff_refs(&self, from: &str, to: &str) -> Result<Vec<crate::FileDiff>, GitError>;

    /// A `git diff`: any two of a revision, the index and the working tree,
    /// limited to pathspecs, with git's context and whitespace options.
    fn diff(&self, spec: &crate::DiffSpec) -> Result<Vec<crate::FileDiff>, GitError>;

    /// The regex `--word-diff` splits a file pair into words with: the
    /// drivers' `wordRegex` (old path first), else `diff.wordRegex`.
    fn word_regex(&self, old: Option<&str>, new: &str) -> Result<Option<Vec<u8>>, GitError>;

    /// The working-tree diff for one file: unstaged (index vs workdir) when
    /// `staged` is false, staged (HEAD vs index) when true. None if unchanged.
    fn file_diff(&self, path: &str, staged: bool) -> Result<Option<crate::FileDiff>, GitError>;

    /// Blame a working-tree file: each line with the commit that last touched it.
    fn blame(&self, path: &str) -> Result<Vec<crate::BlameLine>, GitError>;

    /// Blame a file as it is at revision `rev`.
    fn blame_at(&self, rev: &str, path: &str) -> Result<Vec<crate::BlameLine>, GitError>;

    /// git's blame with its options: ranges, `-w`, `-M`/`-C`, `--reverse`,
    /// ignored revisions and more.
    fn blame_with(&self, opts: &crate::BlameOptions) -> Result<crate::Blame, GitError>;

    /// All references: local branches, remote branches, and tags.
    fn refs(&self) -> Result<Vec<crate::RefEntry>, GitError>;

    /// List one directory of a revision's tree. `path` is empty for the root;
    /// entries come directories first, then files, each sorted by name.
    fn list_tree(&self, rev: &str, path: &str) -> Result<Vec<crate::TreeEntry>, GitError>;

    /// Read a file's contents at a revision. Binary blobs return with `text`
    /// None and `is_binary` set, rather than lossy garbage.
    fn read_blob(&self, rev: &str, path: &str) -> Result<crate::Blob, GitError>;

    /// The most recent commit that touched each of `paths` (full, slash-separated
    /// paths under a directory), keyed by path. Resolves via one history walk that
    /// exits once every path is found, for the tree view's latest-commit strip.
    fn tree_last_commits(
        &self,
        rev: &str,
        paths: &[String],
    ) -> Result<std::collections::HashMap<String, crate::LastCommit>, GitError>;

    /// Every file path in a revision's tree, for the fuzzy file finder.
    fn list_files(&self, rev: &str) -> Result<Vec<String>, GitError>;

    /// Case-insensitive literal search over the working tree, gitignore-aware and
    /// parallel (ripgrep's engine). Bounded in total matches so it stays
    /// responsive; binary files are skipped.
    fn grep(&self, pattern: &str) -> Result<Vec<crate::GrepMatch>, GitError>;

    /// Scoped code search: like [`GitBackend::grep`] but with an optional
    /// regex mode, a path substring filter, and an extension allowlist.
    fn grep_query(&self, q: &crate::GrepQuery) -> Result<Vec<crate::GrepMatch>, GitError>;

    /// Resolve a revision to its full 40-hex commit id, for stable permalinks.
    fn rev_parse(&self, rev: &str) -> Result<String, GitError>;

    /// Commit counts per author (name, email, count) from HEAD's history
    /// (bounded), most first, for a contributors summary.
    fn contributors(&self) -> Result<Vec<(String, String, usize)>, GitError>;

    /// The newest tag by tagged-commit time, if any, for a release summary.
    fn latest_tag(&self) -> Result<Option<crate::TagInfo>, GitError>;

    /// Every tag, newest tagged-commit first, for the releases page. Annotated
    /// tags carry their own message; lightweight tags fall back to the commit
    /// summary.
    fn all_tags(&self) -> Result<Vec<crate::TagInfo>, GitError>;

    /// A gzipped tarball of a revision's tree (as `git archive` would produce),
    /// for source downloads.
    fn archive_targz(&self, rev: &str) -> Result<Vec<u8>, GitError>;

    /// Check out a revision (tag or remote branch) as a detached HEAD.
    fn checkout_detached(&self, rev: &str) -> Result<(), GitError>;

    /// Check out the local branch `rev` (`branch`) or the revision `rev` as a
    /// detached HEAD, treating local changes as `mode` says.
    fn checkout_with(
        &self,
        rev: &str,
        branch: bool,
        mode: crate::CheckoutMode,
    ) -> Result<(), GitError>;

    /// Start the unborn branch `name` (git's `--orphan`), with the index and
    /// working tree of `start`, or empty when `None`.
    fn checkout_orphan(&self, name: &str, start: Option<&str>) -> Result<(), GitError>;

    /// Rebase the current branch onto `rev`; a conflict undoes the whole
    /// rebase. `report` receives git's output lines.
    fn rebase_onto(&self, rev: &str, report: &dyn Fn(crate::OpProgress)) -> Result<(), GitError>;

    /// Three-point rebase: replay the current branch's commits after `upstream`
    /// onto `onto` (i.e. `git rebase --onto <onto> <upstream>`). Used to restack
    /// a child branch after its parent was rewritten. Aborts on conflict.
    fn rebase_range(
        &self,
        upstream: &str,
        onto: &str,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// The commit oid a local branch points at, if it exists.
    fn branch_tip(&self, name: &str) -> Result<Option<String>, GitError>;

    /// Commits reachable from HEAD but not from `base`, oldest first (rebase-todo
    /// order): each `(short_id, subject)`.
    fn commits_between(&self, base: &str) -> Result<Vec<(String, String)>, GitError>;

    /// Walk up to `max_commits` of HEAD's first-parent history and accumulate,
    /// per changed file path, how many commits touched it and the newest such
    /// commit's author time. Used to churn- and recency-weight search results.
    fn file_activity(
        &self,
        max_commits: usize,
    ) -> Result<std::collections::HashMap<String, crate::FileActivity>, GitError>;

    /// Abort an in-progress rebase (rgit's or git's), restoring the pre-rebase
    /// branch and HEAD. Returns what restoring an autostash printed.
    fn rebase_abort(&self) -> Result<String, GitError>;

    /// Continue an in-progress rebase after resolving conflicts, committing
    /// what is staged. Returns the rebase's output.
    fn rebase_continue(&self) -> Result<String, GitError>;

    /// Skip the current commit of an in-progress rebase.
    fn rebase_skip(&self) -> Result<String, GitError>;

    /// Stop an in-progress rebase, leaving HEAD, the index and the working tree
    /// where they are (git's `--quit`).
    fn rebase_quit(&self) -> Result<(), GitError>;

    /// Edit the todo list of an in-progress rebase in the sequence editor
    /// (git's `--edit-todo`).
    fn rebase_edit_todo(&self) -> Result<(), GitError>;

    /// Rebase onto `upstream` (the branch's upstream when `None`), as git's
    /// sequencer does, in git's `.git/rebase-merge` state. A conflict, an
    /// `edit` or a `break` leaves the rebase in progress. Returns the output
    /// lines (`Successfully rebased and updated refs/heads/x.`).
    fn rebase_with(
        &self,
        upstream: Option<&str>,
        opts: &crate::RebaseOptions,
    ) -> Result<String, GitError>;

    /// Undo the last destructive operation, restoring HEAD, its branch, and the
    /// working tree from the operation log (recovers uncommitted work too).
    /// Returns a label of what was undone.
    fn undo(&self) -> Result<String, GitError>;

    /// Redo the operation most recently undone.
    fn redo(&self) -> Result<String, GitError>;

    /// The operation log (undo stack), newest first.
    fn oplog(&self) -> Result<Vec<crate::OpLogEntry>, GitError>;

    /// Fold each modified file's working-tree changes into the newest local
    /// commit (since the trunk) that last touched that file, via fixup commits
    /// and an autosquash rebase. Returns a summary.
    fn absorb(&self) -> Result<String, GitError>;

    /// Run a `git bisect` subcommand (`start <bad> <good>`, `good`, `bad`,
    /// `skip`, `reset`, `run`, ...) natively, with git's state files so git can
    /// continue it. Returns what git prints.
    fn bisect(&self, args: &[String]) -> Result<String, GitError>;

    /// Run `git rerere` (no argument, `clear`, `forget <paths>`, `status`,
    /// `remaining`, `diff` or `gc`) on git's rr-cache; `autoupdate` is
    /// `--[no-]rerere-autoupdate`. Returns what git prints on stdout.
    fn rerere(&self, args: &[String], autoupdate: Option<bool>) -> Result<String, GitError>;

    /// Config entries as `(name, value)`: every entry when `name` is `None`,
    /// else each value of `name` (a multivar has several), oldest first.
    fn config_entries(
        &self,
        scope: crate::ConfigScope,
        name: Option<&str>,
    ) -> Result<Vec<(String, String)>, GitError>;

    /// Set `name` to `value` (in the repository's config unless `scope` is
    /// global). With `add`, append a value to a multivar instead of replacing.
    fn config_write(
        &self,
        scope: crate::ConfigScope,
        name: &str,
        value: &str,
        add: bool,
    ) -> Result<(), GitError>;

    /// Remove `name`; `all` removes every value of a multivar.
    fn config_unset(
        &self,
        scope: crate::ConfigScope,
        name: &str,
        all: bool,
    ) -> Result<(), GitError>;

    /// Apply parsed patches (`git apply`) as `opts` say, returning git's
    /// progress lines. Rejected hunks and three-way conflicts are a
    /// `Conflict` error carrying them.
    fn apply_patch(
        &self,
        files: &[crate::FilePatch],
        opts: &crate::ApplyOpts,
    ) -> Result<String, GitError>;

    /// Notes under `notes_ref` (default `refs/notes/commits`) as
    /// `(note blob id, annotated object id)`.
    fn notes(&self, notes_ref: Option<&str>) -> Result<Vec<(String, String)>, GitError>;

    /// The note attached to `rev`.
    fn note_show(&self, notes_ref: Option<&str>, rev: &str) -> Result<String, GitError>;

    /// Set `message` as the note of `rev`, committing as `git notes <cmd>`
    /// (add, append, edit) does.
    fn note_add(
        &self,
        notes_ref: Option<&str>,
        rev: &str,
        message: &str,
        cmd: &str,
    ) -> Result<(), GitError>;

    /// Remove the notes of `revs` in one commit as `git notes <cmd>` does,
    /// returning which had a note; with `all_or_nothing` nothing is committed
    /// when one had none.
    fn note_remove(
        &self,
        notes_ref: Option<&str>,
        revs: &[String],
        all_or_nothing: bool,
        cmd: &str,
    ) -> Result<Vec<bool>, GitError>;

    /// Copy the note of `from` to `to`; an existing note on `to` needs `force`.
    fn note_copy(
        &self,
        notes_ref: Option<&str>,
        from: &str,
        to: &str,
        force: bool,
    ) -> Result<(), GitError>;

    /// Remove the notes of objects that no longer exist; returns their ids.
    /// `dry_run` only lists them.
    fn notes_prune(&self, notes_ref: Option<&str>, dry_run: bool) -> Result<Vec<String>, GitError>;

    /// Merge the notes of ref `other` into `notes_ref` with their merge base
    /// (`git notes merge`), printing as git does at `verbosity` (default 2).
    /// `strategy` is manual (conflicts are left in NOTES_MERGE_WORKTREE),
    /// ours, theirs, union or cat_sort_uniq. Returns stdout, and the stderr
    /// message and exit code when git would stop.
    fn notes_merge(
        &self,
        notes_ref: &str,
        other: &str,
        strategy: &str,
        verbosity: u8,
    ) -> Result<(String, Option<(String, i32)>), GitError>;

    /// Finish a manual notes merge: `commit` the resolved notes, or abort.
    fn notes_merge_finish(&self, commit: bool, verbosity: u8) -> Result<String, GitError>;

    /// Point ref `name` at revision `new`, or delete it when `new` is `None`
    /// (`git update-ref`). With `old`, only if the ref now holds `old` (all
    /// zeros: only if it does not exist). A symbolic ref is followed unless
    /// `no_deref`.
    fn update_ref(
        &self,
        name: &str,
        new: Option<&str>,
        old: Option<&str>,
        no_deref: bool,
        message: Option<&str>,
    ) -> Result<(), GitError> {
        let update = crate::RefUpdate {
            name: name.to_owned(),
            new: new.map(str::to_owned),
            old: old.map(str::to_owned),
            ..Default::default()
        };
        self.update_refs(&[update], message, no_deref, false, false, false)
            .map(drop)
    }

    /// Change refs all or nothing (`git update-ref`, also its `--stdin`
    /// transactions): each update is checked against its expected old value
    /// first. A symbolic ref is followed unless `no_deref`; `create_reflog`
    /// writes a reflog even outside the namespaces that keep one. With
    /// `check_only`, only verify. With `batch` (`--batch-updates`), an update
    /// that fails its checks is skipped and reported as git's `rejected <ref>
    /// <new> <old> <reason>` line instead of failing them all.
    fn update_refs(
        &self,
        updates: &[crate::RefUpdate],
        message: Option<&str>,
        no_deref: bool,
        create_reflog: bool,
        check_only: bool,
        batch: bool,
    ) -> Result<Vec<String>, GitError>;

    /// `git cherry`: the commits of `head` (after `limit`) missing from
    /// `upstream`, oldest first, each marked by whether upstream already has
    /// an equivalent change.
    fn cherry(
        &self,
        upstream: &str,
        head: &str,
        limit: Option<&str>,
    ) -> Result<Vec<crate::CherryCommit>, GitError>;

    /// Write a bundle of what `args` select (rev-list style); returns how
    /// many refs it records.
    fn bundle_create(&self, path: &std::path::Path, args: &[String]) -> Result<usize, GitError>;

    /// A bundle's header and the prerequisite commits this repository lacks.
    fn bundle_verify(
        &self,
        path: &std::path::Path,
    ) -> Result<(crate::BundleHeader, Vec<(String, String)>), GitError>;

    /// Store a bundle's objects (refs are left alone); returns its refs.
    fn bundle_unbundle(&self, path: &std::path::Path) -> Result<Vec<(String, String)>, GitError>;

    /// `git request-pull`: the summary asking `url`'s owner to pull `end`
    /// (default HEAD) since `start`, and git's warnings when `url` lacks it.
    fn request_pull(
        &self,
        start: &str,
        url: &str,
        end: Option<&str>,
        patch: bool,
    ) -> Result<(String, Vec<String>), GitError>;

    /// `git range-diff`: the commits of two ranges paired up, with how each
    /// pair's patch changed.
    fn range_diff(&self, opts: &crate::RangeDiffOpts) -> Result<String, GitError>;

    /// A merge's combined diff against all its parents, limited to `paths`
    /// (git's `-c`; `dense` is `--cc`).
    fn combined_diff(
        &self,
        commit: &str,
        paths: &[String],
        dense: bool,
    ) -> Result<Vec<crate::CombinedFile>, GitError>;

    /// A merge against a fresh re-merge of its parents, conflict markers and
    /// all (git's `--remerge-diff`); empty unless it has two parents.
    fn remerge_diff(
        &self,
        commit: &str,
        paths: &[String],
    ) -> Result<Vec<crate::FileDiff>, GitError>;

    /// git's `log -L`: follow the `specs` line ranges (`start,end:file` or
    /// `:funcname:file`, read at `tip`) back through `order`, git's
    /// topological order of the walk. Per commit: the diffs of the ranges to
    /// show, empty for a merge, or None when it did not touch them.
    fn line_log(
        &self,
        tip: &str,
        order: &[String],
        specs: &[String],
        first_parent: bool,
    ) -> Result<Vec<Option<Vec<crate::FileDiff>>>, GitError>;

    /// Commits as mbox emails (`git format-patch`), oldest first, the cover
    /// letter (if asked for) first.
    fn format_patch(
        &self,
        opts: &crate::FormatPatchOpts,
    ) -> Result<Vec<crate::PatchMail>, GitError>;

    /// Apply mbox patches as commits (`git am`). `args` are `git am` arguments
    /// (mbox paths, `--abort`, `--continue`, `--skip`, ...); `mbox` is patch
    /// text to apply instead of files. The session lives in git's
    /// `.git/rebase-apply`, so git and rgit can continue each other's.
    fn am(&self, args: &[String], mbox: Option<&[u8]>) -> Result<String, GitError>;

    /// A revision's tree as an archive (`git archive`), honouring the
    /// export-ignore and export-subst attributes.
    fn archive(&self, opts: &crate::ArchiveOpts) -> Result<Vec<u8>, GitError>;

    /// Pack and prune the object database as `git gc` does; returns what it
    /// reports.
    fn gc(&self, opts: &crate::GcOptions) -> Result<String, GitError>;

    /// Pack objects as `git repack` does; returns what it reports.
    fn repack(&self, opts: &crate::RepackOptions) -> Result<String, GitError>;

    /// Write or verify the commit-graph (`git commit-graph`); what verify
    /// finds wrong, in git's words.
    fn commit_graph(&self, op: &crate::CommitGraphOp) -> Result<Vec<String>, GitError>;

    /// Write, verify, expire or repack the multi-pack-index
    /// (`git multi-pack-index`); what verify finds wrong, in git's words.
    fn multi_pack_index(&self, op: &crate::MidxOp) -> Result<Vec<String>, GitError>;

    /// Move loose refs into packed-refs (`git pack-refs`): every ref with
    /// `all`, else tags and refs already packed; `auto` only when enough loose
    /// refs piled up.
    fn pack_refs(&self, all: bool, no_prune: bool, auto: bool) -> Result<(), GitError>;

    /// Drop old reflog entries (`git reflog expire`); git's report lines.
    fn reflog_expire(&self, opts: &crate::ReflogExpire) -> Result<Vec<String>, GitError>;

    /// Drop reflog entries named `ref@{n}` (`git reflog delete`).
    fn reflog_delete(&self, entries: &[String], opts: &crate::ReflogExpire)
    -> Result<(), GitError>;

    /// Whether the full ref name has a reflog (`git reflog exists`).
    fn reflog_exists(&self, name: &str) -> bool;

    /// Run maintenance tasks (`git maintenance run`); returns what they report.
    fn maintenance_run(&self, opts: &crate::MaintenanceRun) -> Result<String, GitError>;

    /// Verify the object database as `git fsck` does: its stdout, stderr
    /// and exit code.
    fn fsck(&self, opts: &crate::FsckOptions) -> Result<crate::FsckReport, GitError>;

    /// The untracked (or ignored) paths `git clean` would remove, from the
    /// root, folders ending in `/`.
    fn clean_candidates(&self, opts: &crate::CleanOptions) -> Result<Vec<String>, GitError>;

    /// Remove `items` (from [`Self::clean_candidates`]) as `git clean` does,
    /// or only report them with `opts.dry_run`; returns git's lines.
    fn clean_remove(
        &self,
        items: &[String],
        opts: &crate::CleanOptions,
    ) -> Result<Vec<String>, GitError>;

    /// Remove tracked paths matching `paths` (files, folders or globs) from the
    /// index (`git rm`), and from the working tree unless `opts.cached`. A
    /// folder needs `recursive`. Without `force`, refuse like git when a file
    /// has staged or unstaged changes. Nothing is removed when any path fails.
    /// Returns the removed paths.
    fn remove_paths(
        &self,
        paths: &[String],
        opts: crate::RmOptions,
    ) -> Result<Vec<String>, GitError>;

    /// Rename a tracked file or folder (`git mv`). An existing folder as `to`
    /// receives `from` inside it. Without `force`, refuse to overwrite an
    /// existing destination (git's default); `force` (git's `-f`) overwrites it.
    /// `dry_run` only checks. Returns the destination path.
    fn move_path(
        &self,
        from: &str,
        to: &str,
        force: bool,
        dry_run: bool,
    ) -> Result<String, GitError>;

    /// Mark untracked files under `paths` as intended to be added (git's
    /// `add -N`): an empty index entry, so diff and status show them.
    fn intent_to_add(&self, paths: &[String]) -> Result<Vec<String>, GitError>;

    /// Write our (`ours`) or their side of the conflicted paths under `paths`
    /// to the working tree, keeping the conflict (checkout's `--ours`/`--theirs`).
    fn checkout_side(&self, paths: &[String], ours: bool) -> Result<(), GitError>;

    /// Describe `rev` relative to the nearest tag, as `git describe` does.
    fn describe(&self, rev: &str, opts: &crate::DescribeOptions) -> Result<String, GitError>;

    /// Run any `git` subcommand and return its stdout - the escape hatch for
    /// operations rgit does not model natively (submodule, notes, grep, gc, ...).
    fn git(&self, args: &[String]) -> Result<String, GitError>;

    /// Reset HEAD (and, per mode, the index and worktree) to `rev`.
    fn reset(&self, rev: &str, mode: crate::ResetMode) -> Result<(), GitError>;

    /// Reset the index entries of `paths` (files, folders or globs) to `rev`,
    /// leaving HEAD and the working tree alone (git's `reset <rev> -- <paths>`).
    fn reset_paths(&self, rev: &str, paths: &[String]) -> Result<(), GitError>;

    /// Cherry-pick `rev` onto HEAD. With `no_commit` (git's `-n`), apply it to
    /// the index and working tree without committing.
    fn cherry_pick(&self, rev: &str, no_commit: bool) -> Result<(), GitError> {
        let opts = crate::PickOptions {
            no_commit,
            ..Default::default()
        };
        self.pick(&[rev.to_owned()], &opts)
    }

    /// Revert `rev` on HEAD. With `no_commit` (git's `-n`), apply the inverse to
    /// the index and working tree without committing.
    fn revert(&self, rev: &str, no_commit: bool) -> Result<(), GitError> {
        let opts = crate::PickOptions {
            revert: true,
            no_commit,
            ..Default::default()
        };
        self.pick(&[rev.to_owned()], &opts)
    }

    /// Cherry-pick (or, with `opts.revert`, revert) commits in order. Each rev is
    /// a commit or a range `A..B`. On a conflict the sequence stops with git's
    /// sequencer state, for [`pick_continue`](Self::pick_continue),
    /// [`pick_skip`](Self::pick_skip) or [`pick_abort`](Self::pick_abort).
    fn pick(&self, revs: &[String], opts: &crate::PickOptions) -> Result<(), GitError>;

    /// Commit the resolved commit of a stopped cherry-pick or revert and apply
    /// the rest of the sequence.
    fn pick_continue(&self) -> Result<(), GitError>;

    /// Drop the current commit of a stopped cherry-pick or revert and apply the
    /// rest of the sequence.
    fn pick_skip(&self) -> Result<(), GitError>;

    /// Cancel a stopped cherry-pick or revert, restoring the HEAD it started from.
    /// When HEAD has moved since the stop, only the sequencer state is dropped
    /// and git's warning is returned; otherwise the result is empty.
    fn pick_abort(&self) -> Result<String, GitError>;

    /// Forget a stopped cherry-pick or revert, leaving HEAD, the index and the
    /// working tree as they are (git's `--quit`).
    fn pick_quit(&self) -> Result<(), GitError>;

    /// Merge `rev` into the current branch. Fast-forwards when possible unless
    /// `no_ff` forces a merge commit. `report` receives git-style progress lines
    /// (`Updating`, `Fast-forward`, `CONFLICT ...`).
    fn merge(
        &self,
        rev: &str,
        no_ff: bool,
        ff_only: bool,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError> {
        let opts = crate::MergeOptions {
            no_ff,
            ff_only,
            ..Default::default()
        };
        self.merge_with(&[rev.to_owned()], &opts, report)
    }

    /// Merge one or more revisions (several make an octopus merge) into the
    /// current branch.
    fn merge_with(
        &self,
        revs: &[String],
        opts: &crate::MergeOptions,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

    /// Commit a merge whose conflicts are resolved, with every MERGE_HEAD as a
    /// parent (git's `merge --continue`).
    fn merge_continue(&self) -> Result<(), GitError>;

    /// Abort an in-progress merge, restoring the working tree and index to HEAD
    /// (git's `merge --abort`).
    fn merge_abort(&self) -> Result<(), GitError>;

    /// Forget an in-progress merge, leaving the index and working tree as they
    /// are (git's `merge --quit`).
    fn merge_quit(&self) -> Result<(), GitError>;

    /// Resolve a conflicted path by taking our side (`ours`) or theirs, writing
    /// that version to the worktree and staging it.
    fn resolve_conflict(&self, path: &str, ours: bool) -> Result<(), GitError>;

    /// Create a tag at HEAD (annotated when `message` is non-empty).
    fn create_tag(&self, name: &str, message: &str) -> Result<(), GitError>;

    /// Create a tag at `rev` (annotated when `message` is non-empty), replacing
    /// a tag of the same name when `force`.
    fn create_tag_at(
        &self,
        name: &str,
        rev: &str,
        message: &str,
        force: bool,
    ) -> Result<(), GitError>;

    /// Create a tag at `rev` as `git tag` does: annotated when `message` is
    /// given, cleaned per `cleanup` (strip, whitespace or verbatim), and
    /// signed when `sign` is set (`""` for the default key).
    fn tag_with(
        &self,
        name: &str,
        rev: &str,
        message: Option<&str>,
        cleanup: &str,
        sign: Option<&str>,
        force: bool,
    ) -> Result<(), GitError>;

    /// Check the named tags' signatures, returning git's report (`git tag -v`).
    fn verify_tags(&self, names: &[String]) -> Result<String, GitError>;

    /// Check the signature of the commit or (with `tag`) tag object at `rev`,
    /// as `git verify-commit` / `git verify-tag` do.
    fn signature_check(&self, rev: &str, tag: bool) -> Result<crate::SignatureCheck, GitError>;

    /// Whether `ancestor` is `rev` or an ancestor of it (git's `merge-base
    /// --is-ancestor`).
    fn is_ancestor(&self, ancestor: &str, rev: &str) -> Result<bool, GitError>;

    /// Delete a tag.
    fn delete_tag(&self, name: &str) -> Result<(), GitError>;

    /// Configured remotes with their fetch URLs, sorted by name.
    fn remotes(&self) -> Result<Vec<crate::Remote>, GitError>;

    /// Add a remote named `name` pointing at `url`.
    fn add_remote(&self, name: &str, url: &str) -> Result<(), GitError>;

    /// Remove the remote named `name`.
    fn remove_remote(&self, name: &str) -> Result<(), GitError>;

    /// Change a remote's fetch URL (git's `remote set-url`).
    fn set_remote_url(&self, name: &str, url: &str) -> Result<(), GitError>;

    /// Every URL of remote `name`, or its push URLs with `push` (its URLs
    /// when it has none), as `git remote get-url --all [--push]` lists them.
    fn remote_urls(&self, name: &str, push: bool) -> Result<Vec<String>, GitError>;

    /// Set remote `name`'s push URL (git's `remote set-url --push`).
    fn set_remote_push_url(&self, name: &str, url: &str) -> Result<(), GitError>;

    /// Delete the remote-tracking branches of `name` that no longer exist on
    /// it (git's `remote prune`). Returns the branches deleted.
    fn prune_remote(&self, name: &str) -> Result<Vec<String>, GitError>;

    /// Edit a remote's `url`s (`pushurl`s with `push`) like `git remote
    /// set-url`: `add` appends `url`, `delete` drops those matching the regex
    /// `url`, and otherwise `url` replaces those matching `old` (the single
    /// one without it).
    fn edit_remote_urls(
        &self,
        name: &str,
        url: &str,
        old: Option<&str>,
        push: bool,
        add: bool,
        delete: bool,
    ) -> Result<(), GitError>;

    /// The refs a remote has, as (name, id), and the branch its HEAD names
    /// when it says (a symref), read from its first URL (`git ls-remote`).
    fn remote_heads(&self, name: &str) -> Result<RemoteHeads, GitError>;

    /// Rename a remote and its tracking refs (git's `remote rename`).
    fn rename_remote(&self, old: &str, new: &str) -> Result<(), GitError>;

    /// Linked worktrees, sorted by name.
    fn worktrees(&self) -> Result<Vec<crate::Worktree>, GitError>;

    /// Create a linked worktree named `name` at `path` (on a new branch `name`).
    fn add_worktree(&self, name: &str, path: &str) -> Result<(), GitError>;

    /// Add a linked worktree at `path` as `git worktree add` does: on a new
    /// branch (-b/-B, or an unborn one with `orphan`) at `commitish` (default
    /// HEAD); else on branch `commitish`, or on a new branch tracking the one
    /// remote branch of that name; else detached at `commitish` (also with
    /// `detach`); else on a branch named after `path`'s last folder, created
    /// at HEAD if missing.
    fn worktree_add(
        &self,
        path: &str,
        commitish: Option<&str>,
        args: &crate::WorktreeAddArgs,
    ) -> Result<(), GitError>;

    /// Fix the links between worktrees and the repository after either was
    /// moved (`git worktree repair`), also for worktrees now at `paths`.
    /// Returns what was repaired.
    fn repair_worktrees(&self, paths: &[String]) -> Result<Vec<String>, GitError>;

    /// Lock a linked worktree (by name or path) against pruning, with an
    /// optional reason.
    fn worktree_lock(&self, worktree: &str, reason: Option<&str>) -> Result<(), GitError>;

    /// Unlock a linked worktree (by name or path).
    fn worktree_unlock(&self, worktree: &str) -> Result<(), GitError>;

    /// Move a linked worktree (by name or path) to `new_path`.
    fn worktree_move(&self, worktree: &str, new_path: &str) -> Result<(), GitError>;

    /// Remove a linked worktree (by name or path) and its folder. It refuses
    /// one with modified or untracked files unless `force`, which also removes
    /// a locked one (git's `-f`).
    fn remove_worktree(&self, name: &str, force: bool) -> Result<(), GitError>;

    /// Prune worktree admin entries whose working tree is gone (git's `worktree
    /// prune`). Returns the names pruned.
    fn prune_worktrees(&self) -> Result<Vec<String>, GitError>;

    /// Prune unreachable loose objects older than `expire` (default: all), as
    /// git's `prune`; `dry_run` only reports. Returns `<id> <type>` lines
    /// (with `dry_run` or `verbose`).
    fn prune_objects(
        &self,
        expire: Option<&str>,
        dry_run: bool,
        verbose: bool,
    ) -> Result<String, GitError>;

    /// Delete a local branch.
    /// Delete a local branch. With `force` false (git's `-d`), refuse a branch
    /// not merged into HEAD; with `force` true (`-D`), delete regardless.
    fn delete_branch(&self, name: &str, force: bool) -> Result<(), GitError>;

    /// Rename a local branch.
    fn rename_branch(&self, old: &str, new: &str) -> Result<(), GitError>;

    /// Copy branch `old` to `new` with its upstream config and reflog, like
    /// `git branch -c`; `force` replaces an existing `new`.
    fn copy_branch(&self, old: &str, new: &str, force: bool) -> Result<(), GitError>;

    /// Read a git config value (local, then global), `None` if unset.
    fn config_get(&self, key: &str) -> Result<Option<String>, GitError>;

    /// Set a git config value in the repository's local config.
    fn config_set(&self, key: &str, value: &str) -> Result<(), GitError>;

    /// Whether a local branch of this name exists.
    fn branch_exists(&self, name: &str) -> bool;

    /// Whether the lanes overlay is active (see the `lanes` module).
    fn lanes_active(&self) -> bool;

    /// Enter lanes mode: record the fork point and a default lane.
    fn lanes_init(&self) -> Result<(), GitError>;

    /// Leave lanes mode: delete the state ref, leaving lane branches in place.
    fn lanes_off(&self) -> Result<(), GitError>;

    /// The lanes state, reconciled against the working tree.
    fn lanes_state(&self) -> Result<crate::LanesState, GitError>;

    /// Create a new empty lane committing to a same-named branch.
    fn lane_new(&self, name: &str) -> Result<(), GitError>;

    /// Create a new lane stacked on another lane (its commits build on that
    /// lane's branch), recorded so `restack` composes.
    fn lane_stack(&self, name: &str, parent: &str) -> Result<(), GitError>;

    /// Move each stacked lane onto its parent lane's current tip, in the object
    /// database (no checkout), so it is safe with the dirty worktree lanes keep.
    fn lane_restack(&self) -> Result<crate::RestackOutcome, GitError>;

    /// Assign a worktree path to a lane.
    fn lane_assign(&self, lane: &str, path: &str) -> Result<(), GitError>;

    /// Assign one hunk of a tracked file (by its current `new_start`) to a lane.
    fn lane_assign_hunk(&self, lane: &str, path: &str, new_start: u32) -> Result<(), GitError>;

    /// Return a path to the default lane.
    fn lane_unassign(&self, path: &str) -> Result<(), GitError>;

    /// Commit a lane's owned changes to its branch (synthesized in memory).
    fn lane_commit(&self, lane: &str, message: &str) -> Result<String, GitError>;

    /// Rename a lane and its branch.
    fn lane_rename(&self, old: &str, new: &str) -> Result<(), GitError>;

    /// Delete a lane, returning its changes to the default lane (branch kept).
    fn lane_delete(&self, name: &str) -> Result<(), GitError>;

    /// Push a lane's branch to the remote and set its upstream (HEAD stays put).
    fn lane_push(&self, lane: &str) -> Result<String, GitError>;

    /// Push a lane's branch and open a pull/merge request for it via the forge
    /// CLI (gh/glab), against the trunk.
    fn lane_pr(&self, lane: &str) -> Result<String, GitError>;

    /// Install an interactive credential prompt for network operations. Backends
    /// that cannot prompt ignore it (the default), failing auth cleanly instead.
    fn set_credential_prompt(&self, _prompt: Box<dyn crate::CredentialPrompt>) {}

    /// Create a new branch stacked on the current one, recording its parent and
    /// fork point in git config.
    fn stack_new(&self, name: &str) -> Result<String, GitError> {
        let parent = self
            .status()?
            .head
            .branch
            .ok_or_else(|| GitError::Other("HEAD is detached; not on a branch".into()))?;
        self.create_branch(name)?;
        self.config_set(&format!("branch.{name}.rgit-stack-parent"), &parent)?;
        if let Some(tip) = self.branch_tip(&parent)? {
            self.config_set(&format!("branch.{name}.rgit-stack-base"), &tip)?;
        }
        Ok(format!("created {name} stacked on {parent}"))
    }

    /// Each local branch paired with its recorded stacked-branch parent, if any.
    fn stack_parents(&self) -> Result<Vec<(String, Option<String>)>, GitError> {
        let mut out = Vec::new();
        for branch in self.local_branches()? {
            let parent = self.config_get(&format!("branch.{branch}.rgit-stack-parent"))?;
            out.push((branch, parent));
        }
        Ok(out)
    }

    /// Rebase every stacked branch onto its parent's current tip (parents
    /// first), replaying only each branch's own commits. A conflict on one
    /// branch does not abort the whole operation: that branch is left untouched
    /// at its old base (the conflicted rebase self-aborts) and reported as
    /// conflicted, while the rest of the stack still moves. Composed from the
    /// other operations, so any backend gets it.
    fn restack(&self) -> Result<crate::RestackOutcome, GitError> {
        let parents: std::collections::HashMap<String, String> = self
            .stack_parents()?
            .into_iter()
            .filter_map(|(b, p)| p.map(|p| (b, p)))
            .collect();
        if parents.is_empty() {
            return Ok(crate::RestackOutcome::default());
        }
        let start = self.status()?.head.branch;

        let mut order: Vec<String> = parents.keys().cloned().collect();
        order.sort_by_key(|b| stack_depth(b, &parents));

        let mut outcome = crate::RestackOutcome::default();
        let result = (|| -> Result<(), GitError> {
            for branch in &order {
                let parent = &parents[branch];
                let base_key = format!("branch.{branch}.rgit-stack-base");
                let recorded = self.config_get(&base_key)?;
                let current_tip = self.branch_tip(parent)?;
                // A conflict self-aborts and leaves us back on `branch`; record
                // it and carry on, so one snag does not strand the whole stack.
                let rebased = match (recorded, &current_tip) {
                    (Some(base), Some(tip)) if base != *tip => {
                        self.checkout_branch(branch)?;
                        Some(self.rebase_range(&base, parent, &|_| {}))
                    }
                    (None, Some(_)) => {
                        self.checkout_branch(branch)?;
                        Some(self.rebase_onto(parent, &|_| {}))
                    }
                    _ => None,
                };
                match rebased {
                    None => {}
                    Some(Ok(())) => {
                        if let Some(tip) = &current_tip {
                            self.config_set(&base_key, tip)?;
                        }
                        outcome.restacked.push(format!("{branch} -> {parent}"));
                    }
                    Some(Err(GitError::Conflict(_))) => outcome.conflicted.push(branch.clone()),
                    Some(Err(e)) => return Err(e),
                }
            }
            Ok(())
        })();
        if let Some(start) = start {
            let _ = self.checkout_branch(&start);
        }
        result?;
        Ok(outcome)
    }

    /// The id of the object a revision names, not peeled (`git rev-parse`).
    fn resolve_object(&self, rev: &str) -> Result<String, GitError>;

    /// The shortest unique abbreviation of an object id, at least `min` digits;
    /// 0 is git's default (core.abbrev, or scaled to the object count).
    fn abbrev_id(&self, id: &str, min: usize) -> Result<String, GitError>;

    /// The full ref name a revision goes through (`refs/heads/main`), if any.
    fn full_ref_name(&self, rev: &str) -> Result<Option<String>, GitError>;

    /// Where a symbolic ref points (`HEAD` -> `refs/heads/main`); None when direct.
    fn symbolic_ref(&self, name: &str) -> Result<Option<String>, GitError>;

    /// Point symbolic ref `name` at ref `target` (`git symbolic-ref NAME REF`).
    fn set_symbolic_ref(
        &self,
        name: &str,
        target: &str,
        message: Option<&str>,
    ) -> Result<(), GitError>;

    /// The repository's `.git` directory.
    fn git_dir(&self) -> std::path::PathBuf;

    /// An object's type, id and raw content (`git cat-file`).
    fn read_object(&self, rev: &str) -> Result<crate::RawObject, GitError>;

    /// An object's size on disk and the id of its delta base, if stored as
    /// a delta (cat-file's `%(objectsize:disk)` and `%(deltabase)`).
    fn object_disk(&self, id: &str) -> Result<(u64, Option<String>), GitError>;

    /// One `git diff-pairs` record as a [`crate::FileDiff`] with git's
    /// header: the diff of its blobs.
    fn diff_pair(
        &self,
        pair: &crate::RawPair,
        context: Option<u32>,
    ) -> Result<crate::FileDiff, GitError>;

    /// An object's type and size, from its header alone.
    fn object_header(&self, id: &str) -> Result<(String, u64), GitError>;

    /// rev-list's `--bisect` over `tips ^hidden -- paths`: the best commit
    /// first (every one, best first, with `all`) with its distance from the
    /// ends, how many commits the best reaches and how many there are.
    fn rev_list_bisect(
        &self,
        tips: &[String],
        hidden: &[String],
        first_parent: bool,
        paths: &[String],
        all: bool,
    ) -> Result<crate::BisectPick<String>, GitError>;

    /// A blob's content as checked out to `path` (cat-file's `--filters`), or
    /// with `textconv` through its diff driver's textconv command.
    fn convert_blob(&self, path: &str, data: &[u8], textconv: bool) -> Result<Vec<u8>, GitError>;

    /// Every object id in the object database, sorted.
    fn all_objects(&self) -> Result<Vec<String>, GitError>;

    /// A revision's tree entries under `paths`, as `git ls-tree` lists them.
    fn ls_tree(
        &self,
        rev: &str,
        paths: &[String],
        walk: crate::TreeWalk,
    ) -> Result<Vec<crate::TreeItem>, GitError>;

    /// Every index entry with its mode, id and stage (`git ls-files -s`).
    fn index_entries(&self) -> Result<Vec<crate::IndexItem>, GitError>;

    /// Untracked, modified and deleted paths, plus ignored ones when asked.
    fn path_states(&self, ignored: bool) -> Result<Vec<(String, crate::PathState)>, GitError>;

    /// Every ref under `refs/` with its object and message, sorted by name.
    fn ref_details(&self) -> Result<Vec<crate::RefDetail>, GitError>;

    /// The commits a `git rev-list` walk shows, as [`GitBackend::log`] picks
    /// and orders them.
    fn rev_walk(&self, walk: &crate::LogOptions) -> Result<Vec<crate::WalkCommit>, GitError>;

    /// The trees and blobs `git rev-list --objects` lists after `commits`:
    /// each commit's tree then what is in it, depth first, each object once
    /// and none that `edges`' trees hold. Each comes with its path (empty for
    /// a commit's tree) and whether it is missing from the repository.
    fn list_objects(
        &self,
        commits: &[String],
        edges: &[String],
    ) -> Result<Vec<(String, String, bool)>, GitError>;

    /// The best common ancestor of two commits, or all of them.
    fn merge_bases(&self, a: &str, b: &str, all: bool) -> Result<Vec<String>, GitError>;

    /// A ref's reflog, newest first.
    fn reflog(&self, name: &str) -> Result<Vec<crate::ReflogItem>, GitError>;

    /// Search tracked files as `git grep` does: every matching line, in path order.
    fn git_grep(&self, q: &crate::GitGrep) -> Result<Vec<crate::GrepHit>, GitError>;

    /// Which of `paths` the ignore rules cover, in order. One lock
    /// acquisition for the batch, for worktree watchers that must drop
    /// ignored-path events without a lock per path.
    fn paths_ignored(&self, paths: &[&std::path::Path]) -> Result<Vec<bool>, GitError> {
        let _ = paths;
        Ok(Vec::new())
    }

    /// The rule that ignores `path`, or None when it is not ignored. Tracked
    /// paths are never ignored unless `no_index`.
    fn check_ignore(
        &self,
        path: &str,
        no_index: bool,
    ) -> Result<Option<crate::IgnoreRule>, GitError>;

    /// `Name <email> time zone` for the author or committer (`git var`).
    fn ident(&self, committer: bool) -> Result<String, GitError>;

    /// Loose and packed object counts (`git count-objects`).
    fn count_objects(&self) -> Result<crate::ObjectCounts, GitError>;
}

/// How many stacked ancestors a branch has, for ordering parents before
/// children during a restack.
fn stack_depth(branch: &str, parents: &std::collections::HashMap<String, String>) -> usize {
    let mut depth = 0;
    let mut cursor = branch;
    while let Some(parent) = parents.get(cursor) {
        if !parents.contains_key(parent) {
            break;
        }
        depth += 1;
        cursor = parent;
        if depth > 1000 {
            break;
        }
    }
    depth
}
