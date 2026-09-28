use std::path::{Path, PathBuf};
use std::sync::Mutex;

use git2::build::CheckoutBuilder;
use git2::{
    ApplyLocation, ApplyOptions, BranchType, Cred, CredentialType, Delta, Diff, DiffFindOptions,
    DiffOptions, ErrorCode, FetchOptions, ObjectType, Oid, Patch, Pathspec, PathspecFlags,
    PushOptions, RemoteCallbacks, Repository, ResetType, Status, StatusOptions,
};

use crate::model::{ConfigScope, RepoState, ResetMode};

use crate::backend::GitBackend;
use crate::diff::{DiffLine, FileDiff, Hunk, LineOrigin};
use crate::error::GitError;
use crate::model::{Commit, Head, OpProgress, RepoStatus, Stash, StatusCode, StatusEntry};

const RECENT_LIMIT: usize = 10;

/// An in-process git backend over libgit2. Reads (status, diff, log) never spawn
/// a subprocess. The repository handle is `!Sync`, so it lives behind a mutex;
/// operations run on a blocking task one at a time.
pub struct Git2Backend {
    repo: Mutex<Repository>,
    workdir: PathBuf,
    /// An optional interactive credential prompt for network operations.
    cred_prompt: Mutex<Option<Box<dyn crate::CredentialPrompt>>>,
}

impl Git2Backend {
    /// Discover the repository containing `start` and validate it has a worktree.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self, GitError> {
        // Install the in-process, ssh-config-aware ssh transport (idempotent).
        #[cfg(feature = "ssh")]
        crate::ssh::register();
        let start = start.as_ref();
        let repo = Repository::discover(start).map_err(|e| match e.code() {
            ErrorCode::NotFound => GitError::NotARepository(start.to_path_buf()),
            _ => GitError::Git(e),
        })?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| GitError::Bare(repo.path().to_path_buf()))?
            .to_path_buf();
        Ok(Self {
            repo: Mutex::new(repo),
            workdir,
            cred_prompt: Mutex::new(None),
        })
    }

    /// Run a destructive operation under an op-log snapshot so it can be undone.
    /// On failure the snapshot is discarded if the repository is unchanged.
    /// Snapshot trouble is logged but never blocks the operation. Takes and
    /// releases the repo lock around `op`, so `op` may lock it (the mutex is not
    /// reentrant).
    fn logged<T>(
        &self,
        label: &str,
        op: impl FnOnce() -> Result<T, GitError>,
    ) -> Result<T, GitError> {
        let pushed = {
            let repo = self.repo.lock().expect("repo mutex");
            crate::oplog::snapshot(&repo, label).unwrap_or_else(|e| {
                tracing::warn!(target: "rgit_git", "oplog snapshot failed: {e}");
                None
            })
        };
        let result = op();
        if result.is_err()
            && let Some(pushed) = pushed
        {
            let repo = self.repo.lock().expect("repo mutex");
            if let Err(e) = crate::oplog::discard(&repo, &pushed) {
                tracing::warn!(target: "rgit_git", "oplog discard failed: {e}");
            }
        }
        result
    }

    /// Push arbitrary refspecs to `remote` (or the current branch's upstream
    /// remote when `None`). Shared by tag and delete pushes; surfaces a refused
    /// ref as an error the way [`push`] does.
    fn push_refspecs(
        &self,
        remote: Option<&str>,
        refspecs: &[String],
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        let remote_name = match remote {
            Some(r) => r.to_owned(),
            None => upstream_remote(&repo)?.0,
        };
        let mut remote = repo.find_remote(&remote_name)?;
        if let Ok(url) = remote.url() {
            report(OpProgress::Line(format!("To {url}")));
        }
        let rejected = std::sync::atomic::AtomicBool::new(false);
        let callbacks = remote_callbacks(report, &rejected, cred_guard.as_deref());
        let mut opts = PushOptions::new();
        opts.remote_callbacks(callbacks);
        let specs: Vec<&str> = refspecs.iter().map(String::as_str).collect();
        remote.push(&specs, Some(&mut opts))?;
        if rejected.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(GitError::PushRejected);
        }
        Ok(())
    }

    /// Run a `git` CLI command in the working directory, returning its stdout on
    /// success or its stderr as a `Cli` error. Used only for the few operations
    /// libgit2 cannot do (rebase continue/skip, bisect).
    fn run_git(&self, args: &[&str], env: &[(&str, &str)]) -> Result<String, GitError> {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args).current_dir(&self.workdir);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd
            .output()
            .map_err(|e| GitError::Cli(format!("could not run git: {e}")))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
        } else {
            let msg = String::from_utf8_lossy(&out.stderr);
            let msg = msg.trim();
            Err(GitError::Cli(if msg.is_empty() {
                format!("git {} failed", args.first().unwrap_or(&""))
            } else {
                msg.to_owned()
            }))
        }
    }
}

/// Reload libgit2's cached index from disk. libgit2 keeps one index instance per
/// `Repository`, so a long-lived backend that runs alongside plain `git` (add /
/// checkout / reset) would otherwise read and write a stale snapshot. A forced
/// read is used, not a soft one: git can leave the index file "racily clean"
/// (rewritten within the same second) so a soft read may miss the change. This
/// is safe only where there are no unwritten in-memory index changes - i.e. at
/// the entry of an operation, NOT mid-merge/rebase where the in-memory index
/// holds conflict state not yet on disk.
fn sync_index(repo: &Repository) -> Result<(), GitError> {
    repo.index()?.read(true)?;
    Ok(())
}

impl GitBackend for Git2Backend {
    fn workdir(&self) -> &Path {
        &self.workdir
    }

    fn status(&self) -> Result<RepoStatus, GitError> {
        let mut repo = self.repo.lock().expect("repo mutex");
        // Reflect any staging done by plain `git` since the last refresh.
        sync_index(&repo)?;
        // stash_foreach needs &mut, so collect stashes before the shared reads.
        let stashes = collect_stashes(&mut repo);
        let entries = collect_entries(&repo)?;
        let recent = collect_recent(&repo);
        let head = collect_head(&repo, recent.first())?;

        let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        // Include untracked files (recursing into new directories) so a brand-new
        // file shows its all-added diff in the preview, like `git diff` with
        // --no-index would. Their contents are read and hashed in full, so past
        // EAGER_UNTRACKED files (an unignored build dir, say) they are left out
        // and `file_diff` loads one on demand.
        const EAGER_UNTRACKED: usize = 200;
        let mut wt_opts = DiffOptions::new();
        if entries.iter().filter(|e| e.is_untracked()).count() <= EAGER_UNTRACKED {
            wt_opts
                .include_untracked(true)
                .recurse_untracked_dirs(true)
                // Emit the file's lines as additions, not just a bare "new file" delta.
                .show_untracked_content(true);
        }
        let unstaged = extract_with_renames(repo.diff_index_to_workdir(None, Some(&mut wt_opts))?)?;
        let mut idx_opts = DiffOptions::new();
        let staged = extract_with_renames(repo.diff_tree_to_index(
            head_tree.as_ref(),
            None,
            Some(&mut idx_opts),
        )?)?;

        Ok(RepoStatus {
            head,
            entries,
            unstaged,
            staged,
            stashes,
            recent,
            state: repo_state(&repo),
        })
    }

