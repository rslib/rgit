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

    /// Stash the working tree and index (including untracked files). Returns the
    /// git-style `Saved working directory and index state WIP on ...` line.
    fn stash_push(&self) -> Result<String, GitError>;

    /// Stash the working tree and index under a descriptive `message`. Returns
    /// the git-style `Saved working directory and index state ...` line.
    fn stash_push_message(&self, message: &str) -> Result<String, GitError>;

    /// Pop the stash at `index` (apply it and drop it).
    fn stash_pop(&self, index: usize) -> Result<(), GitError>;

    /// Apply the stash at `index` without dropping it.
    fn stash_apply(&self, index: usize) -> Result<(), GitError>;

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
    fn commit_file_diff(&self, rev: &str, path: &str)
    -> Result<Option<crate::FileDiff>, GitError>;

    /// The diff between two revisions' trees (`from` as the old side).
    fn diff_refs(&self, from: &str, to: &str) -> Result<Vec<crate::FileDiff>, GitError>;

    /// The working-tree diff for one file: unstaged (index vs workdir) when
    /// `staged` is false, staged (HEAD vs index) when true. None if unchanged.
    fn file_diff(&self, path: &str, staged: bool) -> Result<Option<crate::FileDiff>, GitError>;

    /// Blame a working-tree file: each line with the commit that last touched it.
    fn blame(&self, path: &str) -> Result<Vec<crate::BlameLine>, GitError>;

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

    /// A gzipped tarball of a revision's tree (as `git archive` would produce),
    /// for source downloads.
    fn archive_targz(&self, rev: &str) -> Result<Vec<u8>, GitError>;

    /// Check out a revision (tag or remote branch) as a detached HEAD.
    fn checkout_detached(&self, rev: &str) -> Result<(), GitError>;

    /// Rebase the current branch onto `rev` in-process; aborts on conflict.
    /// `report` receives git-style progress lines (`Applying: ...`).
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

    /// Abort an in-progress rebase, restoring the pre-rebase HEAD. Shells out to
    /// `git` so it works for CLI-started (interactive) rebases too, which libgit2
    /// cannot drive.
    fn rebase_abort(&self) -> Result<(), GitError>;

    /// Continue an in-progress rebase after resolving conflicts. Shells out to
    /// `git` (libgit2 cannot drive a CLI-started rebase).
    fn rebase_continue(&self) -> Result<(), GitError>;

    /// Skip the current commit of an in-progress rebase. Shells out to `git`.
    fn rebase_skip(&self) -> Result<(), GitError>;

    /// Start an interactive rebase (`git rebase -i [onto]`), inheriting the
    /// terminal so git can open the todo editor. Needs a TTY.
    fn rebase_interactive(&self, onto: Option<&str>) -> Result<(), GitError>;

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

    /// Merge `rev` into the current branch. Fast-forwards when possible unless
    /// `no_ff` forces a merge commit. `report` receives git-style progress lines
    /// (`Updating`, `Fast-forward`, `CONFLICT ...`).
    fn merge(
        &self,
        rev: &str,
        no_ff: bool,
        report: &dyn Fn(crate::OpProgress),
    ) -> Result<(), GitError>;

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