    fn stash_push(&self, include_untracked: bool) -> Result<String, GitError> {
        self.logged("stash", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            let sig = repo.signature()?;
            repo.stash_save2(&sig, None, stash_flags(include_untracked))?;
            Ok(stash_saved_line(&repo))
        })
    }

    fn stash_push_message(
        &self,
        message: &str,
        include_untracked: bool,
    ) -> Result<String, GitError> {
        self.logged("stash", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            let sig = repo.signature()?;
            repo.stash_save2(&sig, Some(message), stash_flags(include_untracked))?;
            Ok(stash_saved_line(&repo))
        })
    }

    fn stash_pop(&self, index: usize) -> Result<(), GitError> {
        self.logged("stash pop", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            repo.stash_pop(index, None)?;
            Ok(())
        })
    }

    fn stash_apply(&self, index: usize) -> Result<(), GitError> {
        self.logged("stash apply", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            repo.stash_apply(index, None)?;
            Ok(())
        })
    }

    fn stash_drop(&self, index: usize) -> Result<(), GitError> {
        self.logged("stash drop", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            repo.stash_drop(index)?;
            Ok(())
        })
    }

    fn log(&self, opts: &crate::LogOptions) -> Result<Vec<crate::LogEntry>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let mut walk = repo.revwalk()?;
        let mut seeded = opts.all && walk.push_glob("refs/*").is_ok();
        for rev in &opts.revs {
            seeded |= push_rev(&repo, &mut walk, rev)?;
        }
        if !opts.all && opts.revs.is_empty() {
            seeded = walk.push_head().is_ok();
        }
        if !seeded {
            return Ok(Vec::new());
        }
        walk.set_sorting(git2::Sort::TIME)?;
        if opts.first_parent {
            walk.simplify_first_parent()?;
        }
        let grep = if opts.grep.is_empty() {
            None
        } else {
            let alt: Vec<String> = opts.grep.iter().map(|g| format!("(?:{g})")).collect();
            Some(
                regex::RegexBuilder::new(&alt.join("|"))
                    .case_insensitive(opts.grep_ignore_case)
                    .multi_line(true)
                    .build()
                    .map_err(|e| GitError::Other(format!("bad --grep pattern: {e}")))?,
            )
        };
        let mut follow = match (opts.follow, &opts.paths[..]) {
            (true, [path]) => Some(path.clone()),
            (true, _) => {
                return Err(GitError::Other(
                    "--follow requires exactly one pathspec".into(),
                ));
            }
            _ => None,
        };

        let unpushed = unpushed_oids(&repo);
        let decorations = log_decorations(&repo);
        let author_needle = opts.author.as_ref().map(|a| a.to_lowercase());
        let mut entries = Vec::new();
        let mut passed = 0usize;
        for oid in walk {
            if entries.len() >= opts.limit {
                break;
            }
            let commit = repo.find_commit(oid?)?;
            let when = commit.author().when().seconds();
            if opts.since.map(|s| when < s).unwrap_or(false)
                || opts.until.map(|u| when > u).unwrap_or(false)
            {
                continue;
            }
            if opts
                .merges
                .is_some_and(|m| m != (commit.parent_count() > 1))
            {
                continue;
            }
            if let Some(re) = &grep
                && !re.is_match(&String::from_utf8_lossy(commit.message_bytes()))
            {
                continue;
            }
            if let Some(path) = &mut follow {
                if !follow_path(&repo, &commit, path) {
                    continue;
                }
            } else if !opts.paths.is_empty()
                && !commit_touched_path(&repo, &commit, &opts.paths, opts.first_parent)
            {
                continue;
            }
            let author = commit.author();
            let author_name = author.name().unwrap_or("?").to_owned();
            if let Some(needle) = &author_needle {
                let email = author.email().unwrap_or("");
                if !author_name.to_lowercase().contains(needle)
                    && !email.to_lowercase().contains(needle)
                {
                    continue;
                }
            }
            // Skip the first `offset` matches for pagination, after filtering.
            passed += 1;
            if passed <= opts.offset {
                continue;
            }
            entries.push(crate::LogEntry {
                short_id: commit
                    .as_object()
                    .short_id()
                    .ok()
                    .and_then(|b| b.as_str().ok().map(str::to_owned))
                    .unwrap_or_default(),
                summary: commit
                    .summary()
                    .ok()
                    .flatten()
                    .unwrap_or_default()
                    .to_owned(),
                author: author_name,
                when: relative_age(commit.time().seconds(), now),
                oid: commit.id().to_string(),
                parents: commit.parent_ids().map(|id| id.to_string()).collect(),
                refs: decorations.get(&commit.id()).cloned().unwrap_or_default(),
                unpushed: unpushed.contains(&commit.id()),
            });
        }
        if opts.reverse {
            entries.reverse();
        }
        Ok(entries)
    }

    fn smartlog(&self) -> Result<Vec<crate::SmartlogEntry>, GitError> {
        use std::collections::HashMap;
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Map each local branch tip to its name(s), and collect the tips.
        let mut labels: HashMap<git2::Oid, Vec<String>> = HashMap::new();
        let mut tips: Vec<git2::Oid> = Vec::new();
        for branch in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch?;
            if let Some(oid) = branch.get().target() {
                if let Ok(Some(name)) = branch.name() {
                    labels.entry(oid).or_default().push(name.to_owned());
                }
                tips.push(oid);
            }
        }

        let trunk = detect_trunk(&repo);
        let head_oid = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .map(|c| c.id());

        let entry = |commit: &git2::Commit, is_head: bool, is_trunk: bool| crate::SmartlogEntry {
            short_id: commit
                .as_object()
                .short_id()
                .ok()
                .and_then(|b| b.as_str().ok().map(str::to_owned))
                .unwrap_or_default(),
            summary: commit
                .summary()
                .ok()
                .flatten()
                .unwrap_or_default()
                .to_owned(),
            author: commit.author().name().unwrap_or("?").to_owned(),
            when: relative_age(commit.time().seconds(), now),
            refs: labels.get(&commit.id()).cloned().unwrap_or_default(),
            is_head,
            is_trunk,
            change_id: commit.message().ok().and_then(crate::change_id::extract),
        };

        // Commits on local branches but not on the trunk: your draft work.
        let mut walk = repo.revwalk()?;
        walk.set_sorting(git2::Sort::TIME)?;
        for oid in &tips {
            let _ = walk.push(*oid);
        }
        if let Some(t) = trunk {
            let _ = walk.hide(t);
        }

        let mut out = Vec::new();
        for oid in walk {
            let oid = oid?;
            if Some(oid) == trunk {
                continue;
            }
            let commit = repo.find_commit(oid)?;
            out.push(entry(&commit, Some(oid) == head_oid, false));
            if out.len() >= 200 {
                break;
            }
        }
        // The trunk tip as the base your work diverges from.
        if let Some(t) = trunk {
            let commit = repo.find_commit(t)?;
            out.push(entry(&commit, Some(t) == head_oid, true));
        }
        Ok(out)
    }

    fn commit_details(&self, rev: &str) -> Result<crate::CommitDetails, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let commit = repo.revparse_single(rev)?.peel_to_commit()?;
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut opts = DiffOptions::new();
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;

        let mut refs = Vec::new();
        let mut merged = Vec::new();
        let mut contained = Vec::new();
        for r in repo.references()?.flatten() {
            let (Ok(name), Ok(tip)) = (r.shorthand(), r.peel_to_commit()) else {
                continue;
            };
            if r.is_remote() && name.ends_with("/HEAD") {
                continue;
            }
            let tip = tip.id();
            if tip == commit.id() && (r.is_branch() || r.is_remote() || r.is_tag()) {
                refs.push(name.to_owned());
            }
            if !r.is_branch() {
                continue;
            }
            if tip == commit.id() || repo.graph_descendant_of(commit.id(), tip)? {
                merged.push(name.to_owned());
            }
            if tip == commit.id() || repo.graph_descendant_of(tip, commit.id())? {
                contained.push(name.to_owned());
            }
        }
        let parents = commit
            .parents()
            .map(|p| {
                let short = p
                    .as_object()
                    .short_id()
                    .ok()
                    .and_then(|b| b.as_str().ok().map(str::to_owned))
                    .unwrap_or_default();
                (short, p.summary().ok().flatten().unwrap_or("").to_owned())
            })
            .collect();
        let follows = commit
            .as_object()
            .describe(git2::DescribeOptions::new().describe_tags())
            .and_then(|d| {
                d.format(Some(
                    git2::DescribeFormatOptions::new().always_use_long_format(true),
                ))
            })
            .ok()
            .and_then(|s| {
                // `<tag>-<distance>-g<hash>`; the tag itself may contain dashes.
                let mut it = s.rsplitn(3, '-');
                let _hash = it.next()?;
                let n = it.next()?.parse().ok()?;
                Some((it.next()?.to_owned(), n))
            });

        Ok(crate::CommitDetails {
            author_date: git_date(commit.author().when()),
            committer: commit.committer().name().unwrap_or("?").to_owned(),
            committer_email: commit.committer().email().unwrap_or("").to_owned(),
            commit_date: git_date(commit.committer().when()),
            parents,
            refs,
            merged,
            contained,
            follows,
            id: commit
                .as_object()
                .short_id()
                .ok()
                .and_then(|b| b.as_str().ok().map(str::to_owned))
                .unwrap_or_default(),
            full_id: commit.id().to_string(),
            author: commit.author().name().unwrap_or("?").to_owned(),
            email: commit.author().email().unwrap_or("").to_owned(),
            when: relative_age(commit.time().seconds(), now),
            message: commit.message().unwrap_or("").trim_end().to_owned(),
            files: extract_with_renames(diff)?,
        })
    }

    fn commit_overview(&self, rev: &str) -> Result<crate::CommitOverview, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let commit = repo.revparse_single(rev)?.peel_to_commit()?;
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut opts = DiffOptions::new();
        let mut diff =
            repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
        // Detect renames so a moved file lists once as "old -> new" with only its
        // real +/- counts, matching the working-tree and detail views.
        let mut fopts = DiffFindOptions::new();
        fopts.renames(true);
        diff.find_similar(Some(&mut fopts))?;

        let mut files = Vec::with_capacity(diff.deltas().len());
        for idx in 0..diff.deltas().len() {
            let delta = diff.get_delta(idx).expect("delta in range");
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let old_path = matches!(delta.status(), Delta::Renamed | Delta::Copied)
                .then(|| {
                    delta
                        .old_file()
                        .path()
                        .map(|p| p.to_string_lossy().into_owned())
                })
                .flatten()
                .filter(|old| *old != path);
            let binary = delta.flags().is_binary();
            let (additions, deletions) = if binary {
                (0, 0)
            } else {
                match Patch::from_diff(&diff, idx)? {
                    Some(patch) => {
                        let (_ctx, add, del) = patch.line_stats()?;
                        (add, del)
                    }
                    None => (0, 0),
                }
            };
            files.push(crate::CommitFile {
                path,
                old_path,
                additions,
                deletions,
                binary,
            });
        }

        Ok(crate::CommitOverview {
            id: commit
                .as_object()
                .short_id()
                .ok()
                .and_then(|b| b.as_str().ok().map(str::to_owned))
                .unwrap_or_default(),
            full_id: commit.id().to_string(),
            author: commit.author().name().unwrap_or("?").to_owned(),
            email: commit.author().email().unwrap_or("").to_owned(),
            when: relative_age(commit.time().seconds(), now),
            message: commit.message().unwrap_or("").trim_end().to_owned(),
            files,
        })
    }

    fn commit_file_diff(&self, rev: &str, path: &str) -> Result<Option<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let commit = repo.revparse_single(rev)?.peel_to_commit()?;
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut opts = DiffOptions::new();
        opts.pathspec(path);
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
        Ok(extract_with_renames(diff)?
            .into_iter()
            .find(|f| f.path == path))
    }

    fn diff_refs(&self, from: &str, to: &str) -> Result<Vec<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let from_tree = repo.revparse_single(from)?.peel_to_tree()?;
        let to_tree = repo.revparse_single(to)?.peel_to_tree()?;
        let mut opts = DiffOptions::new();
        let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), Some(&mut opts))?;
        extract_with_renames(diff)
    }

    fn diff(&self, spec: &crate::DiffSpec) -> Result<Vec<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut opts = DiffOptions::new();
        // libgit2 has no `.` pathspec; it means the whole tree, the same as none.
        for p in spec.paths.iter().filter(|p| *p != ".") {
            opts.pathspec(p);
        }
        if let Some(n) = spec.context {
            opts.context_lines(n);
        }
        opts.ignore_whitespace(spec.ignore_all_space)
            .ignore_whitespace_change(spec.ignore_space_change);
        let tree = |rev: &str| repo.revparse_single(rev).and_then(|o| o.peel_to_tree());
        let diff = match (&spec.from, &spec.to) {
            (Some(range), None) if range.contains("..") => {
                let (old, new) = range_trees(&repo, range)?;
                repo.diff_tree_to_tree(Some(&old), Some(&new), Some(&mut opts))?
            }
            (from, Some(to)) => {
                let old = from.as_deref().map(tree).transpose()?;
                repo.diff_tree_to_tree(old.as_ref(), Some(&tree(to)?), Some(&mut opts))?
            }
            (Some(from), None) if spec.cached => {
                repo.diff_tree_to_index(Some(&tree(from)?), None, Some(&mut opts))?
            }
            (Some(from), None) => {
                repo.diff_tree_to_workdir_with_index(Some(&tree(from)?), Some(&mut opts))?
            }
            (None, None) if spec.cached => {
                let head = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
                repo.diff_tree_to_index(head.as_ref(), None, Some(&mut opts))?
            }
            (None, None) => repo.diff_index_to_workdir(None, Some(&mut opts))?,
        };
        extract_with_renames(diff)
    }

    fn file_diff(&self, path: &str, staged: bool) -> Result<Option<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut opts = DiffOptions::new();
        opts.pathspec(path);
        let diff = if staged {
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?
        } else {
            // Include untracked files, with their content, so a brand-new file
            // shows as an all-added diff rather than a bare "new file" delta.
            opts.include_untracked(true)
                .recurse_untracked_dirs(true)
                .show_untracked_content(true);
            repo.diff_index_to_workdir(None, Some(&mut opts))?
        };
        Ok(extract_with_renames(diff)?
            .into_iter()
            .find(|f| f.path == path))
    }

    fn blame(&self, path: &str) -> Result<Vec<crate::BlameLine>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let content = std::fs::read_to_string(self.workdir.join(path))?;
        // libgit2 blames committed history only, so a file that exists solely in
        // the index or working tree (newly added, never committed) has nothing to
        // blame and errors with "path does not exist". Match `git blame` and mark
        // every line not-yet-committed rather than failing.
        let blame = repo.blame_file(Path::new(path), None).ok();
        Ok(blame_lines(blame.as_ref(), &content))
    }

    fn blame_at(&self, rev: &str, path: &str) -> Result<Vec<crate::BlameLine>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let commit = repo.revparse_single(rev)?.peel_to_commit()?;
        let blob = commit
            .tree()?
            .get_path(Path::new(path))?
            .to_object(&repo)?
            .peel_to_blob()?;
        let mut opts = git2::BlameOptions::new();
        opts.newest_commit(commit.id());
        let blame = repo.blame_file(Path::new(path), Some(&mut opts))?;
        Ok(blame_lines(
            Some(&blame),
            &String::from_utf8_lossy(blob.content()),
        ))
    }

    fn ignored(&self) -> Result<Vec<StatusEntry>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut opts = StatusOptions::new();
        opts.include_ignored(true).include_untracked(false);
        Ok(repo
            .statuses(Some(&mut opts))?
            .iter()
            .filter(|e| e.status().contains(Status::IGNORED))
            .filter_map(|e| e.path().ok().map(str::to_owned))
            .map(|path| StatusEntry {
                path,
                orig_path: None,
                index: StatusCode::Unmodified,
                worktree: StatusCode::Ignored,
            })
            .collect())
    }

    fn stage_all(&self) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut index = repo.index()?;
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        Ok(())
    }

    fn unstage_all(&self) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        match repo.head() {
            Ok(head_ref) => {
                let target = head_ref.peel(ObjectType::Commit)?;
                repo.reset_default(Some(&target), ["*"])?;
            }
            Err(_) => {
                // Unborn branch: emptying the index unstages everything.
                let mut index = repo.index()?;
                index.clear()?;
                index.write()?;
            }
        }
        Ok(())
    }

    fn stage_file(&self, path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        no_match(&repo, path)?;
        let mut index = repo.index()?;
        if self.workdir.join(path).is_file() {
            index.add_path(Path::new(path))?;
        } else {
            // A folder, glob or deleted path: add what is there, drop what is gone.
            index.add_all([path], git2::IndexAddOption::DEFAULT, None)?;
            index.update_all([path], None)?;
        }
        index.write()?;
        Ok(())
    }

    fn unstage_file(&self, path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        no_match(&repo, path)?;
        match repo.head() {
            Ok(head_ref) => {
                let target = head_ref.peel(ObjectType::Commit)?;
                repo.reset_default(Some(&target), [path])?;
            }
            Err(_) => {
                // Unborn branch: nothing is committed, so drop the index entry.
                let mut index = repo.index()?;
                index.remove_path(Path::new(path))?;
                index.write()?;
            }
        }
        Ok(())
    }

    fn stage_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut dopts = DiffOptions::new();
        dopts.pathspec(path);
        let diff = repo.diff_index_to_workdir(None, Some(&mut dopts))?;
        apply_one_hunk(&repo, &diff, path, new_start)
    }

    fn unstage_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let head_tree = repo.head()?.peel_to_tree()?;
        let mut dopts = DiffOptions::new();
        dopts.pathspec(path);
        let staged = repo.diff_tree_to_index(Some(&head_tree), None, Some(&mut dopts))?;
        // libgit2 cannot reverse-apply, so synthesize the reversed hunk patch.
        let patch = reverse_hunk_patch(&staged, path, new_start)?;
        let reversed = Diff::from_buffer(patch.as_bytes())?;
        repo.apply(&reversed, ApplyLocation::Index, None)?;
        Ok(())
    }

    fn stage_lines(&self, path: &str, new_start: u32, lines: &[usize]) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut dopts = DiffOptions::new();
        dopts.pathspec(path);
        let diff = repo.diff_index_to_workdir(None, Some(&mut dopts))?;
        let patch = partial_hunk_patch(&diff, path, new_start, lines, false)?;
        repo.apply(
            &Diff::from_buffer(patch.as_bytes())?,
            ApplyLocation::Index,
            None,
        )?;
        Ok(())
    }

    fn unstage_lines(&self, path: &str, new_start: u32, lines: &[usize]) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let head_tree = repo.head()?.peel_to_tree()?;
        let mut dopts = DiffOptions::new();
        dopts.pathspec(path);
        let diff = repo.diff_tree_to_index(Some(&head_tree), None, Some(&mut dopts))?;
        let patch = partial_hunk_patch(&diff, path, new_start, lines, true)?;
        repo.apply(
            &Diff::from_buffer(patch.as_bytes())?,
            ApplyLocation::Index,
            None,
        )?;
        Ok(())
    }

    fn refs(&self) -> Result<Vec<crate::RefEntry>, GitError> {
        use crate::RefKind;
        let repo = self.repo.lock().expect("repo mutex");
        let mut out = Vec::new();

        for (kind, ty) in [
            (RefKind::Local, BranchType::Local),
            (RefKind::Remote, BranchType::Remote),
        ] {
            for branch in repo.branches(Some(ty))? {
                let (branch, _) = branch?;
                let is_head = branch.is_head();
                if let Some(name) = branch.name()?.map(str::to_owned) {
                    out.push(crate::RefEntry {
                        name,
                        kind,
                        is_head,
                    });
                }
            }
        }
        let tags = repo.tag_names(None)?;
        for tag in tags.iter().filter_map(|t| t.ok().flatten()) {
            out.push(crate::RefEntry {
                name: tag.to_owned(),
                kind: RefKind::Tag,
                is_head: false,
            });
        }
        Ok(out)
    }

    fn list_tree(&self, rev: &str, path: &str) -> Result<Vec<crate::TreeEntry>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let commit = repo.revparse_single(rev)?.peel_to_commit()?;
        let root = commit.tree()?;
        let tree = if path.is_empty() {
            root
        } else {
            root.get_path(std::path::Path::new(path))?
                .to_object(&repo)?
                .peel_to_tree()?
        };
        let mut out = Vec::with_capacity(tree.len());
        for e in tree.iter() {
            let Ok(name) = e.name() else { continue };
            let is_dir = e.kind() == Some(git2::ObjectType::Tree);
            let full = if path.is_empty() {
                name.to_owned()
            } else {
                format!("{path}/{name}")
            };
            let size = if is_dir {
                0
            } else {
                e.to_object(&repo)
                    .ok()
                    .and_then(|o| o.into_blob().ok())
                    .map(|b| b.size() as u64)
                    .unwrap_or(0)
            };
            out.push(crate::TreeEntry {
                name: name.to_owned(),
                path: full,
                is_dir,
                size,
            });
        }
        // Directories first, then files, each alphabetical: the settled listing.
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    fn read_blob(&self, rev: &str, path: &str) -> Result<crate::Blob, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        // An empty rev reads the index, as git's `:path` does.
        let blob = if rev.is_empty() {
            sync_index(&repo)?;
            let entry = repo
                .index()?
                .get_path(Path::new(path), 0)
                .ok_or_else(|| GitError::Other(format!("path '{path}' is not in the index")))?;
            repo.find_blob(entry.id)?
        } else {
            repo.revparse_single(&format!("{rev}:{path}"))?
                .peel_to_blob()?
        };
        let content = blob.content();
        let is_binary = blob.is_binary();
        Ok(crate::Blob {
            path: path.to_owned(),
            size: content.len() as u64,
            is_binary,
            text: (!is_binary).then(|| String::from_utf8_lossy(content).into_owned()),
        })
    }

    fn tree_last_commits(
        &self,
        rev: &str,
        paths: &[String],
    ) -> Result<std::collections::HashMap<String, crate::LastCommit>, GitError> {
        use std::collections::{HashMap, HashSet};
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let start = repo.revparse_single(rev)?.peel_to_commit()?.id();
        let mut walk = repo.revwalk()?;
        walk.set_sorting(git2::Sort::TIME)?;
        walk.push(start)?;

        let mut want: HashSet<String> = paths.iter().cloned().collect();
        let mut found: HashMap<String, crate::LastCommit> = HashMap::new();
        // Bound the walk so a huge history cannot stall a page; unresolved paths
        // simply show no latest-commit rather than blocking.
        let mut budget = 2000usize;
        for oid in walk {
            if want.is_empty() || budget == 0 {
                break;
            }
            budget -= 1;
            let commit = repo.find_commit(oid?)?;
            let tree = commit.tree()?;
            let parent_trees: Vec<git2::Tree> =
                commit.parents().filter_map(|p| p.tree().ok()).collect();
            // A commit "touches" a path when its object id there differs from the
            // path in every parent. For a merge that only carried a side's version
            // through unchanged the ids match a parent, so it is simplified away -
            // matching `git log -- path` rather than flagging the merge.
            let oid_at =
                |t: &git2::Tree, p: &str| t.get_path(std::path::Path::new(p)).ok().map(|e| e.id());
            let matched: Vec<String> = want
                .iter()
                .filter(|p| {
                    let cur = oid_at(&tree, p);
                    if parent_trees.is_empty() {
                        cur.is_some()
                    } else {
                        parent_trees.iter().all(|pt| oid_at(pt, p) != cur)
                    }
                })
                .cloned()
                .collect();
            if !matched.is_empty() {
                let last = crate::LastCommit {
                    short_id: commit
                        .as_object()
                        .short_id()
                        .ok()
                        .and_then(|b| b.as_str().ok().map(str::to_owned))
                        .unwrap_or_default(),
                    summary: commit
                        .summary()
                        .ok()
                        .flatten()
                        .unwrap_or_default()
                        .to_owned(),
                    when: relative_age(commit.time().seconds(), now),
                };
                for p in matched {
                    found.insert(p.clone(), last.clone());
                    want.remove(&p);
                }
            }
        }
        Ok(found)
    }

    fn list_files(&self, rev: &str) -> Result<Vec<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let tree = repo.revparse_single(rev)?.peel_to_commit()?.tree()?;
        let mut out = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob)
                && let Ok(name) = entry.name()
            {
                // `root` is the containing dir with a trailing slash, or empty.
                out.push(format!("{root}{name}"));
            }
            git2::TreeWalkResult::Ok
        })?;
        Ok(out)
    }

    fn grep(&self, pattern: &str) -> Result<Vec<crate::GrepMatch>, GitError> {
        let q = crate::GrepQuery {
            pattern: pattern.to_owned(),
            regex: false,
            path: None,
            exts: Vec::new(),
        };
        self.grep_query(&q)
    }

    fn grep_query(&self, q: &crate::GrepQuery) -> Result<Vec<crate::GrepMatch>, GitError> {
        use grep::regex::RegexMatcherBuilder;
        use grep::searcher::Searcher;
        use grep::searcher::sinks::UTF8;
        use rayon::prelude::*;

        const MAX_MATCHES: usize = 300;
        const MAX_PER_FILE: u64 = 50;
        if q.pattern.is_empty() {
            return Ok(Vec::new());
        }
        let workdir = self.workdir.clone();
        // Literal, case-insensitive by default: escape the pattern so it
        // matches text, not as a regex, unless the caller asked for regex mode.
        let pattern = if q.regex {
            q.pattern.clone()
        } else {
            regex::escape(&q.pattern)
        };
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(true)
            .build(&pattern)
            .map_err(|e| GitError::Other(e.to_string()))?;

        let path_filter = q.path.as_ref().map(|p| p.to_lowercase());

        // Collect files once (gitignore-aware), then filter by path/extension
        // before searching them in parallel.
        let files: Vec<(std::path::PathBuf, String)> = ignore::WalkBuilder::new(&workdir)
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(ignore::DirEntry::into_path)
            .filter_map(|path| {
                let rel = path
                    .strip_prefix(&workdir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                if let Some(needle) = &path_filter
                    && !rel.to_lowercase().contains(needle.as_str())
                {
                    return None;
                }
                if !q.exts.is_empty() {
                    let ext = rel
                        .rsplit_once('.')
                        .map(|(_, ext)| ext.to_lowercase())
                        .unwrap_or_default();
                    if !q.exts.iter().any(|e| e == &ext) {
                        return None;
                    }
                }
                Some((path, rel))
            })
            .collect();

        let mut out: Vec<crate::GrepMatch> = files
            .par_iter()
            .flat_map_iter(|(path, rel)| {
                let mut matches = Vec::new();
                let _ = Searcher::new().search_path(
                    &matcher,
                    path,
                    UTF8(|lnum, line| {
                        matches.push(crate::GrepMatch {
                            path: rel.clone(),
                            line: lnum as usize,
                            text: line.trim_end().chars().take(300).collect(),
                        });
                        Ok(matches.len() < MAX_PER_FILE as usize)
                    }),
                );
                matches.into_iter()
            })
            .collect();

        // Stable order (paths walk in arbitrary parallel order), then cap.
        out.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        out.truncate(MAX_MATCHES);
        Ok(out)
    }

    fn rev_parse(&self, rev: &str) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        Ok(repo
            .revparse_single(rev)?
            .peel_to_commit()?
            .id()
            .to_string())
    }

    fn contributors(&self) -> Result<Vec<(String, String, usize)>, GitError> {
        use std::collections::HashMap;
        let repo = self.repo.lock().expect("repo mutex");
        let mut walk = repo.revwalk()?;
        if walk.push_head().is_err() {
            return Ok(Vec::new());
        }
        // name -> (email of first sighting, commit count).
        let mut by_name: HashMap<String, (String, usize)> = HashMap::new();
        // Bound the walk so the sidebar cannot stall on a huge history.
        for oid in walk.flatten().take(5000) {
            if let Ok(commit) = repo.find_commit(oid) {
                let author = commit.author();
                let name = author.name().unwrap_or("?").to_owned();
                let email = author.email().unwrap_or("").to_owned();
                let entry = by_name.entry(name).or_insert((email, 0));
                entry.1 += 1;
            }
        }
        let mut out: Vec<(String, String, usize)> = by_name
            .into_iter()
            .map(|(name, (email, count))| (name, email, count))
            .collect();
        out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        out.truncate(8);
        Ok(out)
    }

    fn latest_tag(&self) -> Result<Option<crate::TagInfo>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut best: Option<(i64, crate::TagInfo)> = None;
        let tags = repo.tag_names(None)?;
        for name in tags.iter().filter_map(|t| t.ok().flatten()) {
            let Ok(obj) = repo.revparse_single(name) else {
                continue;
            };
            let Ok(commit) = obj.peel_to_commit() else {
                continue;
            };
            let t = commit.time().seconds();
            if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
                let message = commit
                    .summary()
                    .ok()
                    .flatten()
                    .unwrap_or_default()
                    .to_owned();
                best = Some((
                    t,
                    crate::TagInfo {
                        name: name.to_owned(),
                        when: relative_age(t, now),
                        message,
                    },
                ));
            }
        }
        Ok(best.map(|(_, ti)| ti))
    }

    fn all_tags(&self) -> Result<Vec<crate::TagInfo>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut out: Vec<(i64, crate::TagInfo)> = Vec::new();
        for name in repo
            .tag_names(None)?
            .iter()
            .filter_map(|t| t.ok().flatten())
        {
            let Ok(obj) = repo.revparse_single(name) else {
                continue;
            };
            let Ok(commit) = obj.peel_to_commit() else {
                continue;
            };
            let t = commit.time().seconds();
            // Prefer an annotated tag's own message; else the commit summary.
            let tag_msg = obj
                .as_tag()
                .and_then(|tg| tg.message().ok().flatten())
                // Drop a trailing PGP/SSH signature block from a signed tag.
                .map(|m| m.split("-----BEGIN").next().unwrap_or(m).trim().to_owned())
                .filter(|m| !m.is_empty());
            let message = tag_msg
                .or_else(|| commit.summary().ok().flatten().map(|s| s.to_owned()))
                .unwrap_or_default();
            out.push((
                t,
                crate::TagInfo {
                    name: name.to_owned(),
                    when: relative_age(t, now),
                    message,
                },
            ));
        }
        out.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        Ok(out.into_iter().map(|(_, ti)| ti).collect())
    }

    fn archive_targz(&self, rev: &str) -> Result<Vec<u8>, GitError> {
        self.archive(rev, "tgz", "", &[])
    }

    fn checkout_detached(&self, rev: &str) -> Result<(), GitError> {
        self.logged("checkout", || {
            let repo = self.repo.lock().expect("repo mutex");
            let commit = repo.revparse_single(rev)?.peel_to_commit()?;
            repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe()))?;
            repo.set_head_detached(commit.id())?;
            Ok(())
        })
    }

    fn commits_between(&self, base: &str) -> Result<Vec<(String, String)>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let base_oid = repo.revparse_single(base)?.peel_to_commit()?.id();
        let mut walk = repo.revwalk()?;
        walk.push_head()?;
        walk.hide(base_oid)?;
        let mut out = Vec::new();
        for oid in walk {
            let commit = repo.find_commit(oid?)?;
            let short = commit
                .as_object()
                .short_id()
                .ok()
                .and_then(|b| b.as_str().ok().map(str::to_owned))
                .unwrap_or_default();
            let subject = commit
                .summary()
                .ok()
                .flatten()
                .unwrap_or_default()
                .to_owned();
            out.push((short, subject));
        }
        out.reverse(); // oldest first, matching a rebase todo
        Ok(out)
    }

    fn file_activity(
        &self,
        max_commits: usize,
    ) -> Result<std::collections::HashMap<String, crate::FileActivity>, GitError> {
        use std::collections::HashMap;
        let repo = self.repo.lock().expect("repo mutex");
        let mut acc: HashMap<String, crate::FileActivity> = HashMap::new();
        // First-parent walk: on a merge, count the mainline change once rather
        // than re-counting every side-branch commit the merge brought in.
        let mut commit = match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
            Some(c) => c,
            None => return Ok(acc),
        };
        let mut opts = DiffOptions::new();
        for _ in 0..max_commits {
            let when = commit.time().seconds();
            let tree = commit.tree()?;
            let parent = commit.parent(0).ok();
            let parent_tree = match parent.as_ref() {
                Some(p) => Some(p.tree()?),
                None => None,
            };
            let diff =
                repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
            for i in 0..diff.deltas().len() {
                let Some(delta) = diff.get_delta(i) else {
                    continue;
                };
                let Some(path) = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|p| p.to_string_lossy().into_owned())
                else {
                    continue;
                };
                let e = acc.entry(path).or_insert(crate::FileActivity {
                    commits: 0,
                    last_epoch: when,
                });
                e.commits += 1;
                if when > e.last_epoch {
                    e.last_epoch = when;
                }
            }
            match parent {
                Some(p) => commit = p,
                None => break,
            }
        }
        Ok(acc)
    }

    fn rebase_onto(&self, rev: &str, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
        self.logged("rebase", || {
            let repo = self.repo.lock().expect("repo mutex");
            let target = repo.revparse_single(rev)?.peel_to_commit()?;
            let upstream = repo.find_annotated_commit(target.id())?;
            run_rebase(&repo, &upstream, None, report)
        })
    }

    fn rebase_range(
        &self,
        upstream: &str,
        onto: &str,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        self.logged("restack", || {
            let repo = self.repo.lock().expect("repo mutex");
            let upstream_oid = repo.revparse_single(upstream)?.peel_to_commit()?.id();
            let onto_oid = repo.revparse_single(onto)?.peel_to_commit()?.id();
            let upstream = repo.find_annotated_commit(upstream_oid)?;
            let onto = repo.find_annotated_commit(onto_oid)?;
            run_rebase(&repo, &upstream, Some(&onto), report)
        })
    }

    fn branch_tip(&self, name: &str) -> Result<Option<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        match repo.find_branch(name, BranchType::Local) {
            Ok(branch) => Ok(branch.get().target().map(|oid| oid.to_string())),
            Err(e) if e.code() == ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn rebase_abort(&self) -> Result<(), GitError> {
        // Shell out like continue/skip: `git rebase --abort` restores the
        // pre-rebase state for both libgit2-created and CLI-started (interactive)
        // rebases, whereas libgit2's open_rebase cannot drive a rebase the git
        // CLI began ("interactive rebase is not supported").
        self.run_git(&["rebase", "--abort"], &[]).map(drop)
    }

    fn rebase_continue(&self) -> Result<(), GitError> {
        // Non-interactive: accept the existing message if git wants an editor.
        self.run_git(&["rebase", "--continue"], &[("GIT_EDITOR", "true")])
            .map(drop)
    }

    fn rebase_skip(&self) -> Result<(), GitError> {
        self.run_git(&["rebase", "--skip"], &[]).map(drop)
    }

    fn rebase_with(
        &self,
        upstream: Option<&str>,
        opts: &crate::RebaseOptions,
    ) -> Result<(), GitError> {
        // libgit2's rebase has no todo list, so these run on git's sequencer.
        let mut args = vec!["rebase"];
        if opts.interactive || opts.autosquash {
            args.push("-i");
        }
        if opts.autosquash {
            args.push("--autosquash");
        }
        for cmd in &opts.exec {
            args.extend(["--exec", cmd]);
        }
        if opts.root {
            args.push("--root");
        }
        if opts.update_refs {
            args.push("--update-refs");
        }
        if let Some(side) = &opts.strategy_option {
            args.extend(["-X", side]);
        }
        if let Some(onto) = &opts.onto {
            args.extend(["--onto", onto]);
        }
        args.extend(upstream);
        self.logged("rebase", || {
            let result = if opts.interactive {
                // Inherit stdio so git can open the todo editor on the terminal.
                let status = std::process::Command::new("git")
                    .args(&args)
                    .current_dir(&self.workdir)
                    .status()
                    .map_err(|e| GitError::Cli(format!("could not run git: {e}")))?;
                if status.success() {
                    Ok(())
                } else {
                    Err(GitError::Cli(
                        "interactive rebase failed or was aborted".into(),
                    ))
                }
            } else {
                // The todo list is taken as generated; commit messages as they are.
                self.run_git(
                    &args,
                    &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
                )
                .map(drop)
            };
            let stopped =
                self.repo.lock().expect("repo mutex").state() != git2::RepositoryState::Clean;
            match result {
                Err(GitError::Cli(msg)) if stopped => {
                    let why: Vec<&str> = msg.lines().filter(|l| !l.starts_with("hint:")).collect();
                    Err(GitError::Conflict(format!(
                        "{}; resolve, then run `rgit rebase --continue` (or --skip / --abort)",
                        why.join("; ")
                    )))
                }
                other => other,
            }
        })
    }

    fn undo(&self) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        crate::oplog::undo(&repo)
    }

    fn redo(&self) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        crate::oplog::redo(&repo)
    }

    fn oplog(&self) -> Result<Vec<crate::OpLogEntry>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        crate::oplog::entries(&repo)
    }

    fn absorb(&self) -> Result<String, GitError> {
        use std::collections::{HashMap, HashSet};

        self.logged("absorb", || {
            // Phase 1 (read): for each changed hunk, blame its lines to find the
            // local commit that last touched them; group hunks by that commit.
            let (base, order, groups, hunk_count) = {
                let repo = self.repo.lock().expect("repo mutex");
                let head = repo.head()?.peel_to_commit()?;

                // Refuse if there are staged changes: a fixup would capture them too.
                let head_tree = head.tree()?;
                if repo
                    .diff_tree_to_index(Some(&head_tree), None, None)?
                    .deltas()
                    .len()
                    > 0
                {
                    return Err(GitError::Conflict(
                        "you have staged changes; commit or unstage them before absorbing".into(),
                    ));
                }

                let trunk = detect_trunk(&repo).ok_or_else(|| {
                    GitError::Conflict("no trunk (main/master) to absorb against".into())
                })?;
                let base = repo.merge_base(head.id(), trunk).unwrap_or(trunk);

                // Local commits (base..HEAD) - the mutable absorb targets.
                let mut walk = repo.revwalk()?;
                walk.push(head.id())?;
                let _ = walk.hide(base);
                let mutable: HashSet<git2::Oid> = walk.flatten().collect();
                if mutable.is_empty() {
                    return Err(GitError::Conflict(
                        "no local commits since the trunk to absorb into".into(),
                    ));
                }

                // Map each modified hunk to the commit its lines were last touched
                // in, grouping hunks per target and keeping first-seen order. Zero
                // context keeps distinct changes as separate hunks (so nearby edits
                // owned by different commits do not merge into one).
                let mut dopts = DiffOptions::new();
                dopts.context_lines(0);
                let diff = repo.diff_index_to_workdir(None, Some(&mut dopts))?;
                let ndeltas = diff.deltas().count();
                let mut order: Vec<git2::Oid> = Vec::new();
                let mut groups: HashMap<git2::Oid, Vec<(String, u32)>> = HashMap::new();
                let mut hunk_count = 0usize;
                for i in 0..ndeltas {
                    let Some(patch) = git2::Patch::from_diff(&diff, i)? else {
                        continue;
                    };
                    if patch.delta().status() != git2::Delta::Modified {
                        continue;
                    }
                    let Some(path) = patch.delta().new_file().path() else {
                        continue;
                    };
                    let path = path.to_string_lossy().into_owned();
                    let Ok(blame) = repo.blame_file(Path::new(&path), None) else {
                        continue;
                    };
                    for h in 0..patch.num_hunks() {
                        let (hunk, num_lines) = patch.hunk(h)?;
                        // Blame the first line the hunk deletes (the changed old
                        // line), not the hunk's leading context, which the base
                        // commit usually owns. Pure additions fall back to the
                        // hunk anchor.
                        let mut line = hunk.old_start().max(1) as usize;
                        for l in 0..num_lines {
                            if let Ok(dl) = patch.line_in_hunk(h, l)
                                && dl.origin() == '-'
                                && let Some(no) = dl.old_lineno()
                            {
                                line = no as usize;
                                break;
                            }
                        }
                        let Some(bhunk) = blame.get_line(line) else {
                            continue;
                        };
                        let target = bhunk.final_commit_id();
                        if !mutable.contains(&target) {
                            continue;
                        }
                        if !groups.contains_key(&target) {
                            order.push(target);
                        }
                        groups
                            .entry(target)
                            .or_default()
                            .push((path.clone(), hunk.new_start()));
                        hunk_count += 1;
                    }
                }
                (base, order, groups, hunk_count)
            };

            if order.is_empty() {
                return Ok("nothing to absorb (changes do not map to local commits)".to_owned());
            }

            // Phase 2 (write): one fixup commit per target, containing only that
            // target's hunks (staged one at a time from a freshly recomputed diff).
            {
                let repo = self.repo.lock().expect("repo mutex");
                let sig = repo
                    .signature()
                    .or_else(|_| git2::Signature::now("rgit", "rgit@localhost"))?;
                for target in &order {
                    for (path, new_start) in &groups[target] {
                        let mut dopts = DiffOptions::new();
                        dopts.pathspec(path);
                        dopts.context_lines(0);
                        let diff = repo.diff_index_to_workdir(None, Some(&mut dopts))?;
                        apply_one_hunk(&repo, &diff, path, *new_start)?;
                    }
                    let subject = repo
                        .find_commit(*target)?
                        .summary()
                        .ok()
                        .flatten()
                        .unwrap_or("")
                        .to_owned();
                    let tree = repo.find_tree(repo.index()?.write_tree()?)?;
                    let head = repo.head()?.peel_to_commit()?;
                    repo.commit(
                        Some("HEAD"),
                        &sig,
                        &sig,
                        &format!("fixup! {subject}"),
                        &tree,
                        &[&head],
                    )?;
                }
            }
            // If autosquash conflicts, abort so the repo is not left mid-rebase with
            // the synthetic fixup commits; the op-log snapshot then fully restores.
            if let Err(e) = self.run_git(
                &["rebase", "-i", "--autosquash", &base.to_string()],
                &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
            ) {
                let _ = self.run_git(&["rebase", "--abort"], &[]);
                return Err(e);
            }
            Ok(format!(
                "absorbed {hunk_count} hunk(s) into {} commit(s)",
                order.len()
            ))
        })
    }

    fn bisect(&self, args: &[String]) -> Result<String, GitError> {
        let mut argv = vec!["bisect"];
        argv.extend(args.iter().map(String::as_str));
        self.run_git(&argv, &[])
    }

    fn config_entries(
        &self,
        scope: ConfigScope,
        name: Option<&str>,
    ) -> Result<Vec<(String, String)>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let config = open_config(&repo, scope, false)?;
        let mut out = Vec::new();
        let entries = match name {
            Some(name) => config.multivar(name, None),
            None => config.entries(None),
        };
        match entries {
            Ok(entries) => entries.for_each(|e| {
                out.push((
                    String::from_utf8_lossy(e.name_bytes()).into_owned(),
                    String::from_utf8_lossy(e.value_bytes()).into_owned(),
                ))
            })?,
            Err(e) if e.code() == ErrorCode::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(out)
    }

    fn config_write(
        &self,
        scope: ConfigScope,
        name: &str,
        value: &str,
        add: bool,
    ) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut config = open_config(&repo, scope, true)?;
        if add {
            // A pattern no value matches appends instead of replacing.
            config.set_multivar(name, "$^", value)?;
        } else {
            config.set_str(name, value)?;
        }
        Ok(())
    }

    fn config_unset(&self, scope: ConfigScope, name: &str, all: bool) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut config = open_config(&repo, scope, true)?;
        if all {
            config.remove_multivar(name, ".*")?;
        } else {
            config.remove(name)?;
        }
        Ok(())
    }

    fn apply_patch(
        &self,
        patch: &[u8],
        cached: bool,
        index: bool,
        reverse: bool,
        check: bool,
    ) -> Result<(), GitError> {
        let reversed;
        let patch = if reverse {
            reversed = reverse_patch(&String::from_utf8_lossy(patch))?;
            reversed.as_bytes()
        } else {
            patch
        };
        let diff = Diff::from_buffer(patch)?;
        let location = if cached {
            ApplyLocation::Index
        } else if index {
            ApplyLocation::Both
        } else {
            ApplyLocation::WorkDir
        };
        let run = || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let mut opts = ApplyOptions::new();
            opts.check(check);
            repo.apply(&diff, location, Some(&mut opts))?;
            Ok(())
        };
        if check {
            run()
        } else {
            self.logged("apply", run)
        }
    }

    fn patch_stat(&self, patch: &[u8]) -> Result<String, GitError> {
        let stats = Diff::from_buffer(patch)?.stats()?;
        let buf = stats.to_buf(git2::DiffStatsFormat::FULL, 80)?;
        Ok(String::from_utf8_lossy(&buf).trim_end().to_owned())
    }

    fn notes(&self, notes_ref: Option<&str>) -> Result<Vec<(String, String)>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let notes = match repo.notes(notes_ref) {
            Ok(notes) => notes,
            Err(e) if e.code() == ErrorCode::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = notes
            .map(|n| n.map(|(note, obj)| (note.to_string(), obj.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_by(|a, b| a.1.cmp(&b.1));
        Ok(out)
    }

    fn note_show(&self, notes_ref: Option<&str>, rev: &str) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let oid = repo.revparse_single(rev)?.id();
        match repo.find_note(notes_ref, oid) {
            Ok(note) => Ok(String::from_utf8_lossy(note.message_bytes())
                .trim_end()
                .to_owned()),
            Err(e) if e.code() == ErrorCode::NotFound => {
                Err(GitError::Other(format!("no note found for object {oid}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn note_add(
        &self,
        notes_ref: Option<&str>,
        rev: &str,
        message: &str,
        force: bool,
        append: bool,
    ) -> Result<(), GitError> {
        self.logged("notes", || {
            let repo = self.repo.lock().expect("repo mutex");
            let oid = repo.revparse_single(rev)?.id();
            let sig = repo.signature()?;
            let old = repo
                .find_note(notes_ref, oid)
                .ok()
                .map(|n| String::from_utf8_lossy(n.message_bytes()).into_owned());
            let message = format!("{}\n", message.trim_end());
            let message = match old {
                Some(old) if append => format!("{}\n\n{message}", old.trim_end()),
                Some(_) if !force => {
                    return Err(GitError::Other(format!(
                        "object {oid} already has a note; use --force to overwrite it"
                    )));
                }
                _ => message,
            };
            repo.note(&sig, &sig, notes_ref, oid, &message, true)?;
            Ok(())
        })
    }

    fn note_remove(&self, notes_ref: Option<&str>, rev: &str) -> Result<(), GitError> {
        self.logged("notes", || {
            let repo = self.repo.lock().expect("repo mutex");
            let oid = repo.revparse_single(rev)?.id();
            let sig = repo.signature()?;
            repo.note_delete(oid, notes_ref, &sig, &sig)
                .map_err(|e| match e.code() {
                    ErrorCode::NotFound => GitError::Other(format!("object {oid} has no note")),
                    _ => e.into(),
                })
        })
    }

    fn update_ref(
        &self,
        name: &str,
        new: Option<&str>,
        old: Option<&str>,
        no_deref: bool,
        message: Option<&str>,
    ) -> Result<(), GitError> {
        self.logged("update-ref", || {
            let repo = self.repo.lock().expect("repo mutex");
            let mut name = name.to_owned();
            while !no_deref
                && let Some(target) = repo
                    .find_reference(&name)
                    .ok()
                    .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
            {
                name = target;
            }
            if let Some(old) = old {
                let want = if old.chars().all(|c| c == '0') {
                    None
                } else {
                    Some(repo.revparse_single(old)?.id())
                };
                let current = repo.refname_to_id(&name).ok();
                if current != want {
                    return Err(GitError::Other(format!(
                        "{name} is at {}, not {old}",
                        current.map_or("nothing".to_owned(), |c| c.to_string())
                    )));
                }
            }
            match new {
                Some(new) => {
                    let id = repo.revparse_single(new)?.id();
                    repo.reference(&name, id, true, message.unwrap_or("update-ref"))?;
                }
                None => repo.find_reference(&name)?.delete()?,
            }
            Ok(())
        })
    }

    fn hash_object(&self, kind: &str, data: &[u8], write: bool) -> Result<String, GitError> {
        let kind = ObjectType::from_str(kind)
            .ok_or_else(|| GitError::Other(format!("invalid object type {kind:?}")))?;
        let oid = if write {
            let repo = self.repo.lock().expect("repo mutex");
            repo.odb()?.write(kind, data)?
        } else {
            Oid::hash_object(kind, data)?
        };
        Ok(oid.to_string())
    }

    fn format_patch(
        &self,
        range: Option<&str>,
        count: Option<usize>,
    ) -> Result<Vec<(String, String)>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut walk = repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL)?;
        match range {
            Some(r) if r.contains("..") => walk.push_range(r)?,
            Some(r) if count.is_some() => {
                walk.push(repo.revparse_single(r)?.peel_to_commit()?.id())?
            }
            Some(r) => {
                walk.push_head()?;
                walk.hide(repo.revparse_single(r)?.peel_to_commit()?.id())?;
            }
            None => walk.push_head()?,
        }
        let mut commits = Vec::new();
        for oid in walk {
            if count.is_some_and(|n| commits.len() >= n) {
                break;
            }
            let commit = repo.find_commit(oid?)?;
            if commit.parent_count() <= 1 {
                commits.push(commit);
            }
        }
        commits.reverse();
        let total = commits.len();
        let mut out = Vec::new();
        for (i, c) in commits.iter().enumerate() {
            let parent = match c.parent_count() {
                0 => None,
                _ => Some(c.parent(0)?.tree()?),
            };
            let mut diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&c.tree()?), None)?;
            diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
            let summary = c.summary()?.unwrap_or("");
            let email = git2::Email::from_diff(
                &diff,
                i + 1,
                total,
                &c.id(),
                summary,
                c.body()?.unwrap_or(""),
                &c.author(),
                &mut git2::EmailCreateOptions::new(),
            )?;
            out.push((
                patch_file_name(i + 1, summary),
                String::from_utf8_lossy(email.as_slice()).into_owned(),
            ));
        }
        Ok(out)
    }

    fn am(&self, args: &[String], mbox: Option<&[u8]>) -> Result<String, GitError> {
        let mut argv: Vec<String> = vec!["am".to_owned()];
        argv.extend(args.iter().cloned());
        let tmp = self
            .repo
            .lock()
            .expect("repo mutex")
            .path()
            .join("rgit-am.mbox");
        if let Some(data) = mbox {
            std::fs::write(&tmp, data)?;
            argv.push(tmp.to_string_lossy().into_owned());
        }
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let out = self.logged("am", || self.run_git(&argv, &[("GIT_EDITOR", "true")]));
        if mbox.is_some() {
            let _ = std::fs::remove_file(&tmp);
        }
        out
    }

    fn archive(
        &self,
        rev: &str,
        format: &str,
        prefix: &str,
        paths: &[String],
    ) -> Result<Vec<u8>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let obj = repo.revparse_single(rev)?;
        let tree = obj.peel_to_tree()?;
        let mtime = obj.peel_to_commit().map_or(0, |c| c.time().seconds());
        let mut found: Vec<(String, i32, Oid)> = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
            if let Ok(name) = e.name() {
                found.push((format!("{root}{name}"), e.filemode(), e.id()));
            }
            git2::TreeWalkResult::Ok
        })?;
        if !paths.is_empty() {
            let files: Vec<String> = found
                .iter()
                .filter(|(p, mode, _)| *mode != 0o040000 && pathspec_matches(paths, p))
                .map(|(p, ..)| p.clone())
                .collect();
            if files.is_empty() {
                return Err(did_not_match(&paths.join(" ")));
            }
            found.retain(|(p, mode, _)| {
                files
                    .iter()
                    .any(|f| f == p || *mode == 0o040000 && f.starts_with(&format!("{p}/")))
            });
        }
        // (name, git file mode, content); folders end in `/`.
        let mut entries = Vec::new();
        if prefix.ends_with('/') {
            entries.push((prefix.to_owned(), 0o040000, Vec::new()));
        }
        for (path, mode, id) in found {
            match mode {
                0o040000 => entries.push((format!("{prefix}{path}/"), mode, Vec::new())),
                0o160000 => {}
                _ => entries.push((
                    format!("{prefix}{path}"),
                    mode,
                    repo.find_blob(id)?.content().to_vec(),
                )),
            }
        }
        match format {
            "zip" => Ok(zip_archive(&entries, mtime)),
            "tar" => tar_archive(&entries, mtime),
            "tgz" | "tar.gz" => {
                use std::io::Write;
                let mut gz =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                gz.write_all(&tar_archive(&entries, mtime)?)?;
                Ok(gz.finish()?)
            }
            other => Err(GitError::Other(format!("unknown archive format {other:?}"))),
        }
    }

    fn gc(&self, args: &[String]) -> Result<String, GitError> {
        let mut argv = vec!["gc"];
        argv.extend(args.iter().map(String::as_str));
        self.run_git(&argv, &[])
    }

    fn fsck(&self, args: &[String]) -> Result<String, GitError> {
        let mut argv = vec!["fsck"];
        argv.extend(args.iter().map(String::as_str));
        self.run_git(&argv, &[])
    }

    fn clean(&self, dry_run: bool, args: &[String]) -> Result<String, GitError> {
        // libgit2 has no clean; -nd lists, -fd removes (files and directories).
        let mut argv = vec!["clean", if dry_run { "-nd" } else { "-fd" }];
        argv.extend(args.iter().map(String::as_str));
        if dry_run {
            return self.run_git(&argv, &[]);
        }
        self.logged("clean", || self.run_git(&argv, &[]))
    }

    fn remove_path(&self, path: &str, cached: bool, recursive: bool) -> Result<(), GitError> {
        self.logged("rm", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let mut index = repo.index()?;
            let hits = index_matches(&index, path)?;
            if hits.is_empty() {
                return Err(did_not_match(path));
            }
            if !recursive && hits.iter().any(|h| h != path) && !path.contains(['*', '?', '[']) {
                return Err(GitError::Other(format!(
                    "not removing '{path}' recursively without -r"
                )));
            }
            for hit in &hits {
                index.remove_path(Path::new(hit))?;
                if !cached {
                    let full = self.workdir.join(hit);
                    if full.is_file() || full.is_symlink() {
                        std::fs::remove_file(&full)?;
                    }
                    // Drop folders left empty, like git does.
                    let mut dir = full.parent();
                    while let Some(d) = dir.filter(|d| *d != self.workdir) {
                        if std::fs::remove_dir(d).is_err() {
                            break;
                        }
                        dir = d.parent();
                    }
                }
            }
            index.write()?;
            Ok(())
        })
    }

    fn move_path(&self, from: &str, to: &str, force: bool) -> Result<(), GitError> {
        self.logged("mv", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let from = from.trim_end_matches('/');
            let mut to = to.trim_end_matches('/').to_owned();
            if self.workdir.join(&to).is_dir() {
                let name = Path::new(from).file_name().unwrap_or_default();
                to = Path::new(&to).join(name).to_string_lossy().into_owned();
            }
            let mut index = repo.index()?;
            let prefix = format!("{from}/");
            let tracked: Vec<String> = index
                .iter()
                .map(|e| String::from_utf8_lossy(&e.path).into_owned())
                .filter(|p| p == from || p.starts_with(&prefix))
                .collect();
            if tracked.is_empty() {
                return Err(GitError::Other(format!(
                    "not under version control: {from}"
                )));
            }
            let dest = self.workdir.join(&to);
            if dest.exists() {
                if !force || dest.is_dir() {
                    return Err(GitError::Other(format!(
                        "destination {to} already exists; use --force to overwrite"
                    )));
                }
                index.remove_path(Path::new(&to))?;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(self.workdir.join(from), &dest)?;
            for old in &tracked {
                index.remove_path(Path::new(old))?;
                index.add_path(Path::new(&format!("{to}{}", &old[from.len()..])))?;
            }
            index.write()?;
            Ok(())
        })
    }

    fn describe(
        &self,
        rev: &str,
        tags: bool,
        dirty: bool,
        long: bool,
        abbrev: Option<u32>,
        pattern: Option<&str>,
    ) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut opts = git2::DescribeOptions::new();
        if tags {
            opts.describe_tags();
        }
        if let Some(p) = pattern {
            opts.pattern(p);
        }
        opts.show_commit_oid_as_fallback(true);
        // git2's Object::describe never sets the dirty flag; only the workdir
        // describe does. So when the target is HEAD (the only case --dirty is
        // meaningful for), describe the workdir; otherwise describe the object.
        let head_oid = repo.head().ok().and_then(|h| h.target());
        let obj = repo.revparse_single(rev)?;
        let is_head = rev == "HEAD"
            || obj
                .peel_to_commit()
                .map(|c| Some(c.id()) == head_oid)
                .unwrap_or(false);
        let describe = if is_head {
            repo.describe(&opts)?
        } else {
            obj.describe(&opts)?
        };
        let mut fmt = git2::DescribeFormatOptions::new();
        if dirty {
            fmt.dirty_suffix("-dirty");
        }
        if long {
            fmt.always_use_long_format(true);
        }
        if let Some(n) = abbrev {
            fmt.abbreviated_size(n);
        }
        Ok(describe.format(Some(&fmt))?)
    }

    fn git(&self, args: &[String]) -> Result<String, GitError> {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_git(&argv, &[])
    }

    fn reset(&self, rev: &str, mode: ResetMode) -> Result<(), GitError> {
        self.logged("reset", || {
            let repo = self.repo.lock().expect("repo mutex");
            let target = repo.revparse_single(rev)?;
            let kind = match mode {
                ResetMode::Soft => ResetType::Soft,
                ResetMode::Mixed => ResetType::Mixed,
                ResetMode::Hard => ResetType::Hard,
            };
            repo.reset(&target, kind, None)?;
            Ok(())
        })
    }

    fn pick(&self, revs: &[String], opts: &crate::PickOptions) -> Result<(), GitError> {
        self.logged(if opts.revert { "revert" } else { "cherry-pick" }, || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            use git2::RepositoryState as S;
            if !matches!(repo.state(), S::Clean | S::Bisect) {
                return Err(GitError::Conflict(
                    "an operation is already in progress; finish or abort it first".into(),
                ));
            }
            let todo = pick_list(&repo, revs, opts.revert)?;
            if todo.is_empty() {
                return Err(GitError::Other("no commits to apply".into()));
            }
            let orig_head = repo.head()?.peel_to_commit()?.id();
            run_picks(&repo, &todo, opts, orig_head)
        })
    }

    fn pick_continue(&self) -> Result<(), GitError> {
        self.logged("cherry-pick", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let state = read_pick_state(&repo)?;
            let mut index = repo.index()?;
            if index.has_conflicts() {
                return Err(GitError::Conflict(
                    "resolve the conflicts and stage the files first".into(),
                ));
            }
            if let Some(source) = pick_head(&repo) {
                let source = repo.find_commit(source)?;
                let head = repo.head()?.peel_to_commit()?;
                let tree = repo.find_tree(index.write_tree()?)?;
                if tree.id() == head.tree_id() {
                    return Err(GitError::Conflict(format!(
                        "the {} is now empty; run `rgit {} --skip`",
                        state.verb(),
                        state.verb()
                    )));
                }
                let message = std::fs::read_to_string(repo.path().join("MERGE_MSG"))
                    .map(|m| strip_comments(&m))
                    .unwrap_or_else(|_| pick_message(&source, &state.opts));
                let committer = repo.signature()?;
                let author = if state.opts.revert {
                    committer.clone()
                } else {
                    source.author().to_owned()
                };
                repo.commit(Some("HEAD"), &author, &committer, &message, &tree, &[&head])?;
            }
            repo.cleanup_state()?;
            run_picks(&repo, &state.todo[1..], &state.opts, state.orig_head)
        })
    }

    fn pick_skip(&self) -> Result<(), GitError> {
        self.logged("cherry-pick", || {
            let repo = self.repo.lock().expect("repo mutex");
            let state = read_pick_state(&repo)?;
            let head = repo.head()?.peel_to_commit()?;
            repo.reset(head.as_object(), ResetType::Hard, None)?;
            repo.cleanup_state()?;
            run_picks(&repo, &state.todo[1..], &state.opts, state.orig_head)
        })
    }

    fn pick_abort(&self) -> Result<(), GitError> {
        self.logged("cherry-pick abort", || {
            let repo = self.repo.lock().expect("repo mutex");
            let state = read_pick_state(&repo)?;
            let orig = repo.find_commit(state.orig_head)?;
            repo.reset(orig.as_object(), ResetType::Hard, None)?;
            repo.cleanup_state()?;
            Ok(())
        })
    }

    fn merge_with(
        &self,
        revs: &[String],
        opts: &crate::MergeOptions,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        if revs.len() > 1 {
            return self.logged("merge", || octopus(self, revs, opts, report));
        }
        let rev = revs.first().map(String::as_str).unwrap_or("HEAD");
        self.logged("merge", || {
            let repo = self.repo.lock().expect("repo mutex");
            let source = repo.revparse_single(rev)?.peel_to_commit()?;
            merge_commit(&repo, &source, rev, opts, report)
        })
    }

    fn merge_abort(&self) -> Result<(), GitError> {
        self.logged("merge abort", || {
            let repo = self.repo.lock().expect("repo mutex");
            // A conflicted merge has not moved HEAD, so hard-resetting to it drops the
            // half-merged index and worktree; cleanup_state clears MERGE_HEAD et al.
            let head = repo.head()?.peel_to_commit()?;
            repo.reset(head.as_object(), git2::ResetType::Hard, None)?;
            repo.cleanup_state()?;
            Ok(())
        })
    }

    fn merge_continue(&self) -> Result<(), GitError> {
        let message = {
            let repo = self.repo.lock().expect("repo mutex");
            if repo.state() != git2::RepositoryState::Merge {
                return Err(GitError::Other("there is no merge to continue".into()));
            }
            std::fs::read_to_string(repo.path().join("MERGE_MSG"))
                .map(|m| strip_comments(&m))
                .unwrap_or_else(|_| "Merge".to_owned())
        };
        // `commit` adds every MERGE_HEAD as a parent and clears the merge state.
        self.commit(&message)
    }

    fn resolve_conflict(&self, path: &str, ours: bool) -> Result<(), GitError> {
        self.logged("resolve", || {
            let repo = self.repo.lock().expect("repo mutex");
            let mut index = repo.index()?;
            // Find the chosen side's blob before mutating the index.
            let chosen = {
                let conflicts = index.conflicts()?;
                let mut found = None;
                for entry in conflicts {
                    let entry = entry?;
                    let side = if ours { entry.our } else { entry.their };
                    if let Some(e) = side
                        && e.path == path.as_bytes()
                    {
                        found = Some(e.id);
                        break;
                    }
                }
                found
            };
            let Some(oid) = chosen else {
                return Err(GitError::Conflict(format!(
                    "no conflict recorded for {path}"
                )));
            };
            let blob = repo.find_blob(oid)?;
            std::fs::write(self.workdir.join(path), blob.content())?;
            index.conflict_remove(Path::new(path))?;
            index.add_path(Path::new(path))?;
            index.write()?;
            Ok(())
        })
    }

    fn create_tag(&self, name: &str, message: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let target = repo.head()?.peel(ObjectType::Commit)?;
        if message.trim().is_empty() {
            repo.tag_lightweight(name, &target, false)?;
        } else {
            let sig = repo.signature()?;
            repo.tag(name, &target, &sig, message, false)?;
        }
        Ok(())
    }

    fn delete_tag(&self, name: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.tag_delete(name)?;
        Ok(())
    }

    fn delete_branch(&self, name: &str, force: bool) -> Result<(), GitError> {
        self.logged("delete branch", || {
            let repo = self.repo.lock().expect("repo mutex");
            let mut branch = repo.find_branch(name, BranchType::Local)?;
            if !force {
                let tip = branch.get().peel_to_commit()?.id();
                let merged = repo
                    .head()
                    .ok()
                    .and_then(|h| h.peel_to_commit().ok())
                    .map(|head| {
                        head.id() == tip
                            || repo.graph_descendant_of(head.id(), tip).unwrap_or(false)
                    })
                    .unwrap_or(false);
                if !merged {
                    return Err(GitError::Other(format!(
                        "branch {name} is not fully merged into HEAD; use --force to delete"
                    )));
                }
            }
            branch.delete()?;
            Ok(())
        })
    }

    fn remotes(&self) -> Result<Vec<crate::Remote>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut remotes = Vec::new();
        let names = repo.remotes()?;
        for name in names.iter().filter_map(|n| n.ok().flatten()) {
            let remote = repo.find_remote(name)?;
            let url = remote.url().unwrap_or_default().to_owned();
            remotes.push(crate::Remote {
                name: name.to_owned(),
                url,
            });
        }
        remotes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(remotes)
    }

    fn add_remote(&self, name: &str, url: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.remote(name, url)?;
        Ok(())
    }

    fn remove_remote(&self, name: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.remote_delete(name)?;
        Ok(())
    }

    fn set_remote_url(&self, name: &str, url: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.remote_set_url(name, url)?;
        Ok(())
    }

    fn rename_remote(&self, old: &str, new: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        // Returns the list of non-default refspecs it could not rename; ignore it.
        repo.remote_rename(old, new)?;
        Ok(())
    }

    fn worktrees(&self) -> Result<Vec<crate::Worktree>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut out = Vec::new();

        // The main worktree first, described from this repository directly.
        if let Some(dir) = repo.workdir() {
            let (branch, head) = worktree_head(&repo);
            out.push(crate::Worktree {
                name: "(main)".to_owned(),
                path: dir.to_string_lossy().into_owned(),
                branch,
                head,
                dirty: repo_is_dirty(&repo),
                locked: false,
                is_main: true,
            });
        }

        // Each linked worktree, opened on its own path for branch/HEAD/dirty.
        let names = repo.worktrees()?;
        for name in names.iter().filter_map(|n| n.ok().flatten()) {
            let Ok(wt) = repo.find_worktree(name) else {
                continue;
            };
            let path = wt.path().to_path_buf();
            let locked = !matches!(wt.is_locked(), Ok(git2::WorktreeLockStatus::Unlocked));
            let (branch, head, dirty) = match Repository::open(&path) {
                Ok(wtr) => {
                    let (b, h) = worktree_head(&wtr);
                    (b, h, repo_is_dirty(&wtr))
                }
                Err(_) => (None, None, false),
            };
            out.push(crate::Worktree {
                name: name.to_owned(),
                path: path.to_string_lossy().into_owned(),
                branch,
                head,
                dirty,
                locked,
                is_main: false,
            });
        }

        out.sort_by(|a, b| b.is_main.cmp(&a.is_main).then(a.name.cmp(&b.name)));
        Ok(out)
    }

    fn add_worktree(&self, name: &str, path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.worktree(name, Path::new(path), None)?;
        Ok(())
    }

    fn remove_worktree(&self, name: &str, force: bool) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let wt = repo.find_worktree(name)?;
        let mut opts = git2::WorktreePruneOptions::new();
        opts.valid(true).working_tree(true).locked(force);
        wt.prune(Some(&mut opts))?;
        Ok(())
    }

    fn prune_worktrees(&self) -> Result<Vec<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut pruned = Vec::new();
        for name in repo.worktrees()?.iter().filter_map(|t| t.ok().flatten()) {
            let Ok(wt) = repo.find_worktree(name) else {
                continue;
            };
            // Only prune entries whose working tree is missing (not valid).
            let mut opts = git2::WorktreePruneOptions::new();
            opts.valid(false).working_tree(true);
            if wt.is_prunable(Some(&mut opts)).unwrap_or(false) && wt.prune(Some(&mut opts)).is_ok()
            {
                pruned.push(name.to_owned());
            }
        }
        Ok(pruned)
    }

    fn prune_objects(&self, dry_run: bool) -> Result<String, GitError> {
        // libgit2 has no object prune; shell out like the other gc-style ops.
        let mut args = vec!["prune"];
        if dry_run {
            args.push("-n");
        }
        self.run_git(&args, &[])
    }

    fn config_get(&self, key: &str) -> Result<Option<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cfg = repo.config()?;
        match cfg.get_string(key) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.code() == ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn config_set(&self, key: &str, value: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut cfg = repo.config()?;
        cfg.set_str(key, value)?;
        Ok(())
    }

    fn branch_exists(&self, name: &str) -> bool {
        let repo = self.repo.lock().expect("repo mutex");
        repo.find_branch(name, BranchType::Local).is_ok()
    }

    fn lanes_active(&self) -> bool {
        let repo = self.repo.lock().expect("repo mutex");
        crate::lanes::active(&repo)
    }

    fn lanes_init(&self) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        crate::lanes::init(&repo)
    }

    fn lanes_off(&self) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        crate::lanes::off(&repo)
    }

    fn lanes_state(&self) -> Result<crate::LanesState, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::lanes::state(&repo)
    }

    fn lane_new(&self, name: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::lanes::new_lane(&repo, name)
    }

    fn lane_stack(&self, name: &str, parent: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::lanes::stack(&repo, name, parent)
    }

    fn lane_restack(&self) -> Result<crate::RestackOutcome, GitError> {
        self.logged("lane restack", || {
            let repo = self.repo.lock().expect("repo mutex");
            crate::lanes::restack(&repo)
        })
    }

    fn lane_assign(&self, lane: &str, path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::lanes::assign(&repo, lane, path)
    }

    fn lane_assign_hunk(&self, lane: &str, path: &str, new_start: u32) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::lanes::assign_hunk(&repo, lane, path, new_start)
    }

    fn lane_unassign(&self, path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::lanes::unassign(&repo, path)
    }

    fn lane_commit(&self, lane: &str, message: &str) -> Result<String, GitError> {
        self.logged("lane commit", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            crate::lanes::commit(&repo, lane, message)
        })
    }

    fn lane_rename(&self, old: &str, new: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::lanes::rename(&repo, old, new)
    }

    fn lane_delete(&self, name: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::lanes::delete(&repo, name)
    }

    fn lane_push(&self, lane: &str) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let branch = crate::lanes::lane_branch(&repo, lane)?;
        if repo.find_branch(&branch, BranchType::Local).is_err() {
            return Err(GitError::Other(format!(
                "lane {lane} has no commits yet; commit it first"
            )));
        }
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        let remote = push_lane_branch(&repo, &branch, &|_| {}, cred_guard.as_deref())?;
        Ok(format!("pushed {branch} to {remote}"))
    }

    fn set_credential_prompt(&self, prompt: Box<dyn crate::CredentialPrompt>) {
        *self.cred_prompt.lock().expect("cred mutex") = Some(prompt);
    }

    fn lane_pr(&self, lane: &str) -> Result<String, GitError> {
        // detect_main locks the repo, so resolve the base before we lock.
        let base = crate::workflow::detect_main(self);
        let repo = self.repo.lock().expect("repo mutex");
        let branch = crate::lanes::lane_branch(&repo, lane)?;
        if repo.find_branch(&branch, BranchType::Local).is_err() {
            return Err(GitError::Other(format!(
                "lane {lane} has no commits yet; commit it first"
            )));
        }
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        push_lane_branch(&repo, &branch, &|_| {}, cred_guard.as_deref())?;
        drop(cred_guard);
        Ok(crate::workflow::open_pull_request(&branch, &base))
    }

    fn rename_branch(&self, old: &str, new: &str) -> Result<(), GitError> {
        self.logged("rename branch", || {
            let repo = self.repo.lock().expect("repo mutex");
            repo.find_branch(old, BranchType::Local)?
                .rename(new, false)?;
            Ok(())
        })
    }

    fn local_branches(&self) -> Result<Vec<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut names = Vec::new();
        for branch in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch?;
            if let Some(name) = branch.name()?.map(str::to_owned) {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    fn remote_branches(&self) -> Result<Vec<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut names = Vec::new();
        for branch in repo.branches(Some(BranchType::Remote))? {
            let (branch, _) = branch?;
            if let Some(name) = branch.name()?.map(str::to_owned) {
                // Skip the symbolic `origin/HEAD` pointer git also hides.
                if !name.ends_with("/HEAD") {
                    names.push(name);
                }
            }
        }
        names.sort();
        Ok(names)
    }

    fn checkout_branch(&self, name: &str) -> Result<(), GitError> {
        self.logged("checkout", || {
            let repo = self.repo.lock().expect("repo mutex");
            checkout(&repo, name)
        })
    }

    fn create_branch(&self, name: &str) -> Result<(), GitError> {
        self.logged("create branch", || {
            let repo = self.repo.lock().expect("repo mutex");
            let head = repo.head()?.peel_to_commit()?;
            repo.branch(name, &head, false)?;
            checkout(&repo, name)
        })
    }

    fn discard_file(&self, path: &str) -> Result<(), GitError> {
        self.logged("discard", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            no_match(&repo, path)?;
            let file = self.workdir.join(path).is_file();
            if file && repo.status_file(Path::new(path))?.contains(Status::WT_NEW) {
                std::fs::remove_file(self.workdir.join(path))?;
            } else {
                // Restore the worktree file to its index (staged) content.
                let mut checkout = CheckoutBuilder::new();
                checkout.path(path).force();
                repo.checkout_index(None, Some(&mut checkout))?;
            }
            Ok(())
        })
    }

    fn discard_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError> {
        self.logged("discard", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let diff = worktree_diff(&repo, path)?;
            let patch = reverse_hunk_patch(&diff, path, new_start)?;
            repo.apply(
                &Diff::from_buffer(patch.as_bytes())?,
                ApplyLocation::WorkDir,
                None,
            )?;
            Ok(())
        })
    }

    fn discard_lines(&self, path: &str, new_start: u32, lines: &[usize]) -> Result<(), GitError> {
        self.logged("discard", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let diff = worktree_diff(&repo, path)?;
            let patch = partial_hunk_patch(&diff, path, new_start, lines, true)?;
            repo.apply(
                &Diff::from_buffer(patch.as_bytes())?,
                ApplyLocation::WorkDir,
                None,
            )?;
            Ok(())
        })
    }

    fn commit_msg_path(&self) -> PathBuf {
        self.repo
            .lock()
            .expect("repo mutex")
            .path()
            .join("COMMIT_EDITMSG")
    }

    fn commit(&self, message: &str) -> Result<(), GitError> {
        self.logged("commit", || {
            let repo = self.repo.lock().expect("repo mutex");
            let parents = match repo.head() {
                Ok(head_ref) => vec![head_ref.peel_to_commit()?],
                Err(_) => Vec::new(),
            };
            make_commit(&repo, message, &parents, true, true)
        })
    }

    fn amend(&self, message: &str) -> Result<(), GitError> {
        self.logged("amend", || {
            let repo = self.repo.lock().expect("repo mutex");
            make_amend(&repo, message, true)
        })
    }

    fn reword(&self, rev: &str, message: &str) -> Result<(), GitError> {
        self.logged("reword", || {
            {
                let repo = self.repo.lock().expect("repo mutex");
                let target = repo.revparse_single(rev)?.peel_to_commit()?.id();
                reword_commit(&repo, target, message)?;
            }
            // Descendant stacked branches point at the old oids; move them forward.
            let _ = self.restack();
            Ok(())
        })
    }

    fn uncommit(&self, n: usize) -> Result<(), GitError> {
        self.logged("uncommit", || {
            let repo = self.repo.lock().expect("repo mutex");
            let target = repo.revparse_single(&format!("HEAD~{}", n.max(1)))?;
            repo.reset(&target, ResetType::Soft, None)?;
            Ok(())
        })
    }

    fn split(&self, rev: &str, paths: &[String]) -> Result<(), GitError> {
        self.logged("split", || {
            {
                let repo = self.repo.lock().expect("repo mutex");
                let branch_ref = head_branch_ref(&repo)?;
                let target = repo.revparse_single(rev)?.peel_to_commit()?;
                let parent = target
                    .parent(0)
                    .map_err(|_| GitError::Other("cannot split the root commit".to_owned()))?;
                let p_tree = parent.tree()?;
                let c_tree = target.tree()?;

                // First part: the parent's tree with the selected paths taken from the
                // target (nested paths handled via a full-path index).
                let mut index = git2::Index::new()?;
                index.read_tree(&p_tree)?;
                let mut opts = DiffOptions::new();
                let diff = repo.diff_tree_to_tree(Some(&p_tree), Some(&c_tree), Some(&mut opts))?;
                let selected = |p: &str| {
                    paths
                        .iter()
                        .any(|s| p == s || p.starts_with(&format!("{s}/")))
                };
                let mut moved = 0;
                for i in 0..diff.deltas().len() {
                    let delta = diff.get_delta(i).expect("delta in range");
                    let path = delta
                        .new_file()
                        .path()
                        .or_else(|| delta.old_file().path())
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if !selected(&path) {
                        continue;
                    }
                    match c_tree.get_path(std::path::Path::new(&path)) {
                        Ok(entry) => index_set(&mut index, &path, entry.id(), entry.filemode())?,
                        Err(_) => {
                            index.remove_path(std::path::Path::new(&path))?;
                        }
                    }
                    moved += 1;
                }
                if moved == 0 {
                    return Err(GitError::Other(
                        "no changes in the given paths for this commit".to_owned(),
                    ));
                }
                let part1_tree = repo.find_tree(index.write_tree_to(&repo)?)?;

                let sig = repo.signature()?;
                let full_msg = target.message().unwrap_or("");
                // Part 1 keeps only the subject (a fresh change id); part 2 keeps the
                // full message and the original change id.
                let subject = full_msg.lines().next().unwrap_or("").to_owned();
                let c1 = repo.commit(
                    None,
                    &target.author(),
                    &sig,
                    &subject,
                    &part1_tree,
                    &[&parent],
                )?;
                let c1_commit = repo.find_commit(c1)?;
                let c2 = repo.commit(
                    None,
                    &target.author(),
                    &sig,
                    full_msg,
                    &c_tree,
                    &[&c1_commit],
                )?;

                let chain = first_parent_chain(&repo, target.id())?;
                let descendants: Vec<&git2::Commit> = chain.iter().rev().skip(1).collect();
                let new_tip = replay_onto(&repo, &descendants, c2)?;
                repo.reference(&branch_ref, new_tip, true, "rgit split")?;
            }
            let _ = self.restack();
            Ok(())
        })
    }

    fn sync(&self, report: &dyn Fn(OpProgress)) -> Result<crate::RestackOutcome, GitError> {
        self.logged("sync", || {
            self.fetch(None, &[], &crate::FetchArgs::default(), report)?;
            {
                let repo = self.repo.lock().expect("repo mutex");
                let current_ref: Option<String> = repo
                    .head()
                    .ok()
                    .and_then(|h| h.name().ok().map(str::to_owned));
                for entry in repo.branches(Some(BranchType::Local))? {
                    let (branch, _) = entry?;
                    let full = branch.get().name().ok().map(str::to_owned);
                    if full.is_none() || full == current_ref {
                        continue;
                    }
                    let tip = branch.get().peel_to_commit()?.id();
                    if let Ok(upstream) = branch.upstream() {
                        let up = upstream.get().peel_to_commit()?.id();
                        // Only a strict fast-forward is safe to apply to a ref we are
                        // not on (no working tree to update).
                        if up != tip
                            && repo.graph_descendant_of(up, tip).unwrap_or(false)
                            && let Some(name) = &full
                        {
                            repo.reference(name, up, true, "rgit sync fast-forward")?;
                        }
                    }
                }
            }
            self.restack()
        })
    }

    fn submit_stack(&self, report: &dyn Fn(OpProgress)) -> Result<Vec<String>, GitError> {
        let parents: std::collections::HashMap<String, Option<String>> =
            self.stack_parents()?.into_iter().collect();
        let start = self
            .status()?
            .head
            .branch
            .ok_or_else(|| GitError::Other("not on a branch".to_owned()))?;
        // The stack chain for the current branch, bottom-up (child before parent
        // when reversed).
        let mut chain: Vec<(String, String)> = Vec::new();
        let mut cur = start.clone();
        while let Some(Some(p)) = parents.get(&cur) {
            chain.push((cur.clone(), p.clone()));
            cur = p.clone();
        }
        chain.reverse();
        if chain.is_empty() {
            return Err(GitError::Other(
                "the current branch is not in a stack".to_owned(),
            ));
        }
        let mut notes = Vec::new();
        for (branch, base) in &chain {
            self.checkout_branch(branch)?;
            // force-with-lease + set upstream: a stack submit re-pushes rewritten
            // branches, but only when the remote still matches ours.
            self.push(None, false, true, true, report)?;
            notes.push(crate::workflow::open_pull_request(branch, base));
        }
        let _ = self.checkout_branch(&start);
        Ok(notes)
    }

    fn prune_merged(&self, base: &str) -> Result<Vec<String>, GitError> {
        self.logged("prune", || {
            let repo = self.repo.lock().expect("repo mutex");
            let base_oid = repo.revparse_single(base)?.peel_to_commit()?.id();
            let current_ref: Option<String> = repo
                .head()
                .ok()
                .and_then(|h| h.name().ok().map(str::to_owned));
            let mut deleted = Vec::new();
            for entry in repo.branches(Some(BranchType::Local))? {
                let (mut branch, _) = entry?;
                let full = branch.get().name().ok().map(str::to_owned);
                if full.is_some() && full == current_ref {
                    continue;
                }
                let name = full
                    .as_deref()
                    .map(|f| f.trim_start_matches("refs/heads/").to_owned())
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let tip = branch.get().peel_to_commit()?.id();
                if tip == base_oid {
                    continue;
                }
                // Merged when base descends from the branch tip (tip is an ancestor).
                if repo.graph_descendant_of(base_oid, tip).unwrap_or(false) {
                    branch.delete()?;
                    deleted.push(name);
                }
            }
            deleted.sort();
            Ok(deleted)
        })
    }

    fn reorder(&self, rev: &str, target: &str, before: bool) -> Result<(), GitError> {
        self.logged("reorder", || {
            {
                let repo = self.repo.lock().expect("repo mutex");
                let branch_ref = head_branch_ref(&repo)?;
                let rev_oid = repo.revparse_single(rev)?.peel_to_commit()?.id();
                let target_oid = repo.revparse_single(target)?.peel_to_commit()?.id();
                if rev_oid == target_oid {
                    return Err(GitError::Other(
                        "cannot move a commit onto itself".to_owned(),
                    ));
                }
                // Walk HEAD down the first-parent chain until both are seen.
                let mut chain: Vec<git2::Commit> = Vec::new();
                let mut c = repo.head()?.peel_to_commit()?;
                let (mut seen_rev, mut seen_target) = (false, false);
                loop {
                    seen_rev |= c.id() == rev_oid;
                    seen_target |= c.id() == target_oid;
                    chain.push(c);
                    if seen_rev && seen_target {
                        break;
                    }
                    c = match chain.last().unwrap().parent(0) {
                        Ok(p) => p,
                        Err(_) => {
                            return Err(GitError::Other(
                                "both commits must be ancestors of HEAD".to_owned(),
                            ));
                        }
                    };
                }
                let base = chain.last().unwrap().parent(0).map_err(|_| {
                    GitError::Other("cannot reorder across the root commit".to_owned())
                })?;

                // Affected commits, oldest first; move rev relative to target.
                let mut order: Vec<Oid> = chain.iter().rev().map(git2::Commit::id).collect();
                let rev_pos = order
                    .iter()
                    .position(|&o| o == rev_oid)
                    .expect("rev in order");
                order.remove(rev_pos);
                let target_pos = order
                    .iter()
                    .position(|&o| o == target_oid)
                    .expect("target in order");
                order.insert(if before { target_pos } else { target_pos + 1 }, rev_oid);

                let commits: Vec<git2::Commit> = order
                    .iter()
                    .map(|o| repo.find_commit(*o))
                    .collect::<Result<_, _>>()?;
                let refs: Vec<&git2::Commit> = commits.iter().collect();
                let new_tip = replay_onto(&repo, &refs, base.id())?;
                repo.reference(&branch_ref, new_tip, true, "rgit move")?;
            }
            let _ = self.restack();
            Ok(())
        })
    }

    fn squash_range(&self, from: &str) -> Result<(), GitError> {
        self.logged("squash", || {
            {
                let repo = self.repo.lock().expect("repo mutex");
                let branch_ref = head_branch_ref(&repo)?;
                let base = repo.revparse_single(from)?.peel_to_commit()?;
                let head = repo.head()?.peel_to_commit()?;
                if base.id() == head.id() {
                    return Err(GitError::Other("nothing to squash".to_owned()));
                }
                let chain = first_parent_chain(&repo, base.id())?; // [HEAD.., base]
                // Folded commits, oldest first (base's child up to HEAD).
                let folded: Vec<&git2::Commit> = chain.iter().rev().skip(1).collect();
                let mut msg = String::new();
                for c in &folded {
                    msg.push_str(c.message().unwrap_or("").trim_end());
                    msg.push_str("\n\n");
                }
                let msg =
                    crate::change_id::preserve(&repo, head.message().unwrap_or(""), msg.trim_end());
                let sig = repo.signature()?;
                let new = repo.commit(None, &head.author(), &sig, &msg, &head.tree()?, &[&base])?;
                repo.reference(&branch_ref, new, true, "rgit squash range")?;
            }
            let _ = self.restack();
            Ok(())
        })
    }

    fn squash(&self, rev: &str) -> Result<(), GitError> {
        self.logged("squash", || {
            {
                let repo = self.repo.lock().expect("repo mutex");
                let branch_ref = head_branch_ref(&repo)?;
                let target = repo.revparse_single(rev)?.peel_to_commit()?;
                let parent = target
                    .parent(0)
                    .map_err(|_| GitError::Other("cannot squash the root commit".to_owned()))?;
                let chain = first_parent_chain(&repo, target.id())?; // [HEAD.., target]

                // Fold target into its parent: parent's parents, target's tree, and
                // both messages joined.
                let combined = format!(
                    "{}\n\n{}",
                    parent.message().unwrap_or("").trim_end(),
                    target.message().unwrap_or("").trim_end()
                );
                let combined =
                    crate::change_id::preserve(&repo, parent.message().unwrap_or(""), &combined);
                let sig = repo.signature()?;
                let grandparents: Vec<git2::Commit> = (0..parent.parent_count())
                    .filter_map(|k| parent.parent(k).ok())
                    .collect();
                let gp_refs: Vec<&git2::Commit> = grandparents.iter().collect();
                let squashed = repo.commit(
                    None,
                    &parent.author(),
                    &sig,
                    &combined,
                    &target.tree()?,
                    &gp_refs,
                )?;

                // Replay the commits above target onto the squashed commit.
                let descendants: Vec<&git2::Commit> = chain.iter().rev().skip(1).collect();
                let new_tip = replay_onto(&repo, &descendants, squashed)?;
                repo.reference(&branch_ref, new_tip, true, "rgit squash")?;
            }
            let _ = self.restack();
            Ok(())
        })
    }

    fn hooks_dir(&self) -> PathBuf {
        let repo = self.repo.lock().expect("repo mutex");
        // core.hooksPath wins (relative to the working directory); otherwise the
        // repository's own hooks directory.
        if let Ok(cfg) = repo.config()
            && let Ok(p) = cfg.get_path("core.hooksPath")
        {
            return if p.is_absolute() {
                p
            } else {
                self.workdir.join(p)
            };
        }
        repo.path().join("hooks")
    }

    fn commit_no_verify(&self, message: &str) -> Result<(), GitError> {
        self.logged("commit", || {
            let repo = self.repo.lock().expect("repo mutex");
            let parents = match repo.head() {
                Ok(head_ref) => vec![head_ref.peel_to_commit()?],
                Err(_) => Vec::new(),
            };
            make_commit(&repo, message, &parents, true, false)
        })
    }

    fn amend_no_verify(&self, message: &str) -> Result<(), GitError> {
        self.logged("amend", || {
            let repo = self.repo.lock().expect("repo mutex");
            make_amend(&repo, message, false)
        })
    }

    fn commit_report(&self) -> Vec<String> {
        let repo = self.repo.lock().expect("repo mutex");
        commit_report(&repo)
    }

    fn head_message(&self) -> Option<String> {
        let repo = self.repo.lock().expect("repo mutex");
        let head = repo.head().ok()?;
        let commit = head.peel_to_commit().ok()?;
        commit.message().ok().map(str::to_owned)
    }

    fn staged_patch(&self) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        let mut opts = DiffOptions::new();
        let diff = repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?;
        let mut buf = String::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let origin = line.origin();
            if matches!(origin, '+' | '-' | ' ') {
                buf.push(origin);
            }
            buf.push_str(std::str::from_utf8(line.content()).unwrap_or(""));
            true
        })?;
        Ok(buf)
    }

    fn commit_extend(&self) -> Result<(), GitError> {
        let message = {
            let repo = self.repo.lock().expect("repo mutex");
            let head = repo.head()?.peel_to_commit()?;
            head.message().unwrap_or("").to_owned()
        };
        self.amend(&message)
    }

    fn fetch(
        &self,
        remote: Option<&str>,
        refspecs: &[String],
        args: &crate::FetchArgs,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        let cred = cred_guard.as_deref();
        let names: Vec<String> = if args.all {
            repo.remotes()?
                .iter()
                .filter_map(|t| t.ok().flatten())
                .map(|s| s.to_owned())
                .collect()
        } else {
            vec![match remote {
                Some(r) => r.to_owned(),
                None => upstream_remote(&repo)?.0,
            }]
        };
        for name in &names {
            let local = repo.find_remote(name)?.url().is_ok_and(is_local_url);
            if args.dry_run {
                fetch_dry_run(&repo, name, refspecs, args.tags, report, cred)?;
            } else if args.depth > 0 && local {
                // libgit2's local transport cannot fetch shallow; git can.
                let depth = args.depth.to_string();
                let mut cmd = vec!["fetch", "--depth", depth.as_str()];
                if args.prune {
                    cmd.push("--prune");
                }
                if args.tags {
                    cmd.push("--tags");
                }
                cmd.push(name);
                cmd.extend(refspecs.iter().map(String::as_str));
                self.run_git(&cmd, &[])?;
            } else {
                do_fetch(&repo, name, refspecs, args, report, cred)?;
            }
        }
        Ok(())
    }

    fn pull(
        &self,
        remote: Option<&str>,
        branch: Option<&str>,
        rebase: Option<bool>,
        ff_only: bool,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let (remote, branch, rebase, ff_only, chosen) = {
            let repo = self.repo.lock().expect("repo mutex");
            let (upstream, current) = upstream_remote(&repo)?;
            let config = repo.config()?;
            // git refuses a diverged pull until a flag or config picks merge or rebase.
            let chosen = rebase.is_some()
                || ff_only
                || config_rebase(&config, &format!("branch.{current}.rebase")).is_some()
                || config_rebase(&config, "pull.rebase").is_some()
                || config.get_string("pull.ff").is_ok();
            let merge_branch = repo
                .branch_upstream_merge(&format!("refs/heads/{current}"))
                .ok()
                .and_then(|b| b.as_str().ok().map(|s| short_ref(s).to_owned()));
            let rebase = !ff_only
                && rebase.unwrap_or_else(|| {
                    config_rebase(&config, &format!("branch.{current}.rebase"))
                        .or_else(|| config_rebase(&config, "pull.rebase"))
                        .unwrap_or(false)
                });
            let ff_only = ff_only || config.get_string("pull.ff").is_ok_and(|v| v == "only");
            (
                remote.map_or(upstream, str::to_owned),
                branch
                    .map(str::to_owned)
                    .or(merge_branch)
                    .unwrap_or(current),
                rebase,
                ff_only,
                chosen,
            )
        };
        self.logged(if rebase { "pull --rebase" } else { "pull" }, || {
            let repo = self.repo.lock().expect("repo mutex");
            let cred_guard = self.cred_prompt.lock().expect("cred mutex");
            let cred = cred_guard.as_deref();
            do_fetch(
                &repo,
                &remote,
                &[],
                &crate::FetchArgs::default(),
                report,
                cred,
            )?;
            let target = repo.refname_to_id(&format!("refs/remotes/{remote}/{branch}"))?;
            if rebase {
                let upstream = repo.find_annotated_commit(target)?;
                run_rebase(&repo, &upstream, None, report)
            } else {
                let url = repo
                    .find_remote(&remote)?
                    .url()
                    .unwrap_or(&remote)
                    .to_owned();
                let source = repo.find_commit(target)?;
                let (analysis, _) = repo.merge_analysis(&[&repo.find_annotated_commit(target)?])?;
                if !chosen && !analysis.is_up_to_date() && !analysis.is_fast_forward() {
                    return Err(GitError::Other(
                        "you have divergent branches and need to specify how to reconcile \
                         them: pass --rebase, --no-rebase or --ff-only, or set pull.rebase"
                            .to_owned(),
                    ));
                }
                let name = format!("branch '{branch}' of {url}");
                merge_commit(
                    &repo,
                    &source,
                    &name,
                    &crate::MergeOptions {
                        ff_only,
                        ..Default::default()
                    },
                    report,
                )
            }
        })
    }

    fn push(
        &self,
        remote: Option<&str>,
        force: bool,
        force_with_lease: bool,
        set_upstream: bool,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let args = crate::PushArgs {
            force,
            force_with_lease,
            set_upstream,
            ..Default::default()
        };
        self.push_to(remote, &[], &args, report)
    }

    fn push_to(
        &self,
        remote: Option<&str>,
        refspecs: &[String],
        args: &crate::PushArgs,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        let remote_name = match remote {
            Some(r) => r.to_owned(),
            None if refspecs.is_empty() && !args.all && !args.tags => upstream_remote(&repo)?.0,
            None => upstream_remote(&repo)
                .map(|(r, _)| r)
                .or_else(|_| default_remote(&repo))?,
        };
        // (force, source ref, destination ref); an empty source deletes.
        let mut specs = refspecs
            .iter()
            .map(|s| expand_push_refspec(&repo, s))
            .collect::<Result<Vec<_>, _>>()?;
        if args.all {
            for b in repo.branches(Some(BranchType::Local))? {
                if let Ok(name) = b?.0.get().name() {
                    specs.push((false, name.to_owned(), name.to_owned()));
                }
            }
        }
        if args.tags {
            for t in repo.tag_names(None)?.iter().flatten().flatten() {
                let name = format!("refs/tags/{t}");
                specs.push((false, name.clone(), name));
            }
        }
        if specs.is_empty() {
            if args.tags {
                report(OpProgress::Line("no tags to push".to_owned()));
                return Ok(());
            }
            let head = format!("refs/heads/{}", current_branch(&repo)?);
            specs.push((false, head.clone(), head));
        }
        let force = args.force || args.force_with_lease;
        let refspecs: Vec<String> = specs
            .iter()
            .map(|(f, src, dst)| {
                let lead = if force || *f { "+" } else { "" };
                format!("{lead}{src}:{dst}")
            })
            .collect();
        // Force-with-lease: each remote branch must still match the ref we
        // last fetched for it.
        let leases: std::collections::HashMap<String, Oid> = specs
            .iter()
            .filter(|_| args.force_with_lease)
            .filter_map(|(_, _, dst)| {
                let name = dst.strip_prefix("refs/heads/")?;
                let id = repo
                    .refname_to_id(&format!("refs/remotes/{remote_name}/{name}"))
                    .ok()?;
                Some((dst.clone(), id))
            })
            .collect();

        let mut remote = repo.find_remote(&remote_name)?;
        if let Ok(url) = remote.url() {
            report(OpProgress::Line(format!("To {url}")));
        }
        let forced: Vec<String> = specs
            .iter()
            .filter(|(f, _, _)| force || *f)
            .map(|(_, _, dst)| dst.clone())
            .collect();
        let rejected = std::sync::atomic::AtomicBool::new(false);
        let dry_run_hit = std::sync::atomic::AtomicBool::new(false);
        let mut callbacks = remote_callbacks(report, &rejected, cred_guard.as_deref());
        let (dry_run, repo_ref) = (args.dry_run, &*repo);
        let (dry_run_flag, rejected_flag) = (&dry_run_hit, &rejected);
        // The negotiation callback sees every update before anything is sent:
        // it enforces the leases, and a dry run reports the updates and stops.
        callbacks.push_negotiation(move |updates| {
            for update in updates {
                if let Ok(dst) = update.dst_refname()
                    && let Some(lease) = leases.get(dst)
                    && update.src() != *lease
                {
                    return Err(git2::Error::from_str(
                        "stale info: the remote branch moved; force-with-lease aborted",
                    ));
                }
            }
            if dry_run {
                for update in updates {
                    let forced = update
                        .dst_refname()
                        .is_ok_and(|d| forced.iter().any(|f| f == d));
                    let line = push_update_line(repo_ref, update, forced);
                    if line.starts_with(" ! ") {
                        rejected_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    report(OpProgress::Line(line));
                }
                dry_run_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                return Err(git2::Error::from_str("dry run"));
            }
            Ok(())
        });

        let mut opts = PushOptions::new();
        opts.remote_callbacks(callbacks);
        let pushed = remote.push(&refspecs, Some(&mut opts));
        if dry_run_hit.load(std::sync::atomic::Ordering::Relaxed) {
            if rejected.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(GitError::PushRejected);
            }
            return Ok(());
        }
        pushed?;
        if args.dry_run {
            report(OpProgress::Line("Everything up-to-date".to_owned()));
            return Ok(());
        }
        // libgit2 reports a refused ref through the callback but still returns
        // success, so surface the rejection as an error.
        if rejected.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(GitError::PushRejected);
        }

        if args.set_upstream {
            for (_, src, dst) in &specs {
                if let (Some(local), Some(theirs)) = (
                    src.strip_prefix("refs/heads/"),
                    dst.strip_prefix("refs/heads/"),
                ) {
                    repo.find_branch(local, BranchType::Local)?
                        .set_upstream(Some(&format!("{remote_name}/{theirs}")))?;
                }
            }
        }
        Ok(())
    }

    fn push_tags(&self, remote: Option<&str>, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
        // libgit2 rejects a wildcard push refspec, so enumerate the tags and push
        // an explicit refspec for each.
        let refspecs: Vec<String> = {
            let repo = self.repo.lock().expect("repo mutex");
            repo.tag_names(None)?
                .iter()
                .filter_map(|t| t.ok().flatten())
                .map(|t| format!("refs/tags/{t}:refs/tags/{t}"))
                .collect()
        };
        if refspecs.is_empty() {
            report(OpProgress::Line("no tags to push".to_owned()));
            return Ok(());
        }
        self.push_refspecs(remote, &refspecs, report)
    }

    fn push_delete(
        &self,
        remote: Option<&str>,
        branch: &str,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        self.logged("push delete", || {
            // An empty source ref deletes the destination on the remote.
            self.push_refspecs(remote, &[format!(":refs/heads/{branch}")], report)
        })
    }
}

/// The current branch name, erroring on a detached or unborn HEAD.
fn current_branch(repo: &Repository) -> Result<String, GitError> {
    if repo.head_detached().unwrap_or(false) {
        return Err(GitError::DetachedHead);
    }
    let head = repo.head()?;
    head.shorthand()
        .ok()
        .ok_or(GitError::DetachedHead)
        .map(str::to_owned)
}

/// The remote to sync the current branch with (its configured upstream remote,
/// or `origin`), plus the branch name.
fn upstream_remote(repo: &Repository) -> Result<(String, String), GitError> {
    let branch = current_branch(repo)?;
    let remote = repo
        .branch_upstream_remote(&format!("refs/heads/{branch}"))
        .ok()
        .and_then(|buf| buf.as_str().ok().map(str::to_owned))
        .unwrap_or_else(|| "origin".to_owned());
    Ok((remote, branch))
}

fn do_fetch(
    repo: &Repository,
    remote: &str,
    refspecs: &[String],
    args: &crate::FetchArgs,
    report: &dyn Fn(OpProgress),
    cred: Option<&dyn crate::CredentialPrompt>,
) -> Result<(), GitError> {
    let mut remote = repo.find_remote(remote)?;
    if let Ok(url) = remote.url() {
        report(OpProgress::Line(format!("From {url}")));
    }
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(remote_callbacks(report, &ignored, cred));
    if args.prune {
        opts.prune(git2::FetchPrune::On);
    }
    if args.tags {
        opts.download_tags(git2::AutotagOption::All);
    } else if !refspecs.is_empty() {
        // git follows no tags when the refs to fetch are named.
        opts.download_tags(git2::AutotagOption::None);
    }
    if args.depth > 0 {
        opts.depth(args.depth);
    }
    remote.fetch(refspecs, Some(&mut opts), None)?;
    Ok(())
}

/// `git fetch --dry-run`: list the remote's refs and report the tracking-ref
/// updates a fetch would make, without downloading or writing anything.
fn fetch_dry_run(
    repo: &Repository,
    name: &str,
    refspecs: &[String],
    tags: bool,
    report: &dyn Fn(OpProgress),
    cred: Option<&dyn crate::CredentialPrompt>,
) -> Result<(), GitError> {
    let mut remote = repo.find_remote(name)?;
    if let Ok(url) = remote.url() {
        report(OpProgress::Line(format!("From {url}")));
    }
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let heads: Vec<(String, Oid)> = {
        let callbacks = remote_callbacks(report, &ignored, cred);
        let conn = remote.connect_auth(git2::Direction::Fetch, Some(callbacks), None)?;
        conn.list()?
            .iter()
            .map(|h| (h.name().to_owned(), h.oid()))
            .collect()
    };
    let wanted = |head: &str| {
        refspecs.is_empty()
            || refspecs.iter().any(|s| {
                let src = s.trim_start_matches('+').split(':').next().unwrap_or("");
                head == src || short_ref(head) == src
            })
    };
    for (head, new) in heads {
        if !wanted(&head) || head.ends_with("^{}") {
            continue;
        }
        let tag = head.starts_with("refs/tags/");
        let dst = if tag && tags {
            head.clone()
        } else {
            let spec = remote
                .refspecs()
                .find(|s| s.direction() == git2::Direction::Fetch && s.src_matches(&head));
            match spec.and_then(|s| s.transform(&head).ok()) {
                Some(buf) => buf.as_str().unwrap_or_default().to_owned(),
                None => continue,
            }
        };
        let (src, dst_short) = (short_ref(&head), short_ref(&dst));
        let line = match repo.refname_to_id(&dst).ok() {
            Some(old) if old == new => continue,
            None if tag => format!(" * [new tag]         {src} -> {dst_short}"),
            None => format!(" * [new branch]      {src} -> {dst_short}"),
            Some(old) if repo.graph_descendant_of(new, old).unwrap_or(true) => {
                format!("   {}..{}  {src} -> {dst_short}", short7(old), short7(new))
            }
            Some(old) => format!(
                " + {}...{} {src} -> {dst_short}  (forced update)",
                short7(old),
                short7(new)
            ),
        };
        report(OpProgress::Line(line));
    }
    Ok(())
}

/// A git-style `push --dry-run` line for one negotiated ref update.
fn push_update_line(repo: &Repository, update: &git2::PushUpdate<'_>, force: bool) -> String {
    let src = short_ref(update.src_refname().unwrap_or_default());
    let dst = short_ref(update.dst_refname().unwrap_or_default());
    let (old, new) = (update.src(), update.dst());
    if new.is_zero() {
        return format!(" - [deleted]         {dst}");
    }
    if old.is_zero() {
        let kind = if update
            .dst_refname()
            .is_ok_and(|d| d.starts_with("refs/tags/"))
        {
            "tag"
        } else {
            "branch"
        };
        return format!(" * [new {kind}]      {src} -> {dst}");
    }
    if repo.graph_descendant_of(new, old).unwrap_or(false) {
        format!("   {}..{}  {src} -> {dst}", short7(old), short7(new))
    } else if force {
        format!(
            " + {}...{} {src} -> {dst} (forced update)",
            short7(old),
            short7(new)
        )
    } else {
        format!(" ! [rejected]        {src} -> {dst} (non-fast-forward)")
    }
}

/// Expand a git push refspec (`branch`, `src:dst`, `:dst`, `+src:dst`) to
/// (force, full source ref, full destination ref); an empty source deletes.
fn expand_push_refspec(repo: &Repository, spec: &str) -> Result<(bool, String, String), GitError> {
    let (force, spec) = match spec.strip_prefix('+') {
        Some(s) => (true, s),
        None => (false, spec),
    };
    let (src, dst) = match spec.split_once(':') {
        Some((s, d)) => (s, Some(d)),
        None => (spec, None),
    };
    let src = if src.is_empty() {
        String::new()
    } else if src == "HEAD" {
        format!("refs/heads/{}", current_branch(repo)?)
    } else {
        repo.resolve_reference_from_short_name(src)
            .ok()
            .and_then(|r| r.name().ok().map(str::to_owned))
            .ok_or_else(|| GitError::Other(format!("src refspec {src} does not match any")))?
    };
    let dst = match dst {
        None | Some("") if src.is_empty() => {
            return Err(GitError::Other(format!("invalid refspec '{spec}'")));
        }
        None | Some("") => src.clone(),
        Some(d) if d.starts_with("refs/") => d.to_owned(),
        Some(d) if src.starts_with("refs/tags/") => format!("refs/tags/{d}"),
        Some(d) => format!("refs/heads/{d}"),
    };
    Ok((force, src, dst))
}

/// A ref name without its `refs/heads/`, `refs/tags/` or `refs/remotes/` prefix.
fn short_ref(name: &str) -> &str {
    ["refs/heads/", "refs/tags/", "refs/remotes/"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .unwrap_or(name)
}

/// A `pull.rebase`-style setting: `false` merges, anything else (`true`,
/// `merges`, `interactive`) rebases. `None` when unset.
fn config_rebase(config: &git2::Config, key: &str) -> Option<bool> {
    let v = config.get_string(key).ok()?.to_ascii_lowercase();
    Some(!matches!(v.as_str(), "false" | "no" | "off" | "0"))
}

/// Whether `url` goes through libgit2's local transport (a path or `file://`),
/// which cannot fetch shallow history.
fn is_local_url(url: &str) -> bool {
    url.starts_with("file://")
        || (!url.contains("://") && !url.split('/').next().unwrap_or("").contains(':'))
}

/// Merge `source` into HEAD: fast-forward when possible (unless `no_ff`), else
/// a merge commit titled `Merge <name>`. `ff_only` refuses a real merge.
fn merge_commit(
    repo: &Repository,
    source: &git2::Commit<'_>,
    name: &str,
    opts: &crate::MergeOptions,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let annotated = repo.find_annotated_commit(source.id())?;
    let (analysis, _) = repo.merge_analysis(&[&annotated])?;

    if analysis.is_up_to_date() {
        report(OpProgress::Line("Already up to date.".to_owned()));
        return Ok(());
    }
    if opts.ff_only && !analysis.is_fast_forward() {
        return Err(GitError::Other(
            "not possible to fast-forward; use a merge commit instead".to_owned(),
        ));
    }
    if analysis.is_fast_forward() && !opts.no_ff && !opts.squash {
        if let Some(old) = repo.head().ok().and_then(|h| h.target()) {
            report(OpProgress::Line(format!(
                "Updating {}..{}",
                short7(old),
                short7(source.id())
            )));
        }
        // Safe checkout first: refuse (rather than clobber) if the update
        // would overwrite local uncommitted changes, like real git. Only
        // move the branch ref once the working tree updated cleanly.
        repo.checkout_tree(source.as_object(), Some(CheckoutBuilder::new().safe()))
            .map_err(|_| {
                GitError::Conflict(
                    "your local changes would be overwritten by the fast-forward; \
                     commit or stash them first"
                        .into(),
                )
            })?;
        let name = repo.head()?.name().unwrap_or("HEAD").to_owned();
        repo.reference(&name, source.id(), true, "merge: fast-forward")?;
        repo.set_head(&name)?;
        report(OpProgress::Line("Fast-forward".to_owned()));
        return Ok(());
    }

    let mut merge_opts = git2::MergeOptions::new();
    if let Some(side) = &opts.strategy_option {
        merge_opts.file_favor(file_favor(side)?);
    }
    repo.merge(&[&annotated], Some(&mut merge_opts), None)?;
    let message = opts
        .message
        .clone()
        .unwrap_or_else(|| format!("Merge {name}"));
    if opts.squash {
        // A squash stages the result as an ordinary change: no MERGE_HEAD.
        repo.cleanup_state()?;
    } else {
        std::fs::write(repo.path().join("MERGE_MSG"), format!("{message}\n"))?;
    }
    let index = repo.index()?;
    if index.has_conflicts() {
        for entry in index.conflicts()?.flatten() {
            if let Some(path) = entry
                .our
                .as_ref()
                .or(entry.their.as_ref())
                .and_then(|e| std::str::from_utf8(&e.path).ok())
            {
                report(OpProgress::Line(format!(
                    "CONFLICT (content): Merge conflict in {path}"
                )));
            }
        }
        report(OpProgress::Line(
            "Automatic merge failed; fix conflicts and then commit the result.".to_owned(),
        ));
        return Err(GitError::Conflict(if opts.squash {
            "merge conflicts; resolve and commit, or reset --hard to abort".into()
        } else {
            "merge conflicts; resolve, then run `rgit merge --continue` (or --abort)".into()
        }));
    }
    if opts.squash {
        report(OpProgress::Line(
            "Squash commit -- not updating HEAD".to_owned(),
        ));
        return Ok(());
    }
    if opts.no_commit {
        report(OpProgress::Line(
            "Automatic merge went well; stopped before committing as requested".to_owned(),
        ));
        return Ok(());
    }
    let sig = repo.signature()?;
    let tree = repo.find_tree(repo.index()?.write_tree()?)?;
    let head = repo.head()?.peel_to_commit()?;
    repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &[&head, source])?;
    repo.cleanup_state()?;
    report(OpProgress::Line(
        "Merge made by the 'ort' strategy.".to_owned(),
    ));
    Ok(())
}

/// Create a new repository at `path` (`git init`), via libgit2.
/// Create a repository at `path`. `initial_branch` names the first branch
/// (git's `-b`); `bare` makes a bare repository (git's `--bare`).
pub fn init(path: &Path, initial_branch: Option<&str>, bare: bool) -> Result<(), GitError> {
    let branch = initial_branch
        .map(str::to_owned)
        .or_else(default_initial_branch);
    let mut opts = git2::RepositoryInitOptions::new();
    opts.bare(bare);
    if let Some(b) = branch.as_deref() {
        opts.initial_head(b);
    }
    Repository::init_opts(path, &opts)?;
    Ok(())
}

fn default_initial_branch() -> Option<String> {
    git2::Config::open_default()
        .ok()?
        .get_string("init.defaultBranch")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Clone `url` into `path` (`git clone`), via libgit2, using the same
/// credentials as fetch/push and reporting git-style progress.
/// `args.depth` is ignored for a plain local path, as git does.
pub fn clone(
    url: &str,
    path: &Path,
    args: &crate::CloneArgs,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let mut depth = args.depth;
    if depth > 0 && is_local_url(url) {
        if url.starts_with("file://") {
            return clone_with_git(url, path, args);
        }
        report(OpProgress::Line(
            "warning: --depth is ignored in local clones; use file:// instead.".to_owned(),
        ));
        depth = 0;
    }
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(remote_callbacks(report, &ignored, None));
    if depth > 0 {
        opts.depth(depth);
    }
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(opts).bare(args.bare);
    if let Some(b) = &args.branch {
        builder.branch(b);
    }
    if let Some(origin) = args.origin.clone() {
        builder.remote_create(move |repo, _, url| repo.remote(&origin, url));
    }
    let repo = builder.clone(url, path)?;
    if args.recurse_submodules && !args.bare {
        update_submodules(&repo, report)?;
    }
    Ok(())
}

/// A shallow clone over `file://`, which libgit2's local transport cannot do.
fn clone_with_git(url: &str, path: &Path, args: &crate::CloneArgs) -> Result<(), GitError> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--quiet", "--depth", &args.depth.to_string()]);
    if let Some(b) = &args.branch {
        cmd.args(["--branch", b]);
    }
    if let Some(o) = &args.origin {
        cmd.args(["--origin", o]);
    }
    if args.bare {
        cmd.arg("--bare");
    }
    if args.recurse_submodules {
        cmd.arg("--recurse-submodules");
    }
    let out = cmd
        .arg(url)
        .arg(path)
        .output()
        .map_err(|e| GitError::Cli(format!("could not run git: {e}")))?;
    if !out.status.success() {
        return Err(GitError::Cli(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}

/// Clone and check out every submodule of `repo`, recursively.
fn update_submodules(repo: &Repository, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
    for mut sm in repo.submodules()? {
        report(OpProgress::Line(format!(
            "Submodule '{}' ({}) registered for path '{}'",
            sm.name().unwrap_or_default(),
            sm.url().ok().flatten().unwrap_or_default(),
            sm.path().display()
        )));
        sm.update(true, None)?;
        update_submodules(&sm.open()?, report)?;
    }
    Ok(())
}

/// A 7-character short oid, as git prints in ref-update lines.
fn short7(oid: git2::Oid) -> String {
    let s = oid.to_string();
    s[..7.min(s.len())].to_owned()
}

/// Credentials plus progress reporting for network operations: ssh-agent for SSH
/// remotes and git's credential helper for HTTPS, and the remote's sideband
/// text, transfer counts, and ref updates fed to `report` as git-style output.
/// `rejected` is set if the remote refuses a ref (libgit2 reports this through
/// the callback but still returns success from `push`).
/// A credential from a default `~/.ssh/<name>` private key (with its `.pub`),
/// for when no ssh-agent is available. Passphrase-protected keys cannot be
/// unlocked here, so they fail and the caller falls through to the next option.
fn ssh_key_file(user: &str, name: &str, passphrase: Option<&str>) -> Result<Cred, git2::Error> {
    let home = std::env::var("HOME").map_err(|_| git2::Error::from_str("HOME is not set"))?;
    let private = PathBuf::from(home).join(".ssh").join(name);
    if !private.exists() {
        return Err(git2::Error::from_str("no such default ssh key"));
    }
    let public = private.with_extension("pub");
    Cred::ssh_key(
        user,
        public.exists().then_some(&public),
        &private,
        passphrase,
    )
}

/// The remote to push a lane branch to: the branch's own remote if configured,
/// else `origin`, else the first remote.
fn default_remote(repo: &Repository) -> Result<String, GitError> {
    if repo.find_remote("origin").is_ok() {
        return Ok("origin".to_owned());
    }
    let remotes = repo.remotes()?;
    if let Ok(Some(name)) = remotes.get(0) {
        return Ok(name.to_owned());
    }
    Err(GitError::Other("no remote configured".to_owned()))
}

/// Push a specific branch (not HEAD) to the default remote and set its upstream.
/// Used by lanes, which never move HEAD. Returns the remote name.
fn push_lane_branch(
    repo: &Repository,
    branch: &str,
    report: &dyn Fn(OpProgress),
    cred: Option<&dyn crate::CredentialPrompt>,
) -> Result<String, GitError> {
    let remote_name = default_remote(repo)?;
    let mut remote = repo.find_remote(&remote_name)?;
    if let Ok(url) = remote.url() {
        report(OpProgress::Line(format!("To {url}")));
    }
    let branch_ref = format!("refs/heads/{branch}");
    let refspec = format!("{branch_ref}:{branch_ref}");
    let rejected = std::sync::atomic::AtomicBool::new(false);
    let callbacks = remote_callbacks(report, &rejected, cred);
    let mut opts = PushOptions::new();
    opts.remote_callbacks(callbacks);
    remote.push(&[refspec.as_str()], Some(&mut opts))?;
    if rejected.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(GitError::PushRejected);
    }
    repo.find_branch(branch, BranchType::Local)?
        .set_upstream(Some(&format!("{remote_name}/{branch}")))?;
    Ok(remote_name)
}

fn remote_callbacks<'a>(
    report: &'a dyn Fn(OpProgress),
    rejected: &'a std::sync::atomic::AtomicBool,
    cred: Option<&'a dyn crate::CredentialPrompt>,
) -> RemoteCallbacks<'a> {
    let mut cb = RemoteCallbacks::new();
    // libgit2 re-invokes this on every auth failure, so a callback that keeps
    // returning the same credential loops forever (the push appears to hang).
    // Count attempts and return an error once the options are exhausted so the
    // operation fails cleanly instead. `ssh_attempts` counts only real SSH_KEY
    // requests, not the separate USERNAME lookup libgit2 does first.
    let mut ssh_attempts = 0usize;
    let mut ssh_pass: Option<String> = None;
    let mut helper_tried = false;
    let mut prompt_tried = false;
    cb.credentials(move |url, username, allowed| {
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::SSH_KEY) {
            let user = username.unwrap_or("git");
            ssh_attempts += 1;
            return match ssh_attempts {
                // First the agent and unencrypted keys, no passphrase.
                1 => Cred::ssh_key_from_agent(user),
                2 => ssh_key_file(user, "id_ed25519", None),
                3 => ssh_key_file(user, "id_rsa", None),
                // Then, if we can prompt, one passphrase retry of each key.
                4 | 5 if cred.is_some() => {
                    if ssh_pass.is_none() {
                        ssh_pass = cred.and_then(|c| c.ssh_passphrase("~/.ssh key"));
                    }
                    match &ssh_pass {
                        Some(pass) if !pass.is_empty() => {
                            let key = if ssh_attempts == 4 {
                                "id_ed25519"
                            } else {
                                "id_rsa"
                            };
                            ssh_key_file(user, key, Some(pass))
                        }
                        _ => Err(git2::Error::from_str("no passphrase given")),
                    }
                }
                _ => Err(git2::Error::from_str(
                    "ssh authentication failed: no usable key in the agent or ~/.ssh \
                     (unlock your key with ssh-add, or use an https remote)",
                )),
            };
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            // First the git credential helper (keychain, cache, ...).
            if !helper_tried {
                helper_tried = true;
                if let Ok(config) = git2::Config::open_default()
                    && let Ok(c) = Cred::credential_helper(&config, url, username)
                {
                    return Ok(c);
                }
            }
            // Then prompt the user, if a prompt is installed.
            if !prompt_tried {
                prompt_tried = true;
                if let Some(cred) = cred {
                    let user = match username {
                        Some(u) => u.to_owned(),
                        None => cred
                            .username(url)
                            .ok_or_else(|| git2::Error::from_str("no username given"))?,
                    };
                    if let Some(pass) = cred.password(url, &user) {
                        return Cred::userpass_plaintext(&user, &pass);
                    }
                }
            }
            return Err(git2::Error::from_str(
                "authentication failed: no valid credentials (set up a credential helper, \
                 or run in a terminal to be prompted)",
            ));
        }
        Err(git2::Error::from_str("no supported authentication method"))
    });
    // The remote's own progress text (`remote: Counting objects...`).
    cb.sideband_progress(move |data| {
        for line in String::from_utf8_lossy(data)
            .split(['\r', '\n'])
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            report(OpProgress::Line(format!("remote: {line}")));
        }
        true
    });
    // Object transfer, for the progress bar.
    cb.transfer_progress(move |stats| {
        report(OpProgress::Transfer {
            received: stats.received_objects(),
            total: stats.total_objects(),
        });
        true
    });
    // Ref updates on the fetch side (`abc1234..def5678  main -> origin/main`).
    cb.update_tips(move |refname, old, new| {
        let line = if old.is_zero() {
            format!(" * [new ref]         -> {refname}")
        } else {
            format!("   {}..{}  {refname}", short7(old), short7(new))
        };
        report(OpProgress::Line(line));
        true
    });
    // Push transfer counts and per-ref status.
    cb.push_transfer_progress(move |current, total, _bytes| {
        report(OpProgress::Transfer {
            received: current,
            total,
        });
    });
    cb.push_update_reference(move |refname, status| {
        match status {
            None => report(OpProgress::Line(format!("   {refname} -> ok"))),
            Some(msg) => {
                rejected.store(true, std::sync::atomic::Ordering::Relaxed);
                report(OpProgress::Line(format!(" ! [rejected] {refname} ({msg})")));
            }
        }
        Ok(())
    });
    cb
}

/// Create a commit on HEAD from the current index, running the pre-commit and
/// commit-msg hooks. `parents` are the new commit's parents (HEAD for a normal
/// commit, HEAD's parents for an amend). With `check_empty`, refuse a commit
/// that would not change the tree.
fn make_commit(
    repo: &Repository,
    message: &str,
    parents: &[git2::Commit],
    check_empty: bool,
    verify: bool,
) -> Result<(), GitError> {
    // Refresh from disk first: a plain `git add` since the backend last touched
    // the index must not be lost by the hook machinery writing the stale cached
    // index back out.
    sync_index(repo)?;

    let mut msg = message.to_owned();
    if verify {
        run_commit_hooks(repo, &mut msg)?;
    }
    // After the commit-msg hook, which a Gerrit setup may use to add its own
    // Change-Id, so rgit never stamps a second one.
    let msg = crate::change_id::ensure(repo, &msg);

    let mut index = repo.index()?;
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    // Committing a stopped merge records every merged head as a parent, as git does.
    let merging = repo.state() == git2::RepositoryState::Merge;
    let mut merge_heads = Vec::new();
    if merging {
        for line in std::fs::read_to_string(repo.path().join("MERGE_HEAD"))?.lines() {
            merge_heads.push(repo.find_commit(Oid::from_str(line.trim())?)?);
        }
    }
    if check_empty && !merging && parents.first().is_some_and(|p| p.tree_id() == tree_oid) {
        return Err(GitError::NothingToCommit);
    }

    let parent_refs: Vec<&git2::Commit> = parents.iter().chain(&merge_heads).collect();
    let sig = repo.signature()?;
    repo.commit(Some("HEAD"), &sig, &sig, &msg, &tree, &parent_refs)?;
    // Like `git commit`: the in-progress merge, cherry-pick or revert is done, but
    // a cherry-pick sequence keeps its todo for `--continue`.
    for file in [
        "MERGE_HEAD",
        "MERGE_MODE",
        "MERGE_MSG",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        let _ = std::fs::remove_file(repo.path().join(file));
    }

    if verify {
        let _ = git2_hooks::hooks_post_commit(repo, None);
    }
    Ok(())
}

/// Git-style summary of the current HEAD commit: the `[branch sha] subject`
/// line and a `N files changed, …` diffstat, mirroring `git commit`'s output.
fn commit_report(repo: &Repository) -> Vec<String> {
    let Ok(head) = repo.head() else {
        return Vec::new();
    };
    let Ok(commit) = head.peel_to_commit() else {
        return Vec::new();
    };
    let short = commit
        .as_object()
        .short_id()
        .ok()
        .and_then(|b| b.as_str().ok().map(str::to_owned))
        .unwrap_or_default();
    let branch = head.shorthand().unwrap_or("HEAD");
    let subject = commit.summary().ok().flatten().unwrap_or("");
    let head_line = if commit.parent_count() == 0 {
        format!("[{branch} (root-commit) {short}] {subject}")
    } else {
        format!("[{branch} {short}] {subject}")
    };
    let mut out = vec![head_line];

    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    if let Ok(tree) = commit.tree()
        && let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
        && let Ok(stats) = diff.stats()
    {
        let plural = |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "s" });
        let mut line = format!(" {} changed", plural(stats.files_changed(), "file"));
        if stats.insertions() > 0 {
            line += &format!(", {}(+)", plural(stats.insertions(), "insertion"));
        }
        if stats.deletions() > 0 {
            line += &format!(", {}(-)", plural(stats.deletions(), "deletion"));
        }
        out.push(line);
    }
    out
}

/// The git-style line git prints after `git stash`: `Saved working directory
/// and index state WIP on <branch>: <short-oid> <subject>`. Stash does not move
/// HEAD, so the base commit it reports is the current HEAD.
fn stash_saved_line(repo: &Repository) -> String {
    let Ok(head) = repo.head() else {
        return "Saved working directory and index state".to_owned();
    };
    let branch = head.shorthand().unwrap_or("HEAD");
    let (short, subject) = match head.peel_to_commit() {
        Ok(commit) => {
            let short = commit
                .as_object()
                .short_id()
                .ok()
                .and_then(|b| b.as_str().ok().map(str::to_owned))
                .unwrap_or_default();
            (
                short,
                commit.summary().ok().flatten().unwrap_or("").to_owned(),
            )
        }
        Err(_) => (String::new(), String::new()),
    };
    format!("Saved working directory and index state WIP on {branch}: {short} {subject}")
}

/// Rewrite HEAD from the index, keeping its parents. Runs the commit hooks when
/// `verify` is set.
fn make_amend(repo: &Repository, message: &str, verify: bool) -> Result<(), GitError> {
    let head = repo.head()?.peel_to_commit()?;
    let old_msg = head.message().unwrap_or("").to_owned();
    let mut msg = message.to_owned();
    if verify {
        run_commit_hooks(repo, &mut msg)?;
    }
    // Amend rewrites the commit; carry the original change id forward so the
    // logical change keeps its identity even though the oid changes.
    let msg = crate::change_id::preserve(repo, &old_msg, &msg);

    sync_index(repo)?;
    let mut index = repo.index()?;
    let tree = repo.find_tree(index.write_tree()?)?;
    let sig = repo.signature()?;
    // Commit::amend rewrites HEAD keeping its parents, which the ref-updating
    // commit refuses ("current tip is not the first parent").
    head.amend(
        Some("HEAD"),
        Some(&sig),
        Some(&sig),
        None,
        Some(&msg),
        Some(&tree),
    )?;

    if verify {
        let _ = git2_hooks::hooks_post_commit(repo, None);
    }
    Ok(())
}

/// The first-parent chain from HEAD down to and including `target`, newest
/// first. Errors if `target` is not an ancestor of HEAD on that chain.
fn first_parent_chain(repo: &Repository, target: Oid) -> Result<Vec<git2::Commit<'_>>, GitError> {
    let mut chain = Vec::new();
    let mut c = repo.head()?.peel_to_commit()?;
    loop {
        let id = c.id();
        chain.push(c);
        if id == target {
            return Ok(chain);
        }
        c = match chain.last().unwrap().parent(0) {
            Ok(p) => p,
            Err(_) => {
                return Err(GitError::Other(
                    "revision is not an ancestor of HEAD".to_owned(),
                ));
            }
        };
    }
}

/// The ref HEAD points at (a branch), for updating after a history rewrite.
/// Errors on a detached HEAD, which these edits do not support.
fn head_branch_ref(repo: &Repository) -> Result<String, GitError> {
    let head = repo.head()?;
    if head.is_branch() {
        Ok(head.name().unwrap_or("HEAD").to_owned())
    } else {
        Err(GitError::Other(
            "HEAD is detached; edit a commit on a branch".to_owned(),
        ))
    }
}

/// Change `target`'s message, keeping its tree and parents, and re-create every
/// commit above it with the rewritten parent (their trees are unchanged, so no
/// merge is needed). Updates the current branch to the new tip.
fn reword_commit(repo: &Repository, target: Oid, new_message: &str) -> Result<(), GitError> {
    let branch_ref = head_branch_ref(repo)?;
    let chain = first_parent_chain(repo, target)?; // [HEAD, ..., target]
    let sig = repo.signature()?;
    let mut new_tip = target;
    // Rebuild bottom-up (target first).
    for commit in chain.iter().rev() {
        let is_target = commit.id() == target;
        let message = if is_target {
            crate::change_id::preserve(repo, commit.message().unwrap_or(""), new_message)
        } else {
            commit.message().unwrap_or("").to_owned()
        };
        let parents: Vec<git2::Commit> = if is_target {
            (0..commit.parent_count())
                .filter_map(|k| commit.parent(k).ok())
                .collect()
        } else {
            vec![repo.find_commit(new_tip)?]
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        new_tip = repo.commit(
            None,
            &commit.author(),
            &sig,
            &message,
            &commit.tree()?,
            &parent_refs,
        )?;
    }
    repo.reference(&branch_ref, new_tip, true, "rgit reword")?;
    Ok(())
}

/// Set one path in an in-memory index to a tree entry (mode + blob id). Only the
/// fields git needs to write a tree are set; the rest are zero.
fn index_set(index: &mut git2::Index, path: &str, id: Oid, mode: i32) -> Result<(), GitError> {
    let entry = git2::IndexEntry {
        ctime: git2::IndexTime::new(0, 0),
        mtime: git2::IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: mode as u32,
        uid: 0,
        gid: 0,
        file_size: 0,
        id,
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    };
    index.add(&entry)?;
    Ok(())
}

/// Cherry-pick `commits` (oldest first) one by one onto `base`, returning the new
/// tip. A conflict aborts with `GitError::Conflict` and leaves refs untouched
/// (only dangling objects are written), so the caller's op-log snapshot fully
/// recovers. Each replayed commit keeps its author and message.
fn replay_onto(repo: &Repository, commits: &[&git2::Commit], base: Oid) -> Result<Oid, GitError> {
    let sig = repo.signature()?;
    let mut tip = base;
    for c in commits {
        let our = repo.find_commit(tip)?;
        let mut index = repo.cherrypick_commit(c, &our, 0, None)?;
        if index.has_conflicts() {
            return Err(GitError::Conflict(format!(
                "conflict replaying {}",
                &c.id().to_string()[..7]
            )));
        }
        let tree = repo.find_tree(index.write_tree_to(repo)?)?;
        tip = repo.commit(
            None,
            &c.author(),
            &sig,
            c.message().unwrap_or(""),
            &tree,
            &[&our],
        )?;
    }
    Ok(tip)
}

/// Run the pre-commit and commit-msg hooks, letting commit-msg rewrite `msg`.
fn run_commit_hooks(repo: &Repository, msg: &mut String) -> Result<(), GitError> {
    run_hook("pre-commit", git2_hooks::hooks_pre_commit(repo, None))?;
    run_hook("commit-msg", git2_hooks::hooks_commit_msg(repo, None, msg))?;
    Ok(())
}

/// Turn a hook result into an error when the hook ran and failed, surfacing its
/// output so the user sees why.
fn run_hook(
    name: &str,
    result: Result<git2_hooks::HookResult, git2_hooks::HooksError>,
) -> Result<(), GitError> {
    match result.map_err(|e| GitError::Hook(e.to_string()))? {
        git2_hooks::HookResult::Run(r) if !r.is_successful() => {
            let detail = if r.stderr.trim().is_empty() {
                r.stdout.trim().to_owned()
            } else {
                r.stderr.trim().to_owned()
            };
            Err(GitError::Hook(format!("{name} hook failed: {detail}")))
        }
        _ => Ok(()),
    }
}

/// Apply a single hunk (selected by its new-side start line) of `diff` to the
/// index, using libgit2's own patch machinery.
/// Whether a pathspec (file, folder or glob) matches `path`, as git matches it.
pub fn pathspec_matches(specs: &[String], path: &str) -> bool {
    Pathspec::new(specs.iter())
        .is_ok_and(|spec| spec.matches_path(Path::new(path), PathspecFlags::DEFAULT))
}

/// `value` as git's `config --bool` or `--int` prints it, else unchanged.
pub fn config_value(value: &str, as_bool: bool, as_int: bool) -> Result<String, GitError> {
    Ok(if as_bool {
        git2::Config::parse_bool(value)?.to_string()
    } else if as_int {
        git2::Config::parse_i64(value)?.to_string()
    } else {
        value.to_owned()
    })
}

/// The config file(s) of `scope`. Writes without a scope go to the repository's
/// own file.
fn open_config(
    repo: &Repository,
    scope: ConfigScope,
    write: bool,
) -> Result<git2::Config, GitError> {
    use git2::{Config, ConfigLevel};
    let local = repo.commondir().join("config");
    let env_global = std::env::var_os("GIT_CONFIG_GLOBAL").map(PathBuf::from);
    let global = || {
        env_global
            .clone()
            .or_else(|| Config::find_global().ok())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".gitconfig")))
            .ok_or_else(|| GitError::Other("no global config file: HOME is not set".to_owned()))
    };
    Ok(match scope {
        ConfigScope::Global => Config::open(&global()?)?,
        ConfigScope::Local => Config::open(&local)?,
        ConfigScope::Any if write => Config::open(&local)?,
        ConfigScope::Any if env_global.is_some() => {
            // libgit2 ignores GIT_CONFIG_GLOBAL, so stack the files as git does.
            let mut config = Config::new()?;
            for (path, level) in [
                (Config::find_system().ok(), ConfigLevel::System),
                (Config::find_xdg().ok(), ConfigLevel::XDG),
                (env_global.clone(), ConfigLevel::Global),
                (Some(local), ConfigLevel::Local),
            ] {
                if let Some(path) = path.filter(|p| p.exists()) {
                    config.add_file(&path, level, false)?;
                }
            }
            config
        }
        ConfigScope::Any => repo.config()?,
    })
}

/// `patch` with its two sides swapped, so applying it undoes the original
/// (libgit2 cannot apply in reverse).
fn reverse_patch(patch: &str) -> Result<String, GitError> {
    let bad = || GitError::Other("malformed patch".to_owned());
    let side = |p: &str, from: &str, to: &str| {
        p.strip_prefix(from)
            .map_or_else(|| p.to_owned(), |rest| format!("{to}{rest}"))
    };
    const SWAPS: &[(&str, &str)] = &[
        ("new file mode ", "deleted file mode "),
        ("deleted file mode ", "new file mode "),
    ];
    // Header pairs whose values swap while the lines keep their order.
    const PAIRS: &[(&str, &str)] = &[("old mode ", "new mode "), ("rename from ", "rename to ")];
    let mut out = String::with_capacity(patch.len());
    let (mut old_left, mut new_left) = (0u32, 0u32);
    let mut minus = String::new();
    let mut held = String::new();
    for line in patch.split_inclusive('\n') {
        if old_left > 0 || new_left > 0 {
            match line.as_bytes()[0] {
                b'+' => {
                    new_left = new_left.saturating_sub(1);
                    out.push('-');
                    out.push_str(&line[1..]);
                }
                b'-' => {
                    old_left = old_left.saturating_sub(1);
                    out.push('+');
                    out.push_str(&line[1..]);
                }
                b'\\' => out.push_str(line),
                _ => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                    out.push_str(line);
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ -") {
            let (ranges, tail) = rest.split_once(" @@").ok_or_else(bad)?;
            let (old, new) = ranges.split_once(" +").ok_or_else(bad)?;
            let count = |r: &str| match r.split_once(',') {
                Some((_, n)) => n.parse::<u32>().map_err(|_| bad()),
                None => Ok(1),
            };
            old_left = count(old)?;
            new_left = count(new)?;
            out.push_str(&format!("@@ -{new} +{old} @@{tail}"));
        } else if let Some(p) = line.strip_prefix("--- ") {
            minus = p.to_owned();
        } else if let Some(p) = line.strip_prefix("+++ ") {
            out.push_str(&format!(
                "--- {}+++ {}",
                side(p, "b/", "a/"),
                side(&minus, "a/", "b/")
            ));
        } else if let Some(rest) = line.strip_prefix("diff --git a/") {
            let rest = rest.trim_end_matches('\n');
            let (a, b) = rest.rsplit_once(" b/").ok_or_else(bad)?;
            out.push_str(&format!("diff --git a/{b} b/{a}\n"));
        } else if line.starts_with("GIT binary patch") || line.starts_with("Binary files") {
            return Err(GitError::Other("cannot reverse a binary patch".to_owned()));
        } else if let Some((from, to)) = SWAPS.iter().find(|(f, _)| line.starts_with(f)) {
            out.push_str(to);
            out.push_str(&line[from.len()..]);
        } else if let Some((first, _)) = PAIRS.iter().find(|(f, _)| line.starts_with(f)) {
            held = line[first.len()..].to_owned();
        } else if let Some((first, second)) = PAIRS.iter().find(|(_, s)| line.starts_with(s)) {
            out.push_str(&format!("{first}{}{second}{held}", &line[second.len()..]));
        } else if let Some((a, rest)) = line.strip_prefix("index ").and_then(|r| r.split_once(".."))
        {
            let end = rest.find([' ', '\n']).unwrap_or(rest.len());
            out.push_str(&format!("index {}..{a}{}", &rest[..end], &rest[end..]));
        } else {
            out.push_str(line);
        }
    }
    Ok(out)
}

/// git's `format-patch` file name: `0001-` and the subject with runs of other
/// characters turned into `-`, 64 bytes at most with the suffix.
fn patch_file_name(n: usize, subject: &str) -> String {
    let mut name = format!("{n:04}-");
    let start = name.len();
    let mut gap = false;
    let mut chars = subject.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            if gap && name.len() > start {
                name.push('-');
            }
            gap = false;
            name.push(c);
            while c == '.' && chars.peek() == Some(&'.') {
                chars.next();
            }
        } else {
            gap = true;
        }
    }
    while name.len() > start && name.ends_with(['.', '-']) {
        name.pop();
    }
    name.truncate(57);
    name + ".patch"
}

/// A tar of `(name, git file mode, content)` entries.
fn tar_archive(entries: &[(String, i32, Vec<u8>)], mtime: i64) -> Result<Vec<u8>, GitError> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, mode, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mtime(mtime.max(0) as u64);
        header.set_mode(match mode {
            0o040000 | 0o100755 => 0o775,
            0o120000 => 0o777,
            _ => 0o664,
        });
        match mode {
            0o040000 => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                builder.append_data(&mut header, name, std::io::empty())?;
            }
            0o120000 => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                builder.append_link(&mut header, name, &*String::from_utf8_lossy(data))?;
            }
            _ => {
                header.set_size(data.len() as u64);
                builder.append_data(&mut header, name, data.as_slice())?;
            }
        }
    }
    Ok(builder.into_inner()?)
}

/// A zip of `(name, git file mode, content)` entries, deflated where that
/// makes them smaller.
fn zip_archive(entries: &[(String, i32, Vec<u8>)], mtime: i64) -> Vec<u8> {
    use std::io::Write;
    let (time, date) = dos_time(mtime);
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, mode, data) in entries {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let mut deflate =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        let deflated = deflate
            .write_all(data)
            .and_then(|()| deflate.finish())
            .unwrap_or_default();
        let (method, body) = if !data.is_empty() && deflated.len() < data.len() {
            (8u16, &deflated)
        } else {
            (0u16, data)
        };
        let flags: u16 = if name.is_ascii() { 0 } else { 0x0800 };
        let offset = out.len() as u32;
        // Fields shared by the local header and the central directory entry.
        let mut common = Vec::new();
        for v in [20u16, flags, method, time, date] {
            common.extend(v.to_le_bytes());
        }
        for v in [crc.sum(), body.len() as u32, data.len() as u32] {
            common.extend(v.to_le_bytes());
        }
        common.extend((name.len() as u16).to_le_bytes());
        common.extend(0u16.to_le_bytes());
        out.extend(0x04034b50u32.to_le_bytes());
        out.extend(&common);
        out.extend(name.as_bytes());
        out.extend(body);
        let attrs = (*mode as u32) << 16 | u32::from(*mode == 0o040000) << 4;
        central.extend(0x02014b50u32.to_le_bytes());
        central.extend(0x0314u16.to_le_bytes());
        central.extend(&common);
        for v in [0u16, 0, 0] {
            central.extend(v.to_le_bytes());
        }
        central.extend(attrs.to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let (start, size, count) = (out.len() as u32, central.len() as u32, entries.len() as u16);
    out.extend(central);
    out.extend(0x06054b50u32.to_le_bytes());
    for v in [0u16, 0, count, count] {
        out.extend(v.to_le_bytes());
    }
    out.extend(size.to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// A unix time as the MS-DOS (time, date) pair zip stores.
fn dos_time(secs: i64) -> (u16, u16) {
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (yoe + era * 400 + i64::from(month <= 2)).clamp(1980, 2107);
    let time = (rem / 3600) << 11 | (rem % 3600 / 60) << 5 | ((rem % 60) / 2);
    let date = (year - 1980) << 9 | month << 5 | day;
    (time as u16, date as u16)
}

fn did_not_match(path: &str) -> GitError {
    GitError::Other(format!("pathspec '{path}' did not match any files"))
}

/// Index paths a pathspec matches.
fn index_matches(index: &git2::Index, path: &str) -> Result<Vec<String>, GitError> {
    let spec = Pathspec::new([path])?;
    let list = spec.match_index(index, PathspecFlags::DEFAULT)?;
    Ok(list
        .entries()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// Fail like git when a pathspec names nothing in the working tree, the index
/// or HEAD.
fn no_match(repo: &Repository, path: &str) -> Result<(), GitError> {
    let spec = Pathspec::new([path])?;
    let flags = PathspecFlags::DEFAULT;
    let hit = spec.match_workdir(repo, flags)?.entries().len() > 0
        || spec.match_index(&repo.index()?, flags)?.entries().len() > 0
        || repo
            .head()
            .and_then(|h| h.peel_to_tree())
            .and_then(|t| spec.match_tree(&t, flags))
            .is_ok_and(|m| m.entries().len() > 0);
    if hit {
        Ok(())
    } else {
        Err(did_not_match(path))
    }
}

fn apply_one_hunk(
    repo: &Repository,
    diff: &Diff,
    path: &str,
    new_start: u32,
) -> Result<(), GitError> {
    let want = path.to_owned();
    let mut opts = ApplyOptions::new();
    opts.hunk_callback(|hunk| hunk.is_some_and(|h| h.new_start() == new_start));
    opts.delta_callback(move |delta| {
        delta.is_some_and(|d| {
            d.new_file()
                .path()
                .is_some_and(|p| p.to_string_lossy() == want)
        })
    });
    repo.apply(diff, ApplyLocation::Index, Some(&mut opts))?;
    Ok(())
}

/// Check out the local branch `name`, updating the worktree and HEAD. A safe
/// checkout errors rather than clobbering conflicting local changes.
/// Drive a libgit2 rebase to completion, reporting git-style progress and
/// aborting on the first conflict. `onto` is `None` for a plain rebase onto
/// `upstream`, or `Some` for a three-point `--onto` rebase.
fn run_rebase(
    repo: &Repository,
    upstream: &git2::AnnotatedCommit,
    onto: Option<&git2::AnnotatedCommit>,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let mut rebase = repo.rebase(None, Some(upstream), onto, None)?;
    let sig = repo.signature()?;
    while let Some(op) = rebase.next() {
        let op = op?;
        if let Some(summary) = repo
            .find_commit(op.id())
            .ok()
            .and_then(|c| c.summary().ok().flatten().map(str::to_owned))
        {
            report(OpProgress::Line(format!("Applying: {summary}")));
        }
        if repo.index()?.has_conflicts() {
            rebase.abort()?;
            report(OpProgress::Line(
                "CONFLICT: rebase hit a conflict and was aborted".to_owned(),
            ));
            return Err(GitError::Conflict(
                "rebase hit a conflict and was aborted".into(),
            ));
        }
        rebase.commit(None, &sig, None)?;
    }
    rebase.finish(Some(&sig))?;
    report(OpProgress::Line(
        "Successfully rebased and updated HEAD.".to_owned(),
    ));
    Ok(())
}

/// The commit the smartlog treats as the trunk: HEAD's upstream if it has one,
/// else the first existing local `main`/`master`/`develop`/`trunk`.
fn detect_trunk(repo: &Repository) -> Option<git2::Oid> {
    if let Ok(head) = repo.head()
        && let Ok(name) = head.shorthand()
        && let Ok(local) = repo.find_branch(name, BranchType::Local)
        && let Ok(up) = local.upstream()
        && let Some(oid) = up.get().target()
    {
        return Some(oid);
    }
    for name in ["main", "master", "develop", "trunk"] {
        if let Ok(branch) = repo.find_branch(name, BranchType::Local)
            && let Some(oid) = branch.get().target()
        {
            return Some(oid);
        }
    }
    None
}

fn checkout(repo: &Repository, name: &str) -> Result<(), GitError> {
    let refname = format!("refs/heads/{name}");
    let object = repo.revparse_single(&refname)?;
    repo.checkout_tree(&object, Some(CheckoutBuilder::new().safe()))?;
    repo.set_head(&refname)?;
    Ok(())
}

/// The unstaged (index-vs-worktree) diff for a single path.
fn worktree_diff<'r>(repo: &'r Repository, path: &str) -> Result<Diff<'r>, GitError> {
    let mut opts = DiffOptions::new();
    opts.pathspec(path);
    Ok(repo.diff_index_to_workdir(None, Some(&mut opts))?)
}

/// Build a patch that applies only `selected` line indices of the hunk at
/// `new_start`. Unselected additions are dropped; unselected deletions are kept
/// as context so the patch still applies. With `reverse`, additions and
/// deletions swap (used to un-stage selected lines).
fn partial_hunk_patch(
    diff: &Diff,
    path: &str,
    new_start: u32,
    selected: &[usize],
    reverse: bool,
) -> Result<String, GitError> {
    let selected: std::collections::HashSet<usize> = selected.iter().copied().collect();

    for idx in 0..diff.deltas().len() {
        let delta_path = diff.get_delta(idx).and_then(|d| {
            d.new_file()
                .path()
                .map(|p| p.to_string_lossy().into_owned())
        });
        if delta_path.as_deref() != Some(path) {
            continue;
        }
        let Some(patch) = Patch::from_diff(diff, idx)? else {
            continue;
        };
        for h in 0..patch.num_hunks() {
            let (hunk, _) = patch.hunk(h)?;
            if hunk.new_start() != new_start {
                continue;
            }

            let mut body = String::new();
            let mut old_count = 0u32;
            let mut new_count = 0u32;
            for j in 0..patch.num_lines_in_hunk(h)? {
                let line = patch.line_in_hunk(h, j)?;
                let content = String::from_utf8_lossy(line.content());
                let effective = match (line.origin(), reverse) {
                    ('+', true) => '-',
                    ('-', true) => '+',
                    (o, _) => o,
                };
                match effective {
                    '+' => {
                        if selected.contains(&j) {
                            body.push('+');
                            body.push_str(&content);
                            new_count += 1;
                        }
                    }
                    '-' => {
                        if selected.contains(&j) {
                            body.push('-');
                            body.push_str(&content);
                            old_count += 1;
                        } else {
                            body.push(' ');
                            body.push_str(&content);
                            old_count += 1;
                            new_count += 1;
                        }
                    }
                    ' ' | '=' => {
                        body.push(' ');
                        body.push_str(&content);
                        old_count += 1;
                        new_count += 1;
                    }
                    _ => {}
                }
            }

            let (old_start, new_start) = if reverse {
                (hunk.new_start(), hunk.old_start())
            } else {
                (hunk.old_start(), hunk.new_start())
            };
            return Ok(format!(
                "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n\
                 @@ -{old_start},{old_count} +{new_start},{new_count} @@\n{body}"
            ));
        }
    }
    Err(GitError::HunkNotFound {
        path: path.to_owned(),
        new_start,
    })
}

/// Build a reversed unified-diff patch for one hunk of a staged diff, so
/// applying it to the index un-stages that hunk.
fn reverse_hunk_patch(diff: &Diff, path: &str, new_start: u32) -> Result<String, GitError> {
    for idx in 0..diff.deltas().len() {
        let delta_path = diff.get_delta(idx).and_then(|d| {
            d.new_file()
                .path()
                .map(|p| p.to_string_lossy().into_owned())
        });
        if delta_path.as_deref() != Some(path) {
            continue;
        }
        let Some(patch) = Patch::from_diff(diff, idx)? else {
            continue;
        };
        for h in 0..patch.num_hunks() {
            let (hunk, _) = patch.hunk(h)?;
            if hunk.new_start() != new_start {
                continue;
            }
            let mut out = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
            // The patch holds only this hunk, so it lands where it sits in the
            // index, not at its HEAD line (other staged hunks shift that). An
            // empty side names the line before it, hence the +1.
            out.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                hunk.new_start(),
                hunk.new_lines(),
                hunk.new_start() + u32::from(hunk.new_lines() == 0),
                hunk.old_lines(),
            ));
            for l in 0..patch.num_lines_in_hunk(h)? {
                let line = patch.line_in_hunk(h, l)?;
                let prefix = match line.origin() {
                    '+' => '-',
                    '-' => '+',
                    _ => ' ',
                };
                out.push(prefix);
                out.push_str(&String::from_utf8_lossy(line.content()));
            }
            return Ok(out);
        }
    }
    Err(GitError::HunkNotFound {
        path: path.to_owned(),
        new_start,
    })
}

fn collect_head(repo: &Repository, newest: Option<&Commit>) -> Result<Head, GitError> {
    let mut head = Head {
        summary: newest.map(|c| c.summary.clone()),
        when: newest.map(|c| c.when.clone()),
        ..Head::default()
    };

    match repo.head() {
        Ok(head_ref) => {
            head.detached = repo.head_detached()?;
            if !head.detached {
                head.branch = head_ref.shorthand().ok().map(str::to_owned);
            }
            head.oid = newest.map(|c| c.short_id.clone());
            if let Some(branch) = head.branch.clone() {
                fill_upstream(repo, &branch, &mut head);
            }
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            head.branch = repo
                .find_reference("HEAD")
                .ok()
                .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
                .map(|t| t.strip_prefix("refs/heads/").unwrap_or(&t).to_owned());
        }
        Err(e) => return Err(e.into()),
    }

    Ok(head)
}

fn fill_upstream(repo: &Repository, branch: &str, head: &mut Head) {
    let Ok(local) = repo.find_branch(branch, BranchType::Local) else {
        return;
    };
    let mut configured = false;
    if let Ok(upstream) = local.upstream() {
        if let Ok(Some(name)) = upstream.name() {
            head.upstream = Some(name.to_owned());
            configured = true;
        }
        if let (Some(local_oid), Some(up_oid)) = (local.get().target(), upstream.get().target())
            && let Ok((ahead, behind)) = repo.graph_ahead_behind(local_oid, up_oid)
        {
            head.ahead = ahead;
            head.behind = behind;
        }
    }
    fill_remotes(repo, &local, branch, head);
    // No tracking configured, but the branch exists on a remote by name: treat
    // that as the effective upstream so "published" state and the REMOTE overview
    // agree. Prefer origin (the usual push target), else the first such remote.
    if !configured
        && let Some((name, ahead, behind)) = head
            .remotes
            .iter()
            .find(|(n, _, _)| n.starts_with("origin/"))
            .or_else(|| head.remotes.first())
    {
        head.upstream = Some(name.clone());
        head.ahead = *ahead;
        head.behind = *behind;
    }
}

/// Ahead/behind of `branch` against every remote that has a branch of the same
/// name, for the REMOTE overview (the tracking upstream is only one of these).
fn fill_remotes(repo: &Repository, local: &git2::Branch, branch: &str, head: &mut Head) {
    let Some(local_oid) = local.get().target() else {
        return;
    };
    let Ok(remotes) = repo.remotes() else {
        return;
    };
    for i in 0..remotes.len() {
        let Ok(Some(remote)) = remotes.get(i) else {
            continue;
        };
        let tracking = format!("{remote}/{branch}");
        if let Ok(rb) = repo.find_branch(&tracking, BranchType::Remote)
            && let Some(roid) = rb.get().target()
            && let Ok((ahead, behind)) = repo.graph_ahead_behind(local_oid, roid)
        {
            head.remotes.push((tracking, ahead, behind));
        }
    }
}

fn collect_entries(repo: &Repository) -> Result<Vec<StatusEntry>, GitError> {
    // Worktree renames stay off, as in `git status`: pairing a deletion with an
    // untracked file would make one entry stand for two paths that stage apart.
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);

    let statuses = repo.statuses(Some(&mut opts))?;
    let mut entries = Vec::with_capacity(statuses.len());

    for entry in statuses.iter() {
        let status = entry.status();
        if status.contains(Status::IGNORED) {
            continue;
        }
        // `StatusEntry::path` is the pre-rename path; a rename lives at its new one.
        let rename = entry
            .head_to_index()
            .filter(|d| d.status() == Delta::Renamed);
        let Some(path) = rename
            .as_ref()
            .and_then(|d| d.new_file().path())
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| entry.path().ok().map(str::to_owned))
        else {
            continue;
        };
        let orig_path = rename
            .and_then(|d| d.old_file().path())
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|old| *old != path);

        entries.push(StatusEntry {
            path,
            orig_path,
            index: index_code(status),
            worktree: worktree_code(status),
        });
    }

    Ok(entries)
}

fn index_code(s: Status) -> StatusCode {
    if s.contains(Status::INDEX_NEW) {
        StatusCode::Added
    } else if s.contains(Status::INDEX_MODIFIED) {
        StatusCode::Modified
    } else if s.contains(Status::INDEX_DELETED) {
        StatusCode::Deleted
    } else if s.contains(Status::INDEX_RENAMED) {
        StatusCode::Renamed
    } else if s.contains(Status::INDEX_TYPECHANGE) {
        StatusCode::TypeChanged
    } else {
        StatusCode::Unmodified
    }
}

fn worktree_code(s: Status) -> StatusCode {
    if s.contains(Status::WT_NEW) {
        StatusCode::Untracked
    } else if s.contains(Status::CONFLICTED) {
        StatusCode::Unmerged
    } else if s.contains(Status::WT_MODIFIED) {
        StatusCode::Modified
    } else if s.contains(Status::WT_DELETED) {
        StatusCode::Deleted
    } else if s.contains(Status::WT_RENAMED) {
        StatusCode::Renamed
    } else if s.contains(Status::WT_TYPECHANGE) {
        StatusCode::TypeChanged
    } else {
        StatusCode::Unmodified
    }
}

fn repo_state(repo: &Repository) -> RepoState {
    use git2::RepositoryState as S;
    match repo.state() {
        S::Merge => RepoState::Merge,
        S::Rebase
        | S::RebaseInteractive
        | S::RebaseMerge
        | S::ApplyMailbox
        | S::ApplyMailboxOrRebase => RepoState::Rebase,
        S::CherryPick | S::CherryPickSequence => RepoState::CherryPick,
        S::Revert | S::RevertSequence => RepoState::Revert,
        S::Bisect => RepoState::Bisect,
        S::Clean => RepoState::Clean,
    }
}

/// The commits `revs` name, in the order a cherry-pick (oldest first) or a
/// revert (newest first) applies them. A rev is a commit or a range `A..B`.
fn pick_list(repo: &Repository, revs: &[String], revert: bool) -> Result<Vec<Oid>, GitError> {
    let commit = |rev: &str| -> Result<Oid, GitError> {
        let rev = if rev.is_empty() { "HEAD" } else { rev };
        Ok(repo.revparse_single(rev)?.peel_to_commit()?.id())
    };
    let mut todo = Vec::new();
    for rev in revs {
        let Some((from, to)) = rev.split_once("..") else {
            todo.push(commit(rev)?);
            continue;
        };
        let mut walk = repo.revwalk()?;
        let mut sort = git2::Sort::TOPOLOGICAL;
        if !revert {
            sort |= git2::Sort::REVERSE;
        }
        walk.set_sorting(sort)?;
        walk.push(commit(to)?)?;
        walk.hide(commit(from)?)?;
        for oid in walk {
            todo.push(oid?);
        }
    }
    Ok(todo)
}

/// A stopped cherry-pick or revert, as git's sequencer records it.
struct PickState {
    todo: Vec<Oid>,
    opts: crate::PickOptions,
    orig_head: Oid,
}

impl PickState {
    fn verb(&self) -> &'static str {
        if self.opts.revert {
            "revert"
        } else {
            "cherry-pick"
        }
    }
}

/// The commit a stopped cherry-pick or revert was applying.
fn pick_head(repo: &Repository) -> Option<Oid> {
    ["CHERRY_PICK_HEAD", "REVERT_HEAD"]
        .iter()
        .find_map(|f| std::fs::read_to_string(repo.path().join(f)).ok())
        .and_then(|s| Oid::from_str(s.trim()).ok())
}

/// Read git's sequencer state (`.git/sequencer`), which rgit and git both write.
fn read_pick_state(repo: &Repository) -> Result<PickState, GitError> {
    let seq = repo.path().join("sequencer");
    let Ok(todo) = std::fs::read_to_string(seq.join("todo")) else {
        // A single pick started by git itself keeps no todo.
        let Some(oid) = pick_head(repo) else {
            return Err(GitError::Other(
                "no cherry-pick or revert in progress".into(),
            ));
        };
        let opts = crate::PickOptions {
            revert: repo.path().join("REVERT_HEAD").exists(),
            ..Default::default()
        };
        let orig_head = repo.head()?.peel_to_commit()?.id();
        return Ok(PickState {
            todo: vec![oid],
            opts,
            orig_head,
        });
    };
    let mut opts = crate::PickOptions::default();
    let mut list = Vec::new();
    for line in todo.lines() {
        let mut words = line.split_whitespace();
        let (Some(verb), Some(rev)) = (words.next(), words.next()) else {
            continue;
        };
        if verb.starts_with('#') {
            continue;
        }
        opts.revert = verb == "revert";
        list.push(repo.revparse_single(rev)?.peel_to_commit()?.id());
    }
    if list.is_empty() {
        return Err(GitError::Other(
            "no cherry-pick or revert in progress".into(),
        ));
    }
    if let Ok(cfg) = git2::Config::open(&seq.join("opts")) {
        opts.no_commit = cfg.get_bool("options.no-commit").unwrap_or(false);
        opts.record_origin = cfg.get_bool("options.record-origin").unwrap_or(false);
        opts.mainline = cfg.get_i32("options.mainline").ok().map(|m| m as u32);
        opts.strategy_option = cfg.get_string("options.strategy-option").ok();
    }
    let head = std::fs::read_to_string(seq.join("head"))?;
    Ok(PickState {
        todo: list,
        opts,
        orig_head: Oid::from_str(head.trim())?,
    })
}

/// Record a stopped pick in git's sequencer format, so `rgit` or `git`
/// `cherry-pick`/`revert` `--continue`, `--skip` and `--abort` all work.
fn write_pick_state(
    repo: &Repository,
    todo: &[Oid],
    opts: &crate::PickOptions,
    orig_head: Oid,
    message: &str,
) -> Result<(), GitError> {
    let dir = repo.path();
    let verb = if opts.revert { "revert" } else { "pick" };
    if !opts.no_commit {
        let head = if opts.revert {
            "REVERT_HEAD"
        } else {
            "CHERRY_PICK_HEAD"
        };
        std::fs::write(dir.join(head), format!("{}\n", todo[0]))?;
    }
    std::fs::write(dir.join("MERGE_MSG"), message)?;
    let seq = dir.join("sequencer");
    std::fs::create_dir_all(&seq)?;
    std::fs::write(seq.join("head"), format!("{orig_head}\n"))?;
    let head = repo.head()?.peel_to_commit()?.id();
    std::fs::write(seq.join("abort-safety"), format!("{head}\n"))?;
    let mut list = String::new();
    for oid in todo {
        let commit = repo.find_commit(*oid)?;
        let subject = commit.summary().ok().flatten().unwrap_or("");
        list.push_str(&format!("{verb} {oid} {subject}\n"));
    }
    std::fs::write(seq.join("todo"), list)?;
    let _ = std::fs::remove_file(seq.join("opts"));
    let mut cfg = git2::Config::open(&seq.join("opts"))?;
    if opts.no_commit {
        cfg.set_bool("options.no-commit", true)?;
    }
    if opts.record_origin {
        cfg.set_bool("options.record-origin", true)?;
    }
    if let Some(m) = opts.mainline {
        cfg.set_i32("options.mainline", m as i32)?;
    }
    if let Some(side) = &opts.strategy_option {
        cfg.set_str("options.strategy-option", side)?;
    }
    Ok(())
}

/// Apply `todo` onto HEAD in order, committing each unless `no_commit`. A
/// conflict or an empty result stops the sequence with git's sequencer state.
fn run_picks(
    repo: &Repository,
    todo: &[Oid],
    opts: &crate::PickOptions,
    orig_head: Oid,
) -> Result<(), GitError> {
    let verb = if opts.revert { "revert" } else { "cherry-pick" };
    let mut ours = repo.find_tree(repo.index()?.write_tree()?)?;
    if !todo.is_empty() && !opts.no_commit && ours.id() != repo.head()?.peel_to_tree()?.id() {
        return Err(GitError::Conflict(
            "you have staged changes; commit or stash them first".into(),
        ));
    }
    let mut staged = None;
    for (i, &oid) in todo.iter().enumerate() {
        let commit = repo.find_commit(oid)?;
        let mut merged = pick_index(repo, &commit, &ours, opts)?;
        let message = pick_message(&commit, opts);
        let subject = commit.summary().ok().flatten().unwrap_or("").to_owned();
        let stop = if merged.has_conflicts() {
            repo.checkout_index(
                Some(&mut merged),
                Some(
                    CheckoutBuilder::new()
                        .safe()
                        .allow_conflicts(true)
                        .conflict_style_merge(true),
                ),
            )?;
            Some(format!(
                "could not apply {} ({subject}); resolve the conflicts, then run `rgit {verb} \
                 --continue` (or --skip / --abort)",
                short7(oid)
            ))
        } else {
            None
        };
        let tree = match stop {
            Some(_) => None,
            None => Some(repo.find_tree(merged.write_tree_to(repo)?)?),
        };
        let stop = match &tree {
            Some(tree) if !opts.no_commit && tree.id() == ours.id() => Some(format!(
                "the {verb} of {} ({subject}) is now empty; run `rgit {verb} --skip`",
                short7(oid)
            )),
            _ => stop,
        };
        if let Some(why) = stop {
            write_pick_state(repo, &todo[i..], opts, orig_head, &message)?;
            return Err(GitError::Conflict(why));
        }
        let tree = tree.expect("no stop means a clean tree");
        if opts.no_commit {
            ours = tree;
            staged = Some(merged);
            continue;
        }
        repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe()))?;
        let head = repo.head()?.peel_to_commit()?;
        let committer = repo.signature()?;
        let author = if opts.revert {
            committer.clone()
        } else {
            commit.author().to_owned()
        };
        repo.commit(Some("HEAD"), &author, &committer, &message, &tree, &[&head])?;
        ours = tree;
    }
    if let Some(mut index) = staged {
        repo.checkout_index(Some(&mut index), Some(CheckoutBuilder::new().safe()))?;
    }
    Ok(())
}

/// The index after applying `commit` (or, for a revert, its inverse) to `ours`.
fn pick_index(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    ours: &git2::Tree<'_>,
    opts: &crate::PickOptions,
) -> Result<git2::Index, GitError> {
    let parents = commit.parent_count();
    let parent = match opts.mainline {
        None if parents > 1 => {
            return Err(GitError::Other(format!(
                "commit {} is a merge but no --mainline was given",
                commit.id()
            )));
        }
        None => commit.parent(0).ok(),
        Some(m) if m == 0 || m as usize > parents => {
            return Err(GitError::Other(format!(
                "commit {} does not have parent {m}",
                commit.id()
            )));
        }
        Some(m) => Some(commit.parent(m as usize - 1)?),
    };
    let parent_tree = match parent {
        Some(p) => p.tree()?,
        None => repo.find_tree(repo.treebuilder(None)?.write()?)?,
    };
    let tree = commit.tree()?;
    let (base, theirs) = if opts.revert {
        (&tree, &parent_tree)
    } else {
        (&parent_tree, &tree)
    };
    let mut merge_opts = git2::MergeOptions::new();
    if let Some(side) = &opts.strategy_option {
        merge_opts.file_favor(file_favor(side)?);
    }
    Ok(repo.merge_trees(base, ours, theirs, Some(&merge_opts))?)
}

/// The libgit2 file favor for git's `-X ours` / `-X theirs`.
fn file_favor(side: &str) -> Result<git2::FileFavor, GitError> {
    match side {
        "ours" => Ok(git2::FileFavor::Ours),
        "theirs" => Ok(git2::FileFavor::Theirs),
        _ => Err(GitError::Other(format!(
            "unknown strategy option '{side}'; use ours or theirs"
        ))),
    }
}

/// The message git gives a cherry-picked or reverted commit.
fn pick_message(commit: &git2::Commit<'_>, opts: &crate::PickOptions) -> String {
    if opts.revert {
        let summary = commit.summary().ok().flatten().unwrap_or("commit");
        let mut msg = format!(
            "Revert \"{summary}\"\n\nThis reverts commit {}",
            commit.id()
        );
        if commit.parent_count() > 1
            && let Ok(parent) = commit.parent_id(opts.mainline.unwrap_or(1) as usize - 1)
        {
            msg.push_str(&format!(", reversing\nchanges made to {parent}"));
        }
        msg.push_str(".\n");
        return msg;
    }
    let mut msg = commit.message().unwrap_or("").trim_end().to_owned();
    msg.push('\n');
    if opts.record_origin {
        if !ends_in_trailers(&msg) {
            msg.push('\n');
        }
        msg.push_str(&format!("(cherry picked from commit {})\n", commit.id()));
    }
    msg
}

/// Whether a message ends in a trailer block (`Key: value` lines), which git's
/// `-x` line joins without a blank line.
fn ends_in_trailers(msg: &str) -> bool {
    let Some((_, last)) = msg.trim_end().rsplit_once("\n\n") else {
        return false;
    };
    last.lines().all(|l| {
        l.starts_with("(cherry picked from commit ")
            || l.split_once(": ").is_some_and(|(key, _)| {
                !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
    })
}

/// A git message file (MERGE_MSG) without its `#` comment lines.
fn strip_comments(msg: &str) -> String {
    let kept: Vec<&str> = msg.lines().filter(|l| !l.starts_with('#')).collect();
    format!("{}\n", kept.join("\n").trim_end())
}

/// Merge several heads at once. libgit2 merges only a single head, so this
/// runs git's octopus strategy.
fn octopus(
    backend: &Git2Backend,
    revs: &[String],
    opts: &crate::MergeOptions,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let mut args = vec!["merge", "--no-edit"];
    for (on, flag) in [
        (opts.no_ff, "--no-ff"),
        (opts.ff_only, "--ff-only"),
        (opts.squash, "--squash"),
        (opts.no_commit, "--no-commit"),
    ] {
        if on {
            args.push(flag);
        }
    }
    if let Some(message) = &opts.message {
        args.extend(["-m", message]);
    }
    if let Some(side) = &opts.strategy_option {
        args.extend(["-X", side]);
    }
    args.extend(revs.iter().map(String::as_str));
    for line in backend.run_git(&args, &[])?.lines() {
        report(OpProgress::Line(line.to_owned()));
    }
    Ok(())
}

/// A short relative age (`5s`, `12m`, `3h`, `9d`) from a commit time to now.
/// Whether `commit`'s diff against its first parent touched `path`, matched as an
/// exact path or a directory prefix. A root commit is compared to the empty tree.
/// Each line of `content` with the commit that last touched it; lines the
/// blame does not cover (not yet committed) get an empty id.
fn blame_lines(blame: Option<&git2::Blame>, content: &str) -> Vec<crate::BlameLine> {
    content
        .lines()
        .enumerate()
        .map(|(i, text)| match blame.and_then(|b| b.get_line(i + 1)) {
            Some(hunk) => crate::BlameLine {
                short_id: hunk.final_commit_id().to_string().chars().take(7).collect(),
                author: hunk
                    .final_signature()
                    .and_then(|s| s.name().ok().map(str::to_owned))
                    .unwrap_or_else(|| "?".to_owned()),
                line: text.to_owned(),
            },
            None => crate::BlameLine {
                short_id: String::new(),
                author: String::new(),
                line: text.to_owned(),
            },
        })
        .collect()
}

/// Whether `commit` changed `paths`. Like git's default history simplification,
/// a merge counts only when it differs from every parent it follows.
fn commit_touched_path(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    paths: &[String],
    first_parent: bool,
) -> bool {
    let Ok(tree) = commit.tree() else {
        return false;
    };
    let mut opts = DiffOptions::new();
    for p in paths.iter().filter(|p| *p != ".") {
        opts.pathspec(p);
    }
    let mut differs = |old: Option<&git2::Tree>| {
        repo.diff_tree_to_tree(old, Some(&tree), Some(&mut opts))
            .is_ok_and(|d| d.deltas().len() > 0)
    };
    let parents = if first_parent {
        commit.parent_count().min(1)
    } else {
        commit.parent_count()
    };
    if parents == 0 {
        return differs(None);
    }
    (0..parents).all(|i| {
        let old = commit.parent(i).ok().and_then(|p| p.tree().ok());
        differs(old.as_ref())
    })
}

/// Whether `commit` touched `path`; when it renamed the file, `path` moves to
/// the old name for older commits, as `git log --follow` does.
fn follow_path(repo: &Repository, commit: &git2::Commit<'_>, path: &mut String) -> bool {
    let Ok(tree) = commit.tree() else {
        return false;
    };
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let Ok(mut diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
        return false;
    };
    if diff
        .find_similar(Some(DiffFindOptions::new().renames(true)))
        .is_err()
    {
        return false;
    }
    let Some(delta) = diff
        .deltas()
        .find(|d| d.new_file().path() == Some(Path::new(path.as_str())))
    else {
        return false;
    };
    if delta.status() == Delta::Renamed
        && let Some(old) = delta.old_file().path()
    {
        *path = old.to_string_lossy().into_owned();
    }
    true
}

/// Add one `git log` revision to a walk: `rev`, `^rev`, `A..B` or `A...B`.
/// Returns whether it added a starting point (not only an exclusion).
fn push_rev(repo: &Repository, walk: &mut git2::Revwalk, rev: &str) -> Result<bool, GitError> {
    if let Some(neg) = rev.strip_prefix('^') {
        walk.hide(repo.revparse_single(neg)?.peel_to_commit()?.id())?;
        return Ok(false);
    }
    let spec = repo.revparse(rev)?;
    let oid = |o: Option<&git2::Object>| -> Result<Oid, GitError> {
        Ok(
            o.ok_or_else(|| GitError::Other(format!("bad revision '{rev}'")))?
                .peel_to_commit()?
                .id(),
        )
    };
    let from = oid(spec.from())?;
    if spec.mode().contains(git2::RevparseMode::SINGLE) {
        walk.push(from)?;
        return Ok(true);
    }
    let to = oid(spec.to())?;
    walk.push(to)?;
    if spec.mode().contains(git2::RevparseMode::MERGE_BASE) {
        walk.push(from)?;
        if let Ok(base) = repo.merge_base(from, to) {
            walk.hide(base)?;
        }
    } else {
        walk.hide(from)?;
    }
    Ok(true)
}

/// The two trees a `A..B` or `A...B` diff compares (merge base for `...`).
fn range_trees<'r>(
    repo: &'r Repository,
    range: &str,
) -> Result<(git2::Tree<'r>, git2::Tree<'r>), GitError> {
    let spec = repo.revparse(range)?;
    let (Some(from), Some(to)) = (spec.from(), spec.to()) else {
        return Err(GitError::Other(format!("bad revision '{range}'")));
    };
    let to_tree = to.peel_to_tree()?;
    let from_tree = if spec.mode().contains(git2::RevparseMode::MERGE_BASE) {
        let base = repo.merge_base(from.peel_to_commit()?.id(), to.peel_to_commit()?.id())?;
        repo.find_commit(base)?.tree()?
    } else {
        from.peel_to_tree()?
    };
    Ok((from_tree, to_tree))
}

/// Stash flags: always keep the index consistent; add untracked files only when
/// asked (git's `-u`).
fn stash_flags(include_untracked: bool) -> Option<git2::StashFlags> {
    if include_untracked {
        Some(git2::StashFlags::INCLUDE_UNTRACKED)
    } else {
        Some(git2::StashFlags::DEFAULT)
    }
}

/// A signature time in `git log`'s default form: `Thu Sep 24 23:24:06 2026 -0500`.
fn git_date(t: git2::Time) -> String {
    let off = i64::from(t.offset_minutes());
    let local = t.seconds() + off * 60;
    let days = local.div_euclid(86400);
    let secs = local.rem_euclid(86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MO: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {} {} {:02}:{:02}:{:02} {} {}{:02}{:02}",
        WD[days.rem_euclid(7) as usize],
        MO[(month - 1) as usize],
        day,
        secs / 3600,
        secs % 3600 / 60,
        secs % 60,
        year,
        if off < 0 { '-' } else { '+' },
        off.abs() / 60,
        off.abs() % 60,
    )
}

fn relative_age(then: i64, now: i64) -> String {
    let delta = (now - then).max(0);
    match delta {
        d if d < 60 => format!("{d}s"),
        d if d < 3600 => format!("{}m", d / 60),
        d if d < 86400 => format!("{}h", d / 3600),
        d => format!("{}d", d / 86400),
    }
}

fn collect_stashes(repo: &mut Repository) -> Vec<Stash> {
    let mut stashes = Vec::new();
    let _ = repo.stash_foreach(|index, message, _oid| {
        stashes.push(Stash {
            index,
            message: message.to_owned(),
        });
        true
    });
    stashes
}

/// Refs pointing at each commit, git --decorate style: a commit's local branch,
/// that branch's upstream, and any tags, keyed by oid. Ordered local, remote,
/// tag (HEAD's branch first) so a log row reads `<hash> <branch> <upstream>
/// <summary>`.
fn log_decorations(
    repo: &Repository,
) -> std::collections::HashMap<git2::Oid, Vec<crate::CommitRef>> {
    use crate::{CommitRef, RefKind};
    let mut map: std::collections::HashMap<git2::Oid, Vec<CommitRef>> =
        std::collections::HashMap::new();
    let head_branch = repo
        .head()
        .ok()
        .filter(|h| h.is_branch())
        .and_then(|h| h.shorthand().ok().map(str::to_owned));
    let Ok(refs) = repo.references() else {
        return map;
    };
    for r in refs.flatten() {
        let Ok(name) = r.name() else { continue };
        let (label, kind) = if let Some(b) = name.strip_prefix("refs/heads/") {
            (b.to_owned(), RefKind::Local)
        } else if let Some(b) = name.strip_prefix("refs/remotes/") {
            // origin/HEAD is a symbolic alias, not a real tip; skip it.
            if b.ends_with("/HEAD") {
                continue;
            }
            (b.to_owned(), RefKind::Remote)
        } else if let Some(t) = name.strip_prefix("refs/tags/") {
            (t.to_owned(), RefKind::Tag)
        } else {
            continue;
        };
        // Peel through annotated tags to the commit the ref ultimately names.
        let Ok(oid) = r.peel_to_commit().map(|c| c.id()) else {
            continue;
        };
        let head = kind == RefKind::Local && head_branch.as_deref() == Some(label.as_str());
        map.entry(oid).or_default().push(CommitRef {
            name: label,
            kind,
            head,
        });
    }
    for v in map.values_mut() {
        v.sort_by_key(|r| (kind_rank(r.kind), !r.head, r.name.clone()));
    }
    map
}

fn kind_rank(k: crate::RefKind) -> u8 {
    match k {
        crate::RefKind::Local => 0,
        crate::RefKind::Remote => 1,
        crate::RefKind::Tag => 2,
    }
}

/// Commits reachable from a local branch but not from any remote-tracking
/// branch, i.e. local-only work. Empty when the repo has no remote refs (nothing
/// to compare against), so a purely local repo does not mark everything unpushed.
fn unpushed_oids(repo: &Repository) -> std::collections::HashSet<git2::Oid> {
    let mut set = std::collections::HashSet::new();
    let mut remote_tips = Vec::new();
    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            if r.is_remote()
                && let Some(oid) = r.target()
            {
                remote_tips.push(oid);
            }
        }
    }
    if remote_tips.is_empty() {
        return set;
    }
    let Ok(mut walk) = repo.revwalk() else {
        return set;
    };
    if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
        for (branch, _) in branches.flatten() {
            if let Some(oid) = branch.get().target() {
                let _ = walk.push(oid);
            }
        }
    }
    for oid in &remote_tips {
        let _ = walk.hide(*oid);
    }
    for oid in walk.flatten() {
        set.insert(oid);
    }
    set
}

fn collect_recent(repo: &Repository) -> Vec<Commit> {
    let Ok(mut walk) = repo.revwalk() else {
        return Vec::new();
    };
    // Fails on an unborn branch; an empty log is the right answer there.
    if walk.push_head().is_err() {
        return Vec::new();
    }
    let unpushed = unpushed_oids(repo);
    let decorations = log_decorations(repo);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    walk.filter_map(Result::ok)
        .take(RECENT_LIMIT)
        .filter_map(|oid| repo.find_commit(oid).ok())
        .map(|commit| Commit {
            short_id: commit
                .as_object()
                .short_id()
                .ok()
                .and_then(|b| b.as_str().ok().map(str::to_owned))
                .unwrap_or_default(),
            summary: commit
                .summary()
                .ok()
                .flatten()
                .unwrap_or_default()
                .to_owned(),
            when: relative_age(commit.time().seconds(), now),
            refs: decorations.get(&commit.id()).cloned().unwrap_or_default(),
            unpushed: unpushed.contains(&commit.id()),
        })
        .collect()
}

/// The branch name (only when HEAD points at a branch) and the abbreviated HEAD
/// commit id for a repository/worktree. Both are `None` on an unborn branch.
fn worktree_head(repo: &Repository) -> (Option<String>, Option<String>) {
    let Ok(head) = repo.head() else {
        return (None, None);
    };
    let branch = head
        .is_branch()
        .then(|| head.shorthand().ok())
        .flatten()
        .map(str::to_owned);
    let short = head
        .peel_to_commit()
        .ok()
        .and_then(|c| c.as_object().short_id().ok())
        .and_then(|b| b.as_str().ok().map(str::to_owned));
    (branch, short)
}

/// Whether a repository/worktree has any uncommitted change: tracked edits,
/// staged files, or new (untracked) files. Ignored files do not count.
fn repo_is_dirty(repo: &Repository) -> bool {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).include_ignored(false);
    match repo.statuses(Some(&mut opts)) {
        Ok(statuses) => statuses
            .iter()
            .any(|e| !e.status().is_empty() && !e.status().contains(Status::IGNORED)),
        Err(_) => false,
    }
}

/// Run libgit2 rename detection over a display diff, then extract it. A moved
/// file collapses from a delete+add pair into one renamed delta showing only its
/// real content changes (none, for a pure move). Staging/patch-apply diffs skip
/// this: they key on exact per-file paths.
fn extract_with_renames(mut diff: Diff) -> Result<Vec<FileDiff>, GitError> {
    let mut fopts = DiffFindOptions::new();
    fopts.renames(true);
    diff.find_similar(Some(&mut fopts))?;
    extract(&diff)
}

fn extract(diff: &Diff) -> Result<Vec<FileDiff>, GitError> {
    let count = diff.deltas().len();
    let mut files = Vec::with_capacity(count);

    for idx in 0..count {
        let delta = diff.get_delta(idx).expect("delta in range");
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Rename/copy deltas carry a distinct source path; expose it so the UI
        // can show "old -> new" instead of a full delete plus add.
        let old_path = matches!(delta.status(), Delta::Renamed | Delta::Copied)
            .then(|| {
                delta
                    .old_file()
                    .path()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .flatten()
            .filter(|old| *old != path);
        let binary = delta.flags().is_binary();

        let mut hunks = Vec::new();
        if let Some(patch) = Patch::from_diff(diff, idx)? {
            for h in 0..patch.num_hunks() {
                let (hunk, _) = patch.hunk(h)?;
                let header = String::from_utf8_lossy(hunk.header()).trim_end().to_owned();
                let count = patch.num_lines_in_hunk(h)?;
                let mut lines = Vec::with_capacity(count);
                for l in 0..count {
                    let line = patch.line_in_hunk(h, l)?;
                    lines.push(DiffLine {
                        origin: line_origin(line.origin()),
                        text: String::from_utf8_lossy(line.content())
                            .trim_end_matches('\n')
                            .to_owned(),
                    });
                }
                hunks.push(Hunk {
                    header,
                    new_start: hunk.new_start(),
                    lines,
                });
            }
        }

        files.push(FileDiff {
            path,
            old_path,
            status: delta_status(delta.status()),
            hunks,
            binary,
        });
    }

    Ok(files)
}

fn delta_status(d: Delta) -> StatusCode {
    match d {
        Delta::Added | Delta::Untracked => StatusCode::Added,
        Delta::Deleted => StatusCode::Deleted,
        Delta::Renamed => StatusCode::Renamed,
        Delta::Copied => StatusCode::Copied,
        Delta::Typechange => StatusCode::TypeChanged,
        Delta::Conflicted => StatusCode::Unmerged,
        _ => StatusCode::Modified,
    }
}

fn line_origin(origin: char) -> LineOrigin {
    match origin {
        '+' => LineOrigin::Added,
        '-' => LineOrigin::Removed,
        ' ' => LineOrigin::Context,
        _ => LineOrigin::Meta,
    }
}
