use crate::pathspec::Pathspec;
use crate::rev::RevParse;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use git2::build::CheckoutBuilder;
use git2::{
    ApplyLocation, ApplyOptions, BranchType, Cred, CredentialType, Delta, Diff, DiffFindOptions,
    DiffOptions, ErrorCode, FetchOptions, ObjectType, Oid, Patch, PushOptions, RemoteCallbacks,
    Repository, ResetType, Status, StatusOptions,
};

use crate::config::{ConfigScope, open_config};
use crate::model::{CommitOptions, RepoState, ResetMode};

use crate::backend::GitBackend;
use crate::diff::{DiffLine, FileDiff, Hunk, LineOrigin};
use crate::error::GitError;
use crate::model::{Commit, Head, OpProgress, RepoStatus, Stash, StatusCode, StatusEntry};
use crate::shallow;

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
    /// A second handle on the same repository. Network transfers run on it so
    /// the main lock is never held across a push or fetch; libgit2 handles are
    /// not Sync, and a locked transfer stalls every refresh and edit behind it.
    fn fresh_handle(&self) -> Result<Repository, GitError> {
        let gitdir = self.repo.lock().expect("repo mutex").path().to_path_buf();
        Ok(Repository::open(gitdir)?)
    }

    /// Discover the repository containing `start` and validate it has a worktree.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self, GitError> {
        // Install the in-process, ssh-config-aware ssh transport (idempotent).
        #[cfg(feature = "ssh")]
        crate::ssh::register();
        crate::config::env_search_paths();
        let start = start.as_ref();
        let repo = Repository::discover(start).map_err(|e| match e.code() {
            ErrorCode::NotFound => GitError::NotARepository(start.to_path_buf()),
            _ => GitError::Git(e),
        })?;
        Self::from_repo(repo)
    }

    /// Open the repository as git finds it from `start`: GIT_DIR, GIT_WORK_TREE,
    /// GIT_INDEX_FILE, GIT_CEILING_DIRECTORIES and the rest of git's
    /// environment, with `-c` settings layered over its config.
    pub fn open_env(start: impl AsRef<Path>) -> Result<Self, GitError> {
        #[cfg(feature = "ssh")]
        crate::ssh::register();
        Self::from_repo(open_env(start.as_ref())?)
    }

    fn from_repo(repo: Repository) -> Result<Self, GitError> {
        crate::config::add_command_line(&repo.config()?)?;
        crate::promisor::install(&repo);
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

    /// Run a `git` CLI command in the working directory, returning its stdout on
    /// success or its stderr as a `Cli` error. Used only for the few operations
    /// libgit2 cannot do (bisect, shallow fetches, signing).
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

    /// Fetch one remote natively.
    fn fetch_remote(
        &self,
        repo: &Repository,
        name: &str,
        refspecs: &[String],
        args: &crate::FetchArgs,
        report: &dyn Fn(OpProgress),
        cred: Option<&dyn crate::CredentialPrompt>,
    ) -> Result<(), GitError> {
        if args.dry_run {
            return fetch_dry_run(repo, name, refspecs, args, report, cred);
        }
        fetch_one(repo, name, refspecs, args, report, cred)
    }

    /// Fetch several remotes as git's `fetch --multiple` does: up to
    /// `--jobs` (or `fetch.parallel`) at once, each adding to FETCH_HEAD,
    /// reported in order under a `Fetching <name>` line.
    fn fetch_many(
        &self,
        repo: &Repository,
        names: &[String],
        args: &crate::FetchArgs,
        report: &dyn Fn(OpProgress),
        cred: Option<&dyn crate::CredentialPrompt>,
    ) -> Result<(), GitError> {
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        if !args.append && !args.dry_run {
            std::fs::write(repo.path().join("FETCH_HEAD"), "")?;
        }
        let each = crate::FetchArgs {
            append: true,
            ..args.clone()
        };
        let jobs = match args.jobs {
            0 => repo.config()?.get_i32("fetch.parallel").unwrap_or(1).max(0) as usize,
            n => n,
        };
        let jobs = match jobs {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        };
        let path = repo.path().to_path_buf();
        type Outcome = (Vec<String>, Result<(), GitError>);
        let done: Vec<Mutex<Option<Outcome>>> = names.iter().map(|_| Mutex::new(None)).collect();
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..jobs.min(names.len()) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Relaxed);
                        let Some(name) = names.get(i) else { break };
                        let lines = Mutex::new(Vec::new());
                        let buffer = |p: OpProgress| {
                            if let OpProgress::Line(l) = p {
                                lines.lock().expect("lines mutex").push(l);
                            }
                        };
                        let result = Repository::open(&path)
                            .map_err(GitError::from)
                            .and_then(|r| self.fetch_remote(&r, name, &[], &each, &buffer, cred));
                        let lines = lines.into_inner().expect("lines mutex");
                        *done[i].lock().expect("outcome mutex") = Some((lines, result));
                    }
                });
            }
        });
        let mut failed = Vec::new();
        for (name, slot) in names.iter().zip(done) {
            report(OpProgress::Line(format!("Fetching {name}")));
            let Some((lines, result)) = slot.into_inner().expect("outcome mutex") else {
                continue;
            };
            for line in lines {
                report(OpProgress::Line(line));
            }
            if let Err(e) = result {
                failed.push(format!("{e}\nerror: could not fetch {name}"));
            }
        }
        match failed.is_empty() {
            true => Ok(()),
            false => Err(GitError::Other(failed.join("\n"))),
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
pub(crate) fn sync_index(repo: &Repository) -> Result<(), GitError> {
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
        let mut unstaged =
            extract_with_renames(&repo, repo.diff_index_to_workdir(None, Some(&mut wt_opts))?)?;
        drop_skipped(&repo, &mut unstaged)?;
        let mut idx_opts = DiffOptions::new();
        let staged = extract_with_renames(
            &repo,
            repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut idx_opts))?,
        )?;

        Ok(RepoStatus {
            head,
            entries,
            unstaged,
            staged,
            stashes,
            recent,
            state: repo_state(&repo),
            rebase: rebase_progress(&repo),
        })
    }

    fn stash_push(&self, include_untracked: bool) -> Result<String, GitError> {
        self.logged("stash", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            let sig = stash_signature(&repo, true)?;
            repo.stash_save2(&sig, None, stash_flags(include_untracked))?;
            restamp_stash(&repo, &sig)?;
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
            let sig = stash_signature(&repo, true)?;
            repo.stash_save2(&sig, Some(message), stash_flags(include_untracked))?;
            restamp_stash(&repo, &sig)?;
            Ok(stash_saved_line(&repo))
        })
    }

    fn stash_push_opts(
        &self,
        message: Option<&str>,
        include_untracked: bool,
        all: bool,
        keep_index: bool,
        paths: &[String],
    ) -> Result<String, GitError> {
        self.logged("stash", || {
            if paths.is_empty() {
                let mut repo = self.repo.lock().expect("repo mutex");
                let sig = stash_signature(&repo, true)?;
                let mut flags = stash_flags(include_untracked || all).unwrap_or_default();
                flags.set(git2::StashFlags::INCLUDE_IGNORED, all);
                flags.set(git2::StashFlags::KEEP_INDEX, keep_index);
                repo.stash_save2(&sig, message, Some(flags))?;
                restamp_stash(&repo, &sig)?;
                return Ok(stash_saved_line(&repo));
            }
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let opts = crate::stash::Opts {
                message,
                untracked: include_untracked,
                all,
                keep_index,
            };
            let out = crate::stash::push_paths(&repo, paths, &opts)?;
            Ok(out)
        })
    }

    fn stash_pop(&self, index: usize) -> Result<(), GitError> {
        self.logged("stash pop", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            apply_stash(&mut repo, index, None, true)
        })
    }

    fn stash_apply(&self, index: usize) -> Result<(), GitError> {
        self.logged("stash apply", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            apply_stash(&mut repo, index, None, false)
        })
    }

    fn stash_apply_opts(
        &self,
        index: usize,
        restore_index: bool,
        drop: bool,
    ) -> Result<(), GitError> {
        self.logged(if drop { "stash pop" } else { "stash apply" }, || {
            let mut repo = self.repo.lock().expect("repo mutex");
            let mut opts = git2::StashApplyOptions::new();
            if restore_index {
                opts.reinstantiate_index();
            }
            apply_stash(&mut repo, index, Some(&mut opts), drop)
        })
    }

    fn stash_drop(&self, index: usize) -> Result<(), GitError> {
        self.logged("stash drop", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            repo.stash_drop(index)?;
            Ok(())
        })
    }

    fn stash_push_part(
        &self,
        message: Option<&str>,
        picked: Option<&[u8]>,
        keep_index: bool,
        paths: &[String],
    ) -> Result<String, GitError> {
        self.logged("stash", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let head = repo.head()?.peel_to_commit()?;
            let head_tree = head.tree()?;
            let index_tree = repo.find_tree(repo.index()?.write_tree()?)?;
            let tree = match picked {
                None => index_tree,
                Some(patch) => {
                    let diff = Diff::from_buffer(patch)?;
                    let mut picked = repo.apply_to_tree(&head_tree, &diff, None)?;
                    repo.find_tree(picked.write_tree_to(&repo)?)?
                }
            };
            if tree.id() == head_tree.id() {
                return Err(GitError::Other(
                    match picked {
                        None => "no staged changes to stash",
                        Some(_) => "no changes selected",
                    }
                    .to_owned(),
                ));
            }
            let stash = stash_commit(&repo, message, &tree)?;
            let line = stash
                .summary()
                .ok()
                .flatten()
                .unwrap_or_default()
                .to_owned();
            store_stash(&repo, stash.id(), &line)?;
            let undo = repo.diff_tree_to_tree(Some(&tree), Some(&head_tree), None)?;
            if picked.is_none() {
                repo.apply(&undo, ApplyLocation::Both, None)?;
            } else {
                repo.apply(&undo, ApplyLocation::WorkDir, None)?;
                if !keep_index {
                    let all = [".".to_owned()];
                    let paths = if paths.is_empty() { &all[..] } else { paths };
                    crate::pathspec::reset_default(&repo, Some(head.as_object()), paths)?;
                }
            }
            Ok(format!("Saved working directory and index state {line}"))
        })
    }

    fn stash_create(&self, message: Option<&str>) -> Result<Option<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let head_tree = repo.head()?.peel_to_tree()?;
        let index_tree = repo.find_tree(repo.index()?.write_tree()?)?;
        let diff = repo.diff_index_to_workdir(None, None)?;
        let mut worktree = repo.apply_to_tree(&index_tree, &diff, None)?;
        let tree = repo.find_tree(worktree.write_tree_to(&repo)?)?;
        if tree.id() == head_tree.id() && index_tree.id() == head_tree.id() {
            return Ok(None);
        }
        Ok(Some(stash_commit(&repo, message, &tree)?.id().to_string()))
    }

    fn stash_store(&self, rev: &str, message: Option<&str>) -> Result<(), GitError> {
        self.logged("stash store", || {
            let repo = self.repo.lock().expect("repo mutex");
            let commit = repo.rev_single(rev)?.peel_to_commit()?;
            if commit.parent_count() < 2 {
                return Err(GitError::Other(format!("{rev} is not a stash-like commit")));
            }
            store_stash(
                &repo,
                commit.id(),
                message.unwrap_or("Created via \"git stash store\"."),
            )
        })
    }

    fn log(&self, opts: &crate::LogOptions) -> Result<Vec<crate::LogEntry>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let walked = crate::walk::walk(&repo, opts)?;
        let unpushed = unpushed_oids(&repo);
        let decorations = log_decorations(&repo);
        let mut entries = Vec::with_capacity(walked.len());
        for w in walked {
            let commit = repo.find_commit(w.id)?;
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
                author: commit.author().name().unwrap_or("?").to_owned(),
                when: relative_age(commit.time().seconds(), now),
                oid: w.id.to_string(),
                parents: w.parents.iter().map(Oid::to_string).collect(),
                refs: decorations.get(&w.id).cloned().unwrap_or_default(),
                unpushed: unpushed.contains(&w.id),
                mark: w.mark,
                source: w.source,
            });
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

        let commit = repo.rev_single(rev)?.peel_to_commit()?;
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
            files: extract_with_renames(&repo, diff)?,
        })
    }

    fn commit_overview(&self, rev: &str) -> Result<crate::CommitOverview, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let commit = repo.rev_single(rev)?.peel_to_commit()?;
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
        let commit = repo.rev_single(rev)?.peel_to_commit()?;
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut opts = DiffOptions::new();
        opts.pathspec(path);
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
        Ok(extract_with_renames(&repo, diff)?
            .into_iter()
            .find(|f| f.path == path))
    }

    fn diff_refs(&self, from: &str, to: &str) -> Result<Vec<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let from_tree = repo.rev_single(from)?.peel_to_tree()?;
        let to_tree = repo.rev_single(to)?.peel_to_tree()?;
        let mut opts = DiffOptions::new();
        let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), Some(&mut opts))?;
        extract_with_renames(&repo, diff)
    }

    fn diff(&self, spec: &crate::DiffSpec) -> Result<Vec<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut opts = DiffOptions::new();
        crate::pathspec::limit_diff(&mut opts, &spec.paths)?;
        if let Some(n) = spec.context {
            opts.context_lines(n);
        }
        opts.ignore_whitespace(spec.ignore_all_space)
            .ignore_whitespace_change(spec.ignore_space_change)
            .indent_heuristic(true)
            .reverse(spec.reverse);
        let t = crate::xdiff::tweaks();
        tweak_options(&mut opts, &t);
        let tree = |rev: &str| repo.rev_single(rev).and_then(|o| o.peel_to_tree());
        let mut diff = match (&spec.from, &spec.to) {
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
        let mut fopts = DiffFindOptions::new();
        fopts.renames(true);
        if let Some(n) = t.rename_limit {
            fopts.rename_limit(n);
        }
        diff.find_similar(Some(&mut fopts))?;
        let mut files = extract(&repo, &diff)?;
        crate::xdiff::finish(&repo, &diff, &mut files, spec, &t);
        crate::userdiff::refine(
            &repo,
            &diff,
            &mut files,
            spec.context.unwrap_or(3) as usize,
            spec.function_context,
        )?;
        if !spec.cached
            && spec.to.is_none()
            && !spec.from.as_ref().is_some_and(|f| f.contains(".."))
        {
            drop_skipped(&repo, &mut files)?;
        }
        Ok(crate::xdiff::relabel(files, &t))
    }

    fn word_regex(&self, old: Option<&str>, new: &str) -> Result<Option<Vec<u8>>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        crate::userdiff::word_regex(&repo, old, new)
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
        Ok(extract_with_renames(&repo, diff)?
            .into_iter()
            .find(|f| f.path == path))
    }

    fn blame(&self, path: &str) -> Result<Vec<crate::BlameLine>, GitError> {
        self.blame_with(&crate::BlameOptions {
            path: path.to_owned(),
            ..Default::default()
        })
        .map(|b| b.lines)
    }

    fn blame_at(&self, rev: &str, path: &str) -> Result<Vec<crate::BlameLine>, GitError> {
        self.blame_with(&crate::BlameOptions {
            path: path.to_owned(),
            revs: vec![rev.to_owned()],
            ..Default::default()
        })
        .map(|b| b.lines)
    }

    fn blame_with(&self, opts: &crate::BlameOptions) -> Result<crate::Blame, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        crate::blame::blame(&repo, &self.workdir, opts)
    }

    fn index_second(&self) -> Option<i64> {
        let repo = self.repo.lock().expect("repo mutex");
        index_second(&repo)
    }

    fn smudge_racy(&self, since: Option<i64>) -> Result<(), GitError> {
        let Some(since) = since else {
            return Ok(());
        };
        let repo = self.repo.lock().expect("repo mutex");
        // In the same second git still treats those entries as racy itself.
        if index_second(&repo).is_none_or(|now| now <= since) {
            return Ok(());
        }
        let mut index = repo.index()?;
        index.read(true)?;
        let suspects: Vec<git2::IndexEntry> = index
            .iter()
            .filter(|e| i64::from(e.mtime.seconds()) >= since && e.file_size != 0)
            .filter(|e| e.mode & 0o170000 == 0o100000)
            .collect();
        let mut changed = false;
        for mut entry in suspects {
            let path = self
                .workdir
                .join(String::from_utf8_lossy(&entry.path).as_ref());
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            // A different size is caught by git anyway.
            if meta.len() != u64::from(entry.file_size) {
                continue;
            }
            if Oid::hash_file(ObjectType::Blob, &path)? != entry.id {
                entry.file_size = 0;
                index.add(&entry)?;
                changed = true;
            }
        }
        if changed {
            index.write()?;
        }
        Ok(())
    }

    fn patch_diff(
        &self,
        rev: Option<&str>,
        cached: bool,
        reverse: bool,
        context: Option<u32>,
        paths: &[String],
    ) -> Result<Vec<u8>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let index = repo.index()?;
        crate::wt_status::patch_diff(&repo, &index, rev, cached, reverse, context, paths)
    }

    fn status_text(&self, opts: &crate::StatusOpts) -> Result<crate::StatusReport, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut index = repo.index()?;
        if !opts.commit_all && opts.commit_paths.is_empty() {
            return crate::wt_status::status(&repo, &index, opts);
        }
        // What `commit -a`, `-i` or `--only` would record, built in memory
        // and dropped again: a dry run leaves the index as it was.
        let paths = root_dots(&opts.commit_paths);
        let result = (|| {
            if opts.commit_all {
                index.update_all(["*"], Some(&mut crate::sparse::keep_sparse(&repo, &index)))?;
            }
            if opts.commit_include {
                let skip = crate::sparse::keep_sparse(&repo, &index);
                crate::pathspec::index_walk(&paths, skip, |s, cb| index.update_all(s, cb))?;
            }
            if opts.commit_paths.is_empty() || opts.commit_include {
                return crate::wt_status::status(&repo, &index, opts);
            }
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            for p in &paths {
                known(Some(&index), head_tree.as_ref(), p)?;
            }
            let skip = crate::sparse::keep_sparse(&repo, &index);
            crate::pathspec::index_walk(&paths, skip, |s, cb| index.update_all(s, cb))?;
            let spec = Pathspec::new(&paths)?;
            let hit = |e: &git2::IndexEntry| {
                spec.matches_path(Path::new(String::from_utf8_lossy(&e.path).as_ref()))
            };
            let mut partial = git2::Index::new()?;
            if let Some(tree) = &head_tree {
                partial.read_tree(tree)?;
            }
            let gone: Vec<PathBuf> = partial
                .iter()
                .filter(|e| hit(e))
                .map(|e| PathBuf::from(String::from_utf8_lossy(&e.path).as_ref()))
                .collect();
            for p in gone {
                partial.remove_path(&p)?;
            }
            for e in index.iter().filter(|e| hit(e)) {
                partial.add(&e)?;
            }
            crate::wt_status::status(&repo, &partial, opts)
        })();
        index.read(true)?;
        result
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
        index.add_all(
            ["*"],
            git2::IndexAddOption::DEFAULT,
            Some(&mut crate::sparse::keep_sparse(&repo, &index)),
        )?;
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
        let path = root_dot(path);
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        no_match(&repo, path)?;
        let mut index = repo.index()?;
        if self.workdir.join(path).is_file() {
            index.add_path(Path::new(path))?;
        } else {
            // A folder, glob or deleted path: add what is there, drop what is gone.
            let specs = [path.to_owned()];
            let skip = crate::sparse::keep_sparse(&repo, &index);
            crate::pathspec::index_walk(&specs, skip, |s, cb| {
                index.add_all(s, git2::IndexAddOption::DEFAULT, cb)
            })?;
            let skip = crate::sparse::keep_sparse(&repo, &index);
            crate::pathspec::index_walk(&specs, skip, |s, cb| index.update_all(s, cb))?;
        }
        index.write()?;
        Ok(())
    }

    fn unstage_file(&self, path: &str) -> Result<(), GitError> {
        let path = root_dot(path);
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        no_match(&repo, path)?;
        match repo.head() {
            Ok(head_ref) => {
                let target = head_ref.peel(ObjectType::Commit)?;
                crate::pathspec::reset_default(&repo, Some(&target), &[path.to_owned()])?;
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

    fn add(&self, paths: &[String], update: bool, force: bool) -> Result<(), GitError> {
        let paths = &root_dots(paths);
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        if !force {
            for p in paths {
                no_match(&repo, p)?;
            }
        }
        // libgit2 hands a callback no pathspec match for an empty pathspec.
        let all = ["*".to_owned()];
        let paths = if paths.is_empty() { &all[..] } else { paths };
        let mut index = repo.index()?;
        if !update {
            let flags = if force {
                git2::IndexAddOption::FORCE
            } else {
                git2::IndexAddOption::CHECK_PATHSPEC
            };
            let skip = crate::sparse::keep_sparse(&repo, &index);
            crate::pathspec::index_walk(paths, skip, |s, cb| index.add_all(s, flags, cb))?;
        }
        let skip = crate::sparse::keep_sparse(&repo, &index);
        crate::pathspec::index_walk(paths, skip, |s, cb| index.update_all(s, cb))?;
        index.write()?;
        Ok(())
    }

    fn checkout_merge(
        &self,
        paths: &[String],
        style: Option<&str>,
    ) -> Result<(usize, usize, Vec<String>), GitError> {
        self.logged("checkout -m", || {
            let paths = &root_dots(paths);
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let mut index = repo.index()?;
            let spec = Pathspec::new(paths)?;
            let hit = |p: &str| spec.matches_path(Path::new(p));
            unmerge_index(&mut index, &hit)?;
            let style = style
                .map(str::to_owned)
                .or_else(|| repo.config().ok()?.get_string("merge.conflictStyle").ok())
                .unwrap_or_else(|| "merge".to_owned());
            let mut conflicted = Vec::new();
            let mut clean = Vec::new();
            for e in index.iter() {
                let path = String::from_utf8_lossy(&e.path).into_owned();
                if !hit(&path) {
                    continue;
                }
                if (e.flags >> 12) & 3 != 0 {
                    if conflicted.last() != Some(&path) {
                        conflicted.push(path);
                    }
                } else {
                    clean.push(path);
                }
            }
            let mut errors = Vec::new();
            let mut recreated = 0;
            for path in &conflicted {
                let c = index.conflict_get(Path::new(path))?;
                let (Some(ours), Some(theirs)) = (c.our, c.their) else {
                    errors.push(format!("path '{path}' does not have necessary versions"));
                    continue;
                };
                let blob = |e: &git2::IndexEntry| -> Result<Vec<u8>, GitError> {
                    Ok(repo.find_blob(e.id)?.content().to_vec())
                };
                let base = c
                    .ancestor
                    .as_ref()
                    .map(blob)
                    .transpose()?
                    .unwrap_or_default();
                let (our_text, their_text) = (blob(&ours)?, blob(&theirs)?);
                fn merge_input<'a>(
                    text: &'a [u8],
                    path: &str,
                    mode: u32,
                ) -> git2::MergeFileInput<'a> {
                    let mut i = git2::MergeFileInput::new();
                    i.content(text).path(path).mode(Some(file_mode(mode)));
                    i
                }
                let input = |text, mode| merge_input(text, path, mode);
                let mut o = git2::MergeFileOptions::new();
                o.ancestor_label("base")
                    .our_label("ours")
                    .their_label("theirs")
                    .style_diff3(style == "diff3")
                    .style_zdiff3(style == "zdiff3");
                let base_mode = c.ancestor.as_ref().map_or(ours.mode, |a| a.mode);
                let merged = git2::merge_file(
                    &input(&base, base_mode),
                    &input(&our_text, ours.mode),
                    &input(&their_text, theirs.mode),
                    Some(&mut o),
                )?;
                let target = self.workdir.join(path);
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let _ = std::fs::remove_file(&target);
                std::fs::write(&target, merged.content())?;
                if ours.mode == 0o100755 {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
                }
                recreated += 1;
            }
            if !clean.is_empty() {
                let mut co = CheckoutBuilder::new();
                co.force().disable_pathspec_match(true);
                for p in &clean {
                    co.path(p);
                }
                repo.checkout_index(Some(&mut index), Some(&mut co))?;
            }
            index.write()?;
            Ok((recreated, clean.len(), errors))
        })
    }

    fn renormalize(&self, paths: &[String]) -> Result<(), GitError> {
        let paths = &root_dots(paths);
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut index = repo.index()?;
        let spec = Pathspec::new(paths)?;
        let tracked: Vec<PathBuf> = index
            .iter()
            .filter(|e| (e.flags >> 12) & 3 == 0 && e.mode != 0o160000)
            .map(|e| PathBuf::from(String::from_utf8_lossy(&e.path).as_ref()))
            .filter(|p| paths.is_empty() || spec.matches_path(p))
            .collect();
        for p in tracked {
            if self.workdir.join(&p).symlink_metadata().is_ok() {
                // add_path hashes through the filters whatever the stat says;
                // with the old entry gone, text=auto no longer defers to a
                // CRLF blob already in the index.
                index.remove_path(&p)?;
                index.add_path(&p)?;
            }
        }
        index.write()?;
        Ok(())
    }

    fn index_chmod(&self, paths: &[String], executable: bool) -> Result<Vec<String>, GitError> {
        let paths = &root_dots(paths);
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut index = repo.index()?;
        let spec = Pathspec::new(paths)?;
        let mut skipped = Vec::new();
        let hits: Vec<git2::IndexEntry> = index
            .iter()
            .filter(|e| spec.matches_path(Path::new(String::from_utf8_lossy(&e.path).as_ref())))
            .collect();
        for mut e in hits {
            if e.mode & 0o170000 != 0o100000 {
                skipped.push(String::from_utf8_lossy(&e.path).into_owned());
                continue;
            }
            e.mode = if executable { 0o100755 } else { 0o100644 };
            index.add(&e)?;
        }
        index.write()?;
        Ok(skipped)
    }

    fn restore(
        &self,
        paths: &[String],
        source: Option<&str>,
        staged: bool,
        worktree: bool,
        overlay: bool,
    ) -> Result<(), GitError> {
        self.logged("restore", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let paths = &root_dots(paths);
            let target = source
                .or(staged.then_some("HEAD"))
                .map(|s| repo.rev_single(s))
                .transpose()?;
            let tree = target.as_ref().map(|t| t.peel_to_tree()).transpose()?;
            // checkout matches a revision's paths in it alone, as git does.
            let index = repo.index()?;
            let index = (!overlay || tree.is_none()).then_some(&index);
            for p in paths {
                known(index, tree.as_ref(), p)?;
            }
            // Overlay: only the source's own files, so the rest stay put.
            // Magic pathspecs reach libgit2 as the literal paths they match.
            let spec = Pathspec::new(paths)?;
            let literal = (overlay && tree.is_some()) || !spec.is_plain();
            let paths: Vec<String> = match &tree {
                Some(tree) if overlay => spec.match_tree(tree),
                _ if !spec.is_plain() => {
                    let mut all = spec.match_index(&repo.index()?);
                    all.extend(
                        tree.as_ref()
                            .map(|t| spec.match_tree(t))
                            .unwrap_or_default(),
                    );
                    all.sort();
                    all.dedup();
                    all
                }
                _ => paths.to_vec(),
            };
            if paths.is_empty() {
                return Ok(());
            }
            // Without overlay, tracked files the source lacks go away, as in git.
            let gone: Vec<String> = match &tree {
                Some(tree) if worktree && !overlay => {
                    let spec = Pathspec::new(&paths)?;
                    repo.index()?
                        .iter()
                        .map(|e| String::from_utf8_lossy(&e.path).into_owned())
                        .filter(|p| spec.matches_path(Path::new(p)))
                        .filter(|p| tree.get_path(Path::new(p)).is_err())
                        .collect()
                }
                _ => Vec::new(),
            };
            for p in &gone {
                let _ = std::fs::remove_file(self.workdir.join(p));
            }
            if staged {
                repo.reset_default(target.as_ref(), &paths)?;
            }
            if !worktree {
                return Ok(());
            }
            let mut checkout = CheckoutBuilder::new();
            checkout.force().disable_pathspec_match(literal);
            for p in &paths {
                checkout.path(p);
            }
            match &tree {
                Some(tree) => {
                    checkout.update_index(false);
                    repo.checkout_tree(tree.as_object(), Some(&mut checkout))?;
                }
                None => repo.checkout_index(None, Some(&mut checkout))?,
            }
            Ok(())
        })
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
        let commit = repo.rev_single(rev)?.peel_to_commit()?;
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
            repo.rev_single(&format!("{rev}:{path}"))?.peel_to_blob()?
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
        let start = repo.rev_single(rev)?.peel_to_commit()?.id();
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
        let tree = repo.rev_single(rev)?.peel_to_commit()?.tree()?;
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
        Ok(repo.rev_single(rev)?.peel_to_commit()?.id().to_string())
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
            let Ok(obj) = repo.rev_single(name) else {
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
            let Ok(obj) = repo.rev_single(name) else {
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
        self.archive(&crate::ArchiveOpts {
            rev: rev.to_owned(),
            format: "tgz".to_owned(),
            ..Default::default()
        })
    }

    fn checkout_detached(&self, rev: &str) -> Result<(), GitError> {
        self.logged("checkout", || {
            let repo = self.repo.lock().expect("repo mutex");
            let commit = repo.rev_single(rev)?.peel_to_commit()?;
            safe_checkout(&repo, commit.as_object(), "checkout")?;
            repo.set_head_detached(commit.id())?;
            Ok(())
        })
    }

    fn checkout_with(
        &self,
        rev: &str,
        branch: bool,
        mode: crate::CheckoutMode,
    ) -> Result<(), GitError> {
        self.logged("checkout", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let refname = format!("refs/heads/{rev}");
            let commit = repo
                .rev_single(if branch { &refname } else { rev })?
                .peel_to_commit()?;
            match mode {
                crate::CheckoutMode::Safe => safe_checkout(&repo, commit.as_object(), "checkout")?,
                crate::CheckoutMode::Force => {
                    repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().force()))?
                }
                crate::CheckoutMode::Merge { diff3 } => checkout_merge(&repo, &commit, rev, diff3)?,
            }
            if branch {
                repo.set_head(&refname)?;
            } else {
                repo.set_head_detached(commit.id())?;
            }
            Ok(())
        })
    }

    fn checkout_orphan(&self, name: &str, start: Option<&str>) -> Result<(), GitError> {
        self.logged("checkout", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let refname = format!("refs/heads/{name}");
            if !git2::Branch::name_is_valid(name)? {
                return Err(GitError::Other(format!(
                    "'{name}' is not a valid branch name"
                )));
            }
            if repo.find_reference(&refname).is_ok() {
                return Err(GitError::Other(format!(
                    "a branch named '{name}' already exists"
                )));
            }
            let tree = match start {
                Some(rev) => repo.rev_single(rev)?.peel_to_tree()?,
                None => repo.find_tree(repo.treebuilder(None)?.write()?)?,
            };
            repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe()))?;
            repo.set_head(&refname)?;
            Ok(())
        })
    }

    fn commits_between(&self, base: &str) -> Result<Vec<(String, String)>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let base_oid = repo.rev_single(base)?.peel_to_commit()?.id();
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
            let mut repo = self.repo.lock().expect("repo mutex");
            crate::rebase::replay(&mut repo, rev, None, report)
        })
    }

    fn rebase_range(
        &self,
        upstream: &str,
        onto: &str,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        self.logged("restack", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            crate::rebase::replay(&mut repo, upstream, Some(onto), report)
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

    fn rebase_abort(&self) -> Result<String, GitError> {
        self.logged("rebase abort", || {
            crate::rebase::abort(&self.repo.lock().expect("repo mutex"))
        })
    }

    fn rebase_continue(&self) -> Result<String, GitError> {
        self.logged("rebase", || {
            crate::rebase::resume(&self.repo.lock().expect("repo mutex"), false)
        })
    }

    fn rebase_skip(&self) -> Result<String, GitError> {
        self.logged("rebase", || {
            crate::rebase::resume(&self.repo.lock().expect("repo mutex"), true)
        })
    }

    fn rebase_quit(&self) -> Result<(), GitError> {
        crate::rebase::quit(&self.repo.lock().expect("repo mutex"))
    }

    fn rebase_edit_todo(&self) -> Result<(), GitError> {
        crate::rebase::edit_todo(&self.repo.lock().expect("repo mutex"))
    }

    fn rebase_with(
        &self,
        upstream: Option<&str>,
        opts: &crate::RebaseOptions,
    ) -> Result<String, GitError> {
        self.logged("rebase", || {
            let mut repo = self.repo.lock().expect("repo mutex");
            crate::rebase::start(&mut repo, upstream, opts)
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
                let sig = ident_signature(&repo, true)
                    .or_else(|_| git2::Signature::now("rgit", "rgit@localhost"))?;
                let author = ident_signature(&repo, false).unwrap_or_else(|_| sig.clone());
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
                    crate::sign::commit_configured(
                        &repo,
                        Some("HEAD"),
                        &author,
                        &sig,
                        &format!("fixup! {subject}"),
                        &tree,
                        &[&head],
                    )?;
                }
            }
            // If autosquash conflicts, abort so the repo is not left mid-rebase with
            // the synthetic fixup commits; the op-log snapshot then fully restores.
            let opts = crate::RebaseOptions {
                autosquash: true,
                autostash: true,
                fork_point: Some(false),
                quiet: true,
                ..Default::default()
            };
            let mut repo = self.repo.lock().expect("repo mutex");
            if let Err(e) = crate::rebase::start(&mut repo, Some(&base.to_string()), &opts) {
                if repo.path().join("rebase-merge").exists() {
                    let _ = crate::rebase::abort(&repo);
                }
                return Err(e);
            }
            Ok(format!(
                "absorbed {hunk_count} hunk(s) into {} commit(s)",
                order.len()
            ))
        })
    }

    fn bisect(&self, args: &[String]) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::bisect::run(&repo, args)
    }

    fn rerere(&self, args: &[String], autoupdate: Option<bool>) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::rerere::command(&repo, args, autoupdate)
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
        let git_dir = self.git_dir();
        let mode = if add {
            crate::SetMode::Add
        } else {
            crate::SetMode::Replace
        };
        crate::config_set(Some(&git_dir), &scope, name, value, None, mode)
    }

    fn config_unset(&self, scope: ConfigScope, name: &str, all: bool) -> Result<(), GitError> {
        let git_dir = self.git_dir();
        crate::config_unset(Some(&git_dir), &scope, name, None, all)
    }

    fn apply_patch(
        &self,
        files: &[crate::FilePatch],
        opts: &crate::ApplyOpts,
    ) -> Result<String, GitError> {
        let run = || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            crate::apply::apply(&repo, files, opts)
        };
        if opts.check {
            run()
        } else {
            self.logged("apply", run)
        }
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
        let oid = repo.rev_single(rev)?.id();
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
        cmd: &str,
    ) -> Result<(), GitError> {
        self.logged("notes", || {
            let repo = self.repo.lock().expect("repo mutex");
            let oid = repo.rev_single(rev)?.id();
            let blob = repo.blob(message.as_bytes())?;
            crate::notes::commit_notes(
                &repo,
                notes_ref.unwrap_or("refs/notes/commits"),
                &format!("Notes added by 'git notes {cmd}'"),
                |notes| {
                    notes.insert(oid, blob);
                    Ok(true)
                },
            )
        })
    }

    fn note_remove(
        &self,
        notes_ref: Option<&str>,
        revs: &[String],
        all_or_nothing: bool,
        cmd: &str,
    ) -> Result<Vec<bool>, GitError> {
        self.logged("notes", || {
            let repo = self.repo.lock().expect("repo mutex");
            let oids = revs
                .iter()
                .map(|r| {
                    repo.rev_single(r).map(|o| o.id()).map_err(|_| {
                        GitError::Other(format!("Failed to resolve '{r}' as a valid ref."))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut removed = Vec::new();
            crate::notes::commit_notes(
                &repo,
                notes_ref.unwrap_or("refs/notes/commits"),
                &format!("Notes removed by 'git notes {cmd}'"),
                |notes| {
                    removed = oids.iter().map(|o| notes.remove(o).is_some()).collect();
                    Ok(removed.contains(&true) && !(all_or_nothing && removed.contains(&false)))
                },
            )?;
            Ok(removed)
        })
    }

    fn note_copy(
        &self,
        notes_ref: Option<&str>,
        from: &str,
        to: &str,
        force: bool,
    ) -> Result<(), GitError> {
        self.logged("notes", || {
            let repo = self.repo.lock().expect("repo mutex");
            let from = repo.rev_single(from)?.id();
            let to = repo.rev_single(to)?.id();
            crate::notes::commit_notes(
                &repo,
                notes_ref.unwrap_or("refs/notes/commits"),
                "Notes added by 'git notes copy'",
                |notes| {
                    if !force && notes.contains_key(&to) {
                        return Err(GitError::Other(format!(
                            "Cannot copy notes. Found existing notes for object {to}. Use '-f' to overwrite existing notes"
                        )));
                    }
                    let note = *notes.get(&from).ok_or_else(|| {
                        GitError::Other(format!(
                            "missing notes on source object {from}. Cannot copy."
                        ))
                    })?;
                    notes.insert(to, note);
                    Ok(true)
                },
            )
        })
    }

    fn notes_prune(&self, notes_ref: Option<&str>, dry_run: bool) -> Result<Vec<String>, GitError> {
        let run = || {
            let repo = self.repo.lock().expect("repo mutex");
            let odb = repo.odb()?;
            let mut gone = Vec::new();
            crate::notes::commit_notes(
                &repo,
                notes_ref.unwrap_or("refs/notes/commits"),
                "Notes removed by 'git notes prune'",
                |notes| {
                    gone = notes.keys().filter(|o| !odb.exists(**o)).copied().collect();
                    for o in &gone {
                        notes.remove(o);
                    }
                    Ok(!dry_run && !gone.is_empty())
                },
            )?;
            Ok(gone.iter().map(Oid::to_string).collect())
        };
        if dry_run {
            run()
        } else {
            self.logged("notes", run)
        }
    }

    fn notes_merge(
        &self,
        notes_ref: &str,
        other: &str,
        strategy: &str,
        verbosity: u8,
    ) -> Result<(String, Option<(String, i32)>), GitError> {
        self.logged("notes", || {
            let repo = self.repo.lock().expect("repo mutex");
            crate::notes::merge(&repo, notes_ref, other, strategy, verbosity)
        })
    }

    fn notes_merge_finish(&self, commit: bool, verbosity: u8) -> Result<String, GitError> {
        self.logged("notes", || {
            let repo = self.repo.lock().expect("repo mutex");
            crate::notes::merge_finish(&repo, commit, verbosity)
        })
    }

    fn update_refs(
        &self,
        updates: &[crate::RefUpdate],
        message: Option<&str>,
        no_deref: bool,
        create_reflog: bool,
        check_only: bool,
        batch: bool,
    ) -> Result<Vec<String>, GitError> {
        let run = || {
            crate::update_ref::update_refs(
                &self.repo.lock().expect("repo mutex"),
                updates,
                message,
                no_deref,
                create_reflog,
                check_only,
                batch,
            )
        };
        if check_only {
            run()
        } else {
            self.logged("update-ref", run)
        }
    }

    fn bundle_create(&self, path: &Path, args: &[String]) -> Result<usize, GitError> {
        crate::bundle::create(&self.repo.lock().expect("repo mutex"), path, args)
    }

    fn bundle_verify(
        &self,
        path: &Path,
    ) -> Result<(crate::BundleHeader, Vec<(String, String)>), GitError> {
        crate::bundle::verify(&self.repo.lock().expect("repo mutex"), path)
    }

    fn bundle_unbundle(&self, path: &Path) -> Result<Vec<(String, String)>, GitError> {
        crate::bundle::unbundle(&self.repo.lock().expect("repo mutex"), path)
    }

    fn request_pull(
        &self,
        start: &str,
        url: &str,
        end: Option<&str>,
        patch: bool,
    ) -> Result<(String, Vec<String>), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        crate::format_patch::request_pull(&repo, start, url, end, patch)
    }

    fn range_diff(&self, opts: &crate::RangeDiffOpts) -> Result<String, GitError> {
        crate::range_diff::range_diff(&self.repo.lock().expect("repo mutex"), opts)
    }

    fn combined_diff(
        &self,
        commit: &str,
        paths: &[String],
        dense: bool,
    ) -> Result<Vec<crate::CombinedFile>, GitError> {
        crate::combine::combined(&self.repo.lock().expect("repo mutex"), commit, paths, dense)
    }

    fn line_log(
        &self,
        tip: &str,
        order: &[String],
        specs: &[String],
        first_parent: bool,
    ) -> Result<Vec<Option<Vec<crate::FileDiff>>>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        crate::line_log::line_log(&repo, tip, order, specs, first_parent)
    }

    fn remerge_diff(
        &self,
        commit: &str,
        paths: &[String],
    ) -> Result<Vec<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let commit = repo.rev_single(commit)?.peel_to_commit()?;
        let Some((tree, notes)) = crate::combine::remerge_tree(&repo, &commit)? else {
            return Ok(Vec::new());
        };
        let mut opts = DiffOptions::new();
        crate::pathspec::limit_diff(&mut opts, paths)?;
        opts.indent_heuristic(true);
        let diff = repo.diff_tree_to_tree(
            Some(&repo.find_tree(tree)?),
            Some(&commit.tree()?),
            Some(&mut opts),
        )?;
        let mut files = extract_with_renames(&repo, diff)?;
        for f in &mut files {
            if let Some((_, note)) = notes.iter().find(|(p, _)| *p == f.path) {
                let at = f.header.find('\n').map_or(f.header.len(), |i| i + 1);
                f.header.insert_str(at, &format!("{note}\n"));
            }
        }
        Ok(files)
    }

    fn cherry(
        &self,
        upstream: &str,
        head: &str,
        limit: Option<&str>,
    ) -> Result<Vec<crate::CherryCommit>, GitError> {
        crate::format_patch::cherry(
            &self.repo.lock().expect("repo mutex"),
            upstream,
            head,
            limit,
        )
    }

    fn format_patch(
        &self,
        opts: &crate::FormatPatchOpts,
    ) -> Result<Vec<crate::PatchMail>, GitError> {
        crate::format_patch::format_patch(&self.repo.lock().expect("repo mutex"), opts)
    }

    fn am(&self, args: &[String], mbox: Option<&[u8]>) -> Result<String, GitError> {
        self.logged("am", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            crate::am::am(&repo, args, mbox)
        })
    }

    fn archive(&self, o: &crate::ArchiveOpts) -> Result<Vec<u8>, GitError> {
        crate::archive::archive(&self.repo.lock().expect("repo mutex"), o)
    }

    fn gc(&self, opts: &crate::GcOptions) -> Result<String, GitError> {
        crate::maintenance::gc(&self.repo.lock().expect("repo mutex"), opts)
    }

    fn repack(&self, opts: &crate::RepackOptions) -> Result<String, GitError> {
        crate::maintenance::repack(&self.repo.lock().expect("repo mutex"), opts)
    }

    fn commit_graph(&self, op: &crate::CommitGraphOp) -> Result<Vec<String>, GitError> {
        crate::commit_graph::run(&self.repo.lock().expect("repo mutex"), op)
    }

    fn multi_pack_index(&self, op: &crate::MidxOp) -> Result<Vec<String>, GitError> {
        crate::midx::run(&self.repo.lock().expect("repo mutex"), op)
    }

    fn pack_refs(&self, all: bool, no_prune: bool, auto: bool) -> Result<(), GitError> {
        crate::maintenance::pack_refs(&self.repo.lock().expect("repo mutex"), all, no_prune, auto)
    }

    fn reflog_expire(&self, opts: &crate::ReflogExpire) -> Result<Vec<String>, GitError> {
        crate::maintenance::reflog_expire(&self.repo.lock().expect("repo mutex"), opts)
    }

    fn reflog_delete(
        &self,
        entries: &[String],
        opts: &crate::ReflogExpire,
    ) -> Result<(), GitError> {
        crate::maintenance::reflog_delete(&self.repo.lock().expect("repo mutex"), entries, opts)
    }

    fn reflog_exists(&self, name: &str) -> bool {
        crate::maintenance::reflog_exists(&self.repo.lock().expect("repo mutex"), name)
    }

    fn maintenance_run(&self, opts: &crate::MaintenanceRun) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        let prefetch = || prefetch(&repo, cred_guard.as_deref());
        crate::maintenance::run(&repo, opts, &prefetch)
    }

    fn fsck(&self, opts: &crate::FsckOptions) -> Result<crate::FsckReport, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::fsck::fsck(&repo, opts)
    }

    fn clean_candidates(&self, opts: &crate::CleanOptions) -> Result<Vec<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::clean::candidates(&repo, opts)
    }

    fn clean_remove(
        &self,
        items: &[String],
        opts: &crate::CleanOptions,
    ) -> Result<Vec<String>, GitError> {
        if opts.dry_run {
            return Ok(crate::clean::remove(&self.workdir, items, opts));
        }
        self.logged("clean", || {
            Ok(crate::clean::remove(&self.workdir, items, opts))
        })
    }

    fn remove_paths(
        &self,
        paths: &[String],
        opts: crate::RmOptions,
    ) -> Result<Vec<String>, GitError> {
        let run = || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let mut index = repo.index()?;
            let mut hits = Vec::new();
            let spec = Pathspec::new(paths)?;
            if !spec.is_plain() {
                let names = spec.match_index(&index);
                let all: Vec<String> = index
                    .iter()
                    .map(|e| String::from_utf8_lossy(&e.path).into_owned())
                    .collect();
                for (orig, how) in spec.seen(&all) {
                    if how == 0 && !opts.ignore_unmatch {
                        return Err(did_not_match(&orig));
                    }
                    if how == 1 && !opts.recursive {
                        return Err(GitError::Other(format!(
                            "not removing '{orig}' recursively without -r"
                        )));
                    }
                }
                hits = names;
            }
            for path in paths.iter().filter(|_| spec.is_plain()) {
                let found = index_matches(&index, root_dot(path))?;
                if found.is_empty() && !opts.ignore_unmatch {
                    return Err(did_not_match(path));
                }
                if !opts.recursive
                    && found.iter().any(|h| h != path)
                    && !path.contains(['*', '?', '['])
                {
                    return Err(GitError::Other(format!(
                        "not removing '{path}' recursively without -r"
                    )));
                }
                hits.extend(found);
            }
            hits.sort();
            hits.dedup();
            if !opts.force {
                rm_check(&repo, &index, &hits, opts.cached)?;
            }
            if opts.dry_run {
                return Ok(hits);
            }
            for hit in &hits {
                index.remove_path(Path::new(hit))?;
                if !opts.cached {
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
            Ok(hits)
        };
        if opts.dry_run {
            run()
        } else {
            self.logged("rm", run)
        }
    }

    fn move_path(
        &self,
        from: &str,
        to: &str,
        force: bool,
        dry_run: bool,
    ) -> Result<String, GitError> {
        let run = || {
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
                let why = if self.workdir.join(from).symlink_metadata().is_ok() {
                    "not under version control"
                } else {
                    "bad source"
                };
                return Err(GitError::Other(format!(
                    "{why}, source={from}, destination={to}"
                )));
            }
            let dest = self.workdir.join(&to);
            if dest.exists() && (!force || dest.is_dir()) {
                return Err(GitError::Other(format!(
                    "destination exists, source={from}, destination={to}"
                )));
            }
            if dry_run {
                return Ok(to);
            }
            if dest.exists() {
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
            Ok(to)
        };
        if dry_run {
            run()
        } else {
            self.logged("mv", run)
        }
    }

    fn intent_to_add(&self, paths: &[String]) -> Result<Vec<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let paths = root_dots(paths);
        for p in &paths {
            no_match(&repo, p)?;
        }
        let spec = Pathspec::new(&paths)?;
        let mut opts = StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let new: Vec<String> = repo
            .statuses(Some(&mut opts))?
            .iter()
            .filter(|e| e.status().contains(Status::WT_NEW))
            .filter_map(|e| e.path().ok().map(str::to_owned))
            .filter(|p| spec.matches_path(Path::new(p)))
            .collect();
        let empty = repo.blob(b"")?;
        let mut index = repo.index()?;
        for p in &new {
            let meta = std::fs::symlink_metadata(self.workdir.join(p))?;
            index.add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: if meta.is_symlink() {
                    0o120000
                } else {
                    crate::lanes::executable_mode(&meta)
                },
                uid: 0,
                gid: 0,
                file_size: 0,
                id: empty,
                flags: 0,
                flags_extended: git2::IndexEntryExtendedFlag::INTENT_TO_ADD.bits(),
                path: p.as_bytes().to_vec(),
            })?;
        }
        index.write()?;
        Ok(new)
    }

    fn checkout_side(&self, paths: &[String], ours: bool) -> Result<(), GitError> {
        let wrote = self.logged("checkout", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let spec = Pathspec::new(root_dots(paths))?;
            let mut wrote = false;
            for c in repo.index()?.conflicts()? {
                let c = c?;
                let Some(any) = c.our.as_ref().or(c.their.as_ref()).or(c.ancestor.as_ref()) else {
                    continue;
                };
                let path = String::from_utf8_lossy(&any.path).into_owned();
                if !spec.matches_path(Path::new(&path)) {
                    continue;
                }
                let Some(side) = (if ours { c.our } else { c.their }) else {
                    return Err(GitError::Other(format!(
                        "path '{path}' does not have {} version",
                        if ours { "our" } else { "their" }
                    )));
                };
                std::fs::write(self.workdir.join(&path), repo.find_blob(side.id)?.content())?;
                wrote = true;
            }
            Ok(wrote)
        })?;
        if wrote {
            Ok(())
        } else {
            self.restore(paths, None, false, true, false)
        }
    }

    fn describe(&self, rev: &str, opts: &crate::DescribeOptions) -> Result<String, GitError> {
        crate::describe::describe(&self.repo.lock().expect("repo mutex"), rev, opts)
    }

    fn git(&self, args: &[String]) -> Result<String, GitError> {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_git(&argv, &[])
    }

    fn reset(&self, rev: &str, mode: ResetMode) -> Result<(), GitError> {
        self.logged("reset", || {
            let repo = self.repo.lock().expect("repo mutex");
            let target = repo.rev_single(rev)?;
            if mode == ResetMode::Merge {
                sync_index(&repo)?;
                reset_merge(&repo, &target.peel_to_commit()?)?;
            }
            if mode == ResetMode::Keep {
                // A safe checkout updates only what differs from HEAD and
                // refuses to overwrite local changes there.
                let commit = target.peel(ObjectType::Commit)?;
                repo.checkout_tree(&commit, Some(CheckoutBuilder::new().safe()))?;
            }
            let kind = match mode {
                ResetMode::Soft => ResetType::Soft,
                ResetMode::Mixed | ResetMode::Keep | ResetMode::Merge => ResetType::Mixed,
                ResetMode::Hard => ResetType::Hard,
            };
            repo.reset(&target, kind, None)?;
            Ok(())
        })
    }

    fn reset_paths(&self, rev: &str, paths: &[String]) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let target = repo.rev_single(rev)?.peel(ObjectType::Commit)?;
        crate::pathspec::reset_default(&repo, Some(&target), &root_dots(paths))?;
        Ok(())
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
                let message = match &state.opts.cleanup {
                    Some(mode) => cleanup_message(&message, Some(mode), false)?,
                    None => message,
                };
                let committer = repo.committer_from_env()?;
                let author = if state.opts.revert {
                    committer.clone()
                } else {
                    source.author().to_owned()
                };
                let key =
                    crate::sign::commit_key(&repo, state.opts.sign.as_deref(), state.opts.no_sign);
                crate::sign::commit(
                    &repo,
                    Some("HEAD"),
                    &author,
                    &committer,
                    &message,
                    &tree,
                    &[&head],
                    key.as_deref(),
                )?;
            }
            crate::rerere::say(&repo, None);
            end_operation(&repo)?;
            run_picks(&repo, &state.todo[1..], &state.opts, state.orig_head)
        })
    }

    fn pick_skip(&self) -> Result<(), GitError> {
        self.logged("cherry-pick", || {
            let repo = self.repo.lock().expect("repo mutex");
            let state = read_pick_state(&repo)?;
            let head = repo.head()?.peel_to_commit()?;
            repo.reset(head.as_object(), ResetType::Hard, None)?;
            end_operation(&repo)?;
            run_picks(&repo, &state.todo[1..], &state.opts, state.orig_head)
        })
    }

    fn pick_abort(&self) -> Result<String, GitError> {
        self.logged("cherry-pick abort", || {
            let repo = self.repo.lock().expect("repo mutex");
            let state = read_pick_state(&repo)?;
            // git's safety check: a HEAD moved since the stop is not rewound.
            let safe = std::fs::read_to_string(repo.path().join("sequencer/abort-safety"))
                .ok()
                .and_then(|s| Oid::from_str(s.trim()).ok());
            let head = repo.head()?.peel_to_commit()?.id();
            let warning = if safe.is_some_and(|s| s != head) {
                "warning: You seem to have moved HEAD. Not rewinding, check your HEAD!".to_owned()
            } else {
                let orig = repo.find_commit(state.orig_head)?;
                repo.reset(orig.as_object(), ResetType::Hard, None)?;
                String::new()
            };
            end_operation(&repo)?;
            Ok(warning)
        })
    }

    fn pick_quit(&self) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        end_operation(&repo)?;
        Ok(())
    }

    fn merge_with(
        &self,
        revs: &[String],
        opts: &crate::MergeOptions,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let rev = revs.first().map(String::as_str).unwrap_or("HEAD");
        self.logged("merge", || {
            let repo = self.repo.lock().expect("repo mutex");
            let autostash = opts.autostash.unwrap_or_else(|| {
                repo.config()
                    .and_then(|c| c.get_bool("merge.autoStash"))
                    .unwrap_or(false)
            });
            let stash = if autostash {
                autostash_create(&repo, report)?
            } else {
                None
            };
            let before = repo.head()?.peel_to_tree()?;
            let result = if revs.len() > 1 {
                octopus(&repo, revs, opts, report)
            } else {
                repo.rev_single(rev)
                    .and_then(|o| o.peel_to_commit())
                    .map_err(GitError::from)
                    .and_then(|source| merge_commit(&repo, &source, rev, opts, report))
            };
            let after = repo.head()?.peel_to_tree()?;
            if result.is_ok() && opts.stat && after.id() != before.id() {
                let stats = repo
                    .diff_tree_to_tree(Some(&before), Some(&after), None)?
                    .stats()?;
                let format = git2::DiffStatsFormat::FULL | git2::DiffStatsFormat::INCLUDE_SUMMARY;
                let buf = stats.to_buf(format, 80)?;
                for line in String::from_utf8_lossy(&buf).lines() {
                    report(OpProgress::Line(line.to_owned()));
                }
            }
            if let Some(id) = stash {
                if result.is_err() && repo.index()?.has_conflicts() {
                    std::fs::write(repo.path().join("MERGE_AUTOSTASH"), format!("{id}\n"))?;
                    report(OpProgress::Line(
                        "When finished, apply stashed changes with `git stash pop`".to_owned(),
                    ));
                } else {
                    report(OpProgress::Line(autostash_apply(&repo, id)?));
                }
            }
            result
        })
    }

    fn merge_abort(&self) -> Result<(), GitError> {
        self.logged("merge abort", || {
            let repo = self.repo.lock().expect("repo mutex");
            // A conflicted merge has not moved HEAD, so hard-resetting to it drops the
            // half-merged index and worktree; cleanup_state clears MERGE_HEAD et al.
            let head = repo.head()?.peel_to_commit()?;
            repo.reset(head.as_object(), git2::ResetType::Hard, None)?;
            end_operation(&repo)?;
            if let Some(id) = take_merge_autostash(&repo) {
                eprintln!("{}", autostash_apply(&repo, id)?);
            }
            Ok(())
        })
    }

    fn merge_quit(&self) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        for file in ["MERGE_HEAD", "MERGE_MODE", "MERGE_MSG", "AUTO_MERGE"] {
            let _ = std::fs::remove_file(repo.path().join(file));
        }
        if let Some(id) = take_merge_autostash(&repo) {
            stash_store(&repo, id)?;
        }
        Ok(())
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
        self.create_tag_at(name, "HEAD", message, false)
    }

    fn create_tag_at(
        &self,
        name: &str,
        rev: &str,
        message: &str,
        force: bool,
    ) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let target = repo.rev_single(rev)?;
        if message.trim().is_empty() {
            repo.tag_lightweight(name, &target, force)?;
        } else {
            let sig = ident_signature(&repo, true)?;
            repo.tag(name, &target, &sig, message, force)?;
        }
        Ok(())
    }

    fn tag_with(
        &self,
        name: &str,
        rev: &str,
        message: Option<&str>,
        cleanup: &str,
        sign: Option<&str>,
        force: bool,
    ) -> Result<(), GitError> {
        let message = match message {
            None => None,
            Some(m) => Some(match cleanup {
                "verbatim" => m.to_owned(),
                "whitespace" => git2::message_prettify(m, None)?,
                "strip" => git2::message_prettify(m, Some(b'#'))?,
                other => {
                    return Err(GitError::Other(format!(
                        "invalid cleanup mode {other}; use strip, whitespace or verbatim"
                    )));
                }
            }),
        };
        let repo = self.repo.lock().expect("repo mutex");
        let target = repo.rev_single(rev)?;
        let Some(m) = message else {
            repo.tag_lightweight(name, &target, force)?;
            return Ok(());
        };
        let refname = format!("refs/tags/{name}");
        if !force && repo.find_reference(&refname).is_ok() {
            return Err(GitError::Other(format!("tag '{name}' already exists")));
        }
        let id = crate::sign::tag_object(
            &repo,
            name,
            &target,
            &ident_signature(&repo, true)?,
            &m,
            sign,
        )?;
        repo.reference(&refname, id, true, "")?;
        Ok(())
    }

    fn verify_tags(&self, names: &[String]) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut out = String::new();
        let mut failed = false;
        for name in names {
            let id = repo
                .find_reference(&format!("refs/tags/{name}"))
                .ok()
                .and_then(|r| r.target())
                .ok_or_else(|| GitError::Other(format!("tag '{name}' not found.")))?;
            let c = crate::sign::check_tag(&repo, id)?;
            out.push_str(&String::from_utf8_lossy(&c.payload));
            if c.result == 'N' {
                out.push_str("error: no signature found\n");
            }
            out.push_str(&c.output);
            failed |= !c.good;
        }
        if failed {
            Err(GitError::Cli(out.trim_end().to_owned()))
        } else {
            Ok(out.trim_end().to_owned())
        }
    }

    fn signature_check(&self, rev: &str, tag: bool) -> Result<crate::SignatureCheck, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let obj = repo.rev_single(rev)?;
        let kind = obj.kind().map_or("unknown", |k| k.str());
        match (tag, obj.kind()) {
            (true, Some(ObjectType::Tag)) => crate::sign::check_tag(&repo, obj.id()),
            (false, Some(ObjectType::Commit)) => crate::sign::check_commit(&repo, obj.id()),
            _ => Err(GitError::Other(format!(
                "{rev}: cannot verify a non-{} object of type {kind}.",
                if tag { "tag" } else { "commit" }
            ))),
        }
    }

    fn is_ancestor(&self, ancestor: &str, rev: &str) -> Result<bool, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let a = repo.rev_single(ancestor)?.peel_to_commit()?.id();
        let r = repo.rev_single(rev)?.peel_to_commit()?.id();
        Ok(a == r || repo.graph_descendant_of(r, a)?)
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
            let url = remote_urls(&repo, name, false)?
                .into_iter()
                .next()
                .unwrap_or_default();
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
        // libgit2 deletes every ref a fetch refspec writes; git only those
        // under refs/remotes, so a mirror's `refs/*:refs/*` must not reach it.
        let key = format!("remote.{name}.fetch");
        let specs = config_values(&repo, &key)?;
        let tracking = |s: &&String| {
            s.split_once(':')
                .is_some_and(|(_, d)| d.starts_with("refs/remotes/"))
        };
        if !specs.iter().all(|s| tracking(&s)) {
            let mut config = open_config(&repo, ConfigScope::Local, true)?;
            config.remove_multivar(&key, ".*")?;
            for spec in specs.iter().filter(tracking) {
                config.set_multivar(&key, "$^", spec)?;
            }
        }
        repo.remote_delete(name)?;
        Ok(())
    }

    fn set_remote_url(&self, name: &str, url: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.remote_set_url(name, url)?;
        Ok(())
    }

    fn remote_urls(&self, name: &str, push: bool) -> Result<Vec<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.find_remote(name)?;
        remote_urls(&repo, name, push)
    }

    fn set_remote_push_url(&self, name: &str, url: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.find_remote(name)?;
        repo.remote_set_pushurl(name, Some(url))?;
        Ok(())
    }

    fn prune_remote(&self, name: &str) -> Result<Vec<String>, GitError> {
        self.logged("remote prune", || {
            let repo = self.repo.lock().expect("repo mutex");
            let cred_guard = self.cred_prompt.lock().expect("cred mutex");
            // What the remote's first URL has, as git reads it.
            let (mut remote, _, _) = fetch_source(&repo, name)?;
            let ignored = std::sync::atomic::AtomicBool::new(false);
            let callbacks = remote_callbacks(&|_| {}, &ignored, cred_guard.as_deref());
            let conn = remote.connect_auth(git2::Direction::Fetch, Some(callbacks), None)?;
            let theirs: Vec<String> = conn.list()?.iter().map(|h| h.name().to_owned()).collect();
            drop(conn);
            let mut pruned = Vec::new();
            for spec in config_values(&repo, &format!("remote.{name}.fetch"))? {
                let Some((src, dst)) = spec.trim_start_matches('+').split_once(':') else {
                    continue;
                };
                let pattern = match dst.split_once('*') {
                    Some((pre, _)) => format!("{pre}*"),
                    None => dst.to_owned(),
                };
                for r in repo.references_glob(&pattern)? {
                    let mut r = r?;
                    if r.symbolic_target().is_ok_and(|t| t.is_some()) {
                        continue;
                    }
                    let Ok(local) = r.name().map(str::to_owned) else {
                        continue;
                    };
                    if map_glob(dst, src, &local).is_some_and(|t| !theirs.contains(&t)) {
                        r.delete()?;
                        pruned.push(short_ref(&local).to_owned());
                    }
                }
            }
            Ok(pruned)
        })
    }

    fn rename_remote(&self, old: &str, new: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        // libgit2 leaves non-default refspecs and push remotes alone; git
        // moves their `refs/remotes/<old>/` and names too.
        let odd = repo.remote_rename(old, new)?;
        let mut config = open_config(&repo, ConfigScope::Local, true)?;
        if !odd.is_empty() {
            let key = format!("remote.{new}.fetch");
            let specs = config_values(&repo, &key)?;
            config.remove_multivar(&key, ".*")?;
            let (from, to) = (
                format!(":refs/remotes/{old}/"),
                format!(":refs/remotes/{new}/"),
            );
            for spec in specs {
                config.set_multivar(&key, "$^", &spec.replace(&from, &to))?;
            }
        }
        let mut renamed = Vec::new();
        config.entries(None)?.for_each(|e| {
            let key = String::from_utf8_lossy(e.name_bytes()).into_owned();
            let push_remote = key == "remote.pushdefault"
                || key.starts_with("branch.") && key.ends_with(".pushremote");
            if push_remote && e.value_bytes() == old.as_bytes() {
                renamed.push(key);
            }
        })?;
        for key in renamed {
            config.set_str(&key, new)?;
        }
        Ok(())
    }

    fn edit_remote_urls(
        &self,
        name: &str,
        url: &str,
        old: Option<&str>,
        push: bool,
        add: bool,
        delete: bool,
    ) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.find_remote(name)?;
        let key = format!("remote.{name}.{}", if push { "pushurl" } else { "url" });
        let mut config = open_config(&repo, ConfigScope::Local, true)?;
        let matching = |re: &str| -> Result<usize, GitError> {
            let mut n = 0;
            if let Ok(mut entries) = config.multivar(&key, Some(re)) {
                while let Some(e) = entries.next() {
                    e?;
                    n += 1;
                }
            }
            Ok(n)
        };
        if add {
            config.set_multivar(&key, "$^", url)?;
        } else if delete {
            let found = matching(url)?;
            if found == 0 {
                return Err(GitError::Other(format!("No such URL found: {url}")));
            }
            if !push && found == config_values(&repo, &key)?.len() {
                return Err(GitError::Other(
                    "Will not delete all non-push URLs".to_owned(),
                ));
            }
            config.remove_multivar(&key, url)?;
        } else if let Some(old) = old {
            if matching(old)? == 0 {
                return Err(GitError::Other(format!("No such URL found: {old}")));
            }
            config.set_multivar(&key, old, url)?;
        } else {
            config.set_str(&key, url)?;
        }
        Ok(())
    }

    fn remote_heads(&self, name: &str) -> Result<crate::backend::RemoteHeads, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        let url = remote_urls(&repo, name, false)?
            .into_iter()
            .next()
            .ok_or_else(|| GitError::Other(format!("remote {name} has no URL")))?;
        let mut remote = repo.remote_anonymous(&url)?;
        let rejected = std::sync::atomic::AtomicBool::new(false);
        let callbacks = remote_callbacks(&|_| {}, &rejected, cred_guard.as_deref());
        let conn = remote.connect_auth(git2::Direction::Fetch, Some(callbacks), None)?;
        let list = conn.list()?;
        let heads = list
            .iter()
            .map(|h| (h.name().to_owned(), h.oid().to_string()))
            .collect();
        let head = list
            .iter()
            .find(|h| h.name() == "HEAD")
            .and_then(|h| h.symref_target())
            .map(|s| short_ref(s).to_owned());
        Ok((heads, head))
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
                oid: head_oid(&repo),
                lock_reason: None,
                prunable: None,
            });
        }

        // Each linked worktree, opened on its own admin folder for
        // branch/HEAD/dirty, so one whose folder is gone still reads.
        let names = repo.worktrees()?;
        for name in names.iter().filter_map(|n| n.ok().flatten()) {
            let Ok(wt) = repo.find_worktree(name) else {
                continue;
            };
            let path = wt.path().to_path_buf();
            let (locked, lock_reason) = match wt.is_locked() {
                Ok(git2::WorktreeLockStatus::Locked(reason)) => (
                    true,
                    reason
                        .map(|r| r.trim_end().to_owned())
                        .filter(|r| !r.is_empty()),
                ),
                _ => (false, None),
            };
            let (branch, head, dirty, oid) = match Repository::open_from_worktree(&wt) {
                Ok(wtr) => {
                    let (b, h) = worktree_head(&wtr);
                    (b, h, path.exists() && repo_is_dirty(&wtr), head_oid(&wtr))
                }
                // libgit2 will not open a worktree whose folder is gone; its
                // HEAD is still in the admin folder.
                Err(_) => {
                    let head = std::fs::read_to_string(
                        repo.commondir().join("worktrees").join(name).join("HEAD"),
                    )
                    .unwrap_or_default();
                    let head = head.trim();
                    let (branch, id) = match head.strip_prefix("ref: ") {
                        Some(r) => (Some(short_ref(r).to_owned()), repo.refname_to_id(r).ok()),
                        None => (None, Oid::from_str(head).ok()),
                    };
                    let short = id
                        .and_then(|id| repo.find_object(id, None).ok())
                        .and_then(|o| o.short_id().ok())
                        .and_then(|b| b.as_str().ok().map(str::to_owned));
                    (branch, short, false, id.map(|id| id.to_string()))
                }
            };
            out.push(crate::Worktree {
                name: name.to_owned(),
                path: path.to_string_lossy().into_owned(),
                branch,
                head,
                dirty,
                locked,
                is_main: false,
                oid,
                lock_reason,
                prunable: (!path.exists())
                    .then(|| "gitdir file points to non-existent location".to_owned()),
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

    fn worktree_add(
        &self,
        path: &str,
        commitish: Option<&str>,
        a: &crate::WorktreeAddArgs,
    ) -> Result<(), GitError> {
        let abs = std::path::absolute(path)?;
        if abs.read_dir().is_ok_and(|mut d| d.next().is_some()) || abs.is_file() {
            return Err(GitError::Other(format!("'{path}' already exists")));
        }
        let repo = self.repo.lock().expect("repo mutex");
        let local = |b: &str| repo.find_branch(b, BranchType::Local).is_ok();
        let branch = |name: &str, start: &str, reset: bool| {
            new_worktree_branch(&repo, name, start, reset, a.track).map(|()| name.to_owned())
        };
        // The branch to check out there, creating it (and tracking a remote
        // branch it starts from) when needed; None to detach.
        let head = match (&a.new_branch, commitish) {
            _ if a.orphan => {
                if commitish.is_some() {
                    return Err(GitError::Other(
                        "--orphan starts an empty branch and takes no commit-ish".to_owned(),
                    ));
                }
                let name = a
                    .new_branch
                    .clone()
                    .unwrap_or_else(|| worktree_base_name(&abs));
                if local(&name) && !a.reset {
                    return Err(GitError::Other(format!(
                        "a branch named '{name}' already exists"
                    )));
                }
                Some(name)
            }
            _ if a.detach => None,
            (Some(b), start) => Some(branch(b, start.unwrap_or("HEAD"), a.reset)?),
            (None, Some(c)) if local(c) => Some(c.to_owned()),
            (None, Some(c)) => {
                // As git does, track the one remote branch of that name.
                let mut remote = Vec::new();
                for b in repo.branches(Some(BranchType::Remote))? {
                    if let Some(n) = b?.0.name()?
                        && n.ends_with(&format!("/{c}"))
                    {
                        remote.push(n.to_owned());
                    }
                }
                match remote.as_slice() {
                    [one] => Some(branch(c, one, false)?),
                    _ => None,
                }
            }
            (None, None) => {
                let name = worktree_base_name(&abs);
                if local(&name) {
                    Some(name)
                } else {
                    Some(branch(&name, "HEAD", false)?)
                }
            }
        };
        let refname = head.as_ref().map(|b| format!("refs/heads/{b}"));
        if let Some(refname) = refname.as_ref().filter(|_| !a.force)
            && let Some(other) = checked_out_at(&repo, refname)?
        {
            return Err(GitError::Other(format!(
                "'{}' is already used by worktree at '{other}'",
                short_ref(refname)
            )));
        }
        let head_line = match &refname {
            Some(r) => format!("ref: {r}\n"),
            None => {
                let commit = repo
                    .rev_single(commitish.unwrap_or("HEAD"))?
                    .peel_to_commit()?;
                format!("{}\n", commit.id())
            }
        };
        let base = worktree_base_name(&abs);
        let mut name = base.clone();
        for n in 1.. {
            if !repo.commondir().join("worktrees").join(&name).exists() {
                break;
            }
            name = format!("{base}{n}");
        }
        // A linked worktree is a `.git` file pointing at an admin folder
        // under the common git dir, which points back; write both as git does.
        std::fs::create_dir_all(&abs)?;
        let abs = std::fs::canonicalize(&abs)?;
        let admin = std::fs::canonicalize(repo.commondir())?
            .join("worktrees")
            .join(&name);
        std::fs::create_dir_all(&admin)?;
        std::fs::write(admin.join("commondir"), "../..\n")?;
        std::fs::write(
            admin.join("gitdir"),
            format!("{}\n", abs.join(".git").display()),
        )?;
        std::fs::write(admin.join("HEAD"), head_line)?;
        if a.lock {
            let reason = a.reason.as_deref().unwrap_or("added with --lock");
            std::fs::write(admin.join("locked"), format!("{reason}\n"))?;
        }
        std::fs::write(abs.join(".git"), format!("gitdir: {}\n", admin.display()))?;
        if !a.orphan && !a.no_checkout {
            let wt = Repository::open(&abs)?;
            wt.checkout_head(Some(CheckoutBuilder::new().force()))?;
        }
        Ok(())
    }

    fn repair_worktrees(&self, paths: &[String]) -> Result<Vec<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut fixed = Vec::new();
        let admin_root = std::fs::canonicalize(repo.commondir())?.join("worktrees");
        // A worktree at a new path: its `.git` file still names its admin
        // folder, which must now point here.
        for p in paths {
            let dotgit = std::fs::canonicalize(p)?.join(".git");
            let text = std::fs::read_to_string(&dotgit)
                .map_err(|_| GitError::Other(format!("{p}: not a linked worktree")))?;
            let admin = PathBuf::from(text.trim().trim_start_matches("gitdir: "));
            let gitdir = admin.join("gitdir");
            let want = format!("{}\n", dotgit.display());
            if std::fs::read_to_string(&gitdir).ok().as_deref() != Some(want.as_str()) {
                std::fs::write(&gitdir, want)?;
                fixed.push(format!("repair: gitdir incorrect: {}", gitdir.display()));
            }
        }
        // Each worktree's `.git` file must name its admin folder, which may
        // have moved with the repository.
        for name in repo.worktrees()?.iter().filter_map(|n| n.ok().flatten()) {
            let admin = admin_root.join(name);
            let Ok(text) = std::fs::read_to_string(admin.join("gitdir")) else {
                continue;
            };
            let dotgit = PathBuf::from(text.trim());
            let want = format!("gitdir: {}\n", admin.display());
            if dotgit.is_file()
                && std::fs::read_to_string(&dotgit).ok().as_deref() != Some(want.as_str())
            {
                std::fs::write(&dotgit, want)?;
                fixed.push(format!("repair: .git file broken: {}", dotgit.display()));
            }
        }
        Ok(fixed)
    }

    fn worktree_lock(&self, worktree: &str, reason: Option<&str>) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        find_worktree(&repo, worktree)?.lock(reason)?;
        Ok(())
    }

    fn worktree_unlock(&self, worktree: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        find_worktree(&repo, worktree)?.unlock()?;
        Ok(())
    }

    fn worktree_move(&self, worktree: &str, new_path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let wt = find_worktree(&repo, worktree)?;
        if let Ok(git2::WorktreeLockStatus::Locked(_)) = wt.is_locked() {
            return Err(GitError::Other(format!(
                "cannot move a locked working tree; run `rgit worktree unlock {worktree}` first"
            )));
        }
        let from = wt.path().to_path_buf();
        let mut to = std::path::absolute(new_path)?;
        if to.is_dir() {
            to = to.join(from.file_name().unwrap_or_default());
        }
        if to.exists() {
            return Err(GitError::Other(format!(
                "'{}' already exists",
                to.display()
            )));
        }
        // A move is a rename plus the admin folder's pointer back to it.
        std::fs::rename(&from, &to)?;
        let to = std::fs::canonicalize(&to)?;
        let admin = repo
            .commondir()
            .join("worktrees")
            .join(wt.name()?.unwrap_or_default());
        std::fs::write(
            admin.join("gitdir"),
            format!("{}\n", to.join(".git").display()),
        )?;
        Ok(())
    }

    fn remove_worktree(&self, name: &str, force: bool) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let wt = find_worktree(&repo, name)?;
        if !force && Repository::open(wt.path()).is_ok_and(|r| repo_is_dirty(&r)) {
            return Err(GitError::Other(format!(
                "worktree {name} has modified or untracked files; use --force to remove it"
            )));
        }
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

    fn prune_objects(
        &self,
        expire: Option<&str>,
        dry_run: bool,
        verbose: bool,
    ) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cut =
            expire.map_or_else(|| Ok(crate::maintenance::now()), crate::maintenance::cutoff)?;
        Ok(crate::maintenance::prune(&repo, cut, dry_run, verbose)?.join("\n"))
    }

    fn config_get(&self, key: &str) -> Result<Option<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cfg = open_config(&repo, ConfigScope::Any, false)?;
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
        let repo = self.fresh_handle()?;
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
        let repo = self.fresh_handle()?;
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

    fn copy_branch(&self, old: &str, new: &str, force: bool) -> Result<(), GitError> {
        self.logged("copy branch", || {
            let repo = self.repo.lock().expect("repo mutex");
            let (from, to) = (format!("refs/heads/{old}"), format!("refs/heads/{new}"));
            let commit = repo.find_reference(&from)?.peel_to_commit()?;
            repo.branch(new, &commit, force)?;
            let src = repo.reflog(&from)?;
            let mut log = repo.reflog(&to)?;
            while !log.is_empty() {
                log.remove(0, false)?;
            }
            for e in src.iter().rev() {
                log.append(e.id_new(), &e.committer(), e.message()?)?;
            }
            let msg = format!("Branch: copied {from} to {to}");
            log.append(commit.id(), &ident_signature(&repo, true)?, Some(&msg))?;
            log.write()?;
            let mut config = open_config(&repo, ConfigScope::Local, true)?;
            let mut copied = Vec::new();
            config.entries(None)?.for_each(|e| {
                let name = String::from_utf8_lossy(e.name_bytes());
                if let Some(key) = name.strip_prefix(&format!("branch.{old}.")) {
                    copied.push((
                        format!("branch.{new}.{key}"),
                        String::from_utf8_lossy(e.value_bytes()).into_owned(),
                    ));
                }
            })?;
            for (name, _) in &copied {
                let _ = config.remove_multivar(name, ".*");
            }
            for (name, value) in copied {
                config.set_multivar(&name, "$^", &value)?;
            }
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

    fn branch_from(
        &self,
        name: &str,
        start: &str,
        force: bool,
        track: bool,
    ) -> Result<(), GitError> {
        self.logged("create branch", || {
            let repo = self.repo.lock().expect("repo mutex");
            if !git2::Branch::name_is_valid(name)? {
                return Err(GitError::Other(format!(
                    "'{name}' is not a valid branch name"
                )));
            }
            let refname = format!("refs/heads/{name}");
            if !force && repo.find_reference(&refname).is_ok() {
                return Err(GitError::Other(format!(
                    "a branch named '{name}' already exists"
                )));
            }
            let commit = repo.rev_single(start)?.peel_to_commit()?;
            repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe()))?;
            // A plain reference, not `branch()`, which refuses to reset HEAD's branch.
            repo.reference(
                &refname,
                commit.id(),
                true,
                &format!("branch: Created from {start}"),
            )?;
            repo.set_head(&refname)?;
            if track || repo.find_branch(start, BranchType::Remote).is_ok() {
                repo.find_branch(name, BranchType::Local)?
                    .set_upstream(Some(start))?;
            }
            Ok(())
        })
    }

    fn create_branch_at(&self, name: &str, start: &str, force: bool) -> Result<(), GitError> {
        self.logged("create branch", || {
            let repo = self.repo.lock().expect("repo mutex");
            let commit = repo.rev_single(start)?.peel_to_commit()?;
            let mut branch = repo.branch(name, &commit, force)?;
            if repo.find_branch(start, BranchType::Remote).is_ok() {
                branch.set_upstream(Some(start))?;
            }
            Ok(())
        })
    }

    fn previous_checkout(&self) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let (object, reference) = repo
            .revparse_ext("@{-1}")
            .map_err(|_| GitError::Other("no previous branch to switch to".to_owned()))?;
        Ok(match reference.as_ref().filter(|r| r.is_branch()) {
            Some(r) => r.shorthand().unwrap_or_default().to_owned(),
            None => object.id().to_string(),
        })
    }

    fn set_upstream(&self, name: &str, upstream: Option<&str>) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.find_branch(name, BranchType::Local)?
            .set_upstream(upstream)?;
        Ok(())
    }

    fn branch_upstream(&self, name: &str) -> Result<Option<(String, usize, usize)>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let branch = repo.find_branch(name, BranchType::Local)?;
        let Ok(upstream) = branch.upstream() else {
            return Ok(None);
        };
        let up_name = upstream.name()?.unwrap_or_default().to_owned();
        let (ahead, behind) = match (branch.get().target(), upstream.get().target()) {
            (Some(local), Some(up)) => repo.graph_ahead_behind(local, up)?,
            _ => (0, 0),
        };
        Ok(Some((up_name, ahead, behind)))
    }

    fn discard_file(&self, path: &str) -> Result<(), GitError> {
        let path = root_dot(path);
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
        self.commit_with(message, &CommitOptions::default())
    }

    fn amend(&self, message: &str) -> Result<(), GitError> {
        self.commit_with(
            message,
            &CommitOptions {
                amend: true,
                ..CommitOptions::default()
            },
        )
    }

    fn commit_with(&self, message: &str, opts: &CommitOptions) -> Result<(), GitError> {
        self.logged(if opts.amend { "amend" } else { "commit" }, || {
            let repo = self.repo.lock().expect("repo mutex");
            write_commit(&repo, message, opts)
        })
    }

    fn reword(&self, rev: &str, message: &str) -> Result<(), GitError> {
        self.logged("reword", || {
            {
                let repo = self.repo.lock().expect("repo mutex");
                let target = repo.rev_single(rev)?.peel_to_commit()?.id();
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
            let target = repo.rev_single(&format!("HEAD~{}", n.max(1)))?;
            repo.reset(&target, ResetType::Soft, None)?;
            Ok(())
        })
    }

    fn split(&self, rev: &str, paths: &[String]) -> Result<(), GitError> {
        self.logged("split", || {
            {
                let repo = self.repo.lock().expect("repo mutex");
                let branch_ref = head_branch_ref(&repo)?;
                let target = repo.rev_single(rev)?.peel_to_commit()?;
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

                let sig = ident_signature(&repo, true)?;
                let full_msg = target.message().unwrap_or("");
                // Part 1 keeps only the subject (a fresh change id); part 2 keeps the
                // full message and the original change id.
                let subject = full_msg.lines().next().unwrap_or("").to_owned();
                let c1 = crate::sign::commit_configured(
                    &repo,
                    None,
                    &target.author(),
                    &sig,
                    &subject,
                    &part1_tree,
                    &[&parent],
                )?;
                let c1_commit = repo.find_commit(c1)?;
                let c2 = crate::sign::commit_configured(
                    &repo,
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
            let base_oid = repo.rev_single(base)?.peel_to_commit()?.id();
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
                let rev_oid = repo.rev_single(rev)?.peel_to_commit()?.id();
                let target_oid = repo.rev_single(target)?.peel_to_commit()?.id();
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
                let base = repo.rev_single(from)?.peel_to_commit()?;
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
                let sig = ident_signature(&repo, true)?;
                let new = crate::sign::commit_configured(
                    &repo,
                    None,
                    &head.author(),
                    &sig,
                    &msg,
                    &head.tree()?,
                    &[&base],
                )?;
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
                let target = repo.rev_single(rev)?.peel_to_commit()?;
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
                let sig = ident_signature(&repo, true)?;
                let grandparents: Vec<git2::Commit> = (0..parent.parent_count())
                    .filter_map(|k| parent.parent(k).ok())
                    .collect();
                let gp_refs: Vec<&git2::Commit> = grandparents.iter().collect();
                let squashed = crate::sign::commit_configured(
                    &repo,
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
        hooks_dir(&self.repo.lock().expect("repo mutex"))
    }

    fn commit_no_verify(&self, message: &str) -> Result<(), GitError> {
        self.commit_with(
            message,
            &CommitOptions {
                no_verify: true,
                ..CommitOptions::default()
            },
        )
    }

    fn amend_no_verify(&self, message: &str) -> Result<(), GitError> {
        self.commit_with(
            message,
            &CommitOptions {
                amend: true,
                no_verify: true,
                ..CommitOptions::default()
            },
        )
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

    fn prepared_message(&self) -> Option<String> {
        let repo = self.repo.lock().expect("repo mutex");
        ["SQUASH_MSG", "MERGE_MSG"]
            .iter()
            .find_map(|f| std::fs::read_to_string(repo.path().join(f)).ok())
            .map(|m| strip_comments(&m))
            .filter(|m| !m.trim().is_empty())
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
        // As in git, a group (`remotes.<group>`) stands for its remotes.
        let expand = |name: &str| -> Result<Vec<String>, GitError> {
            let group: Vec<String> = config_values(&repo, &format!("remotes.{name}"))?
                .iter()
                .flat_map(|v| v.split_whitespace().map(str::to_owned))
                .collect();
            match group.is_empty() {
                false => Ok(group),
                true if repo.find_remote(name).is_ok() => Ok(vec![name.to_owned()]),
                true => Err(GitError::Other(format!(
                    "no such remote or remote group: {name}"
                ))),
            }
        };
        let (names, multiple): (Vec<String>, bool) = if args.all {
            let config = repo.config()?;
            let names: Vec<String> = repo
                .remotes()?
                .iter()
                .flatten()
                .flatten()
                .filter(|r| {
                    !config
                        .get_bool(&format!("remote.{r}.skipFetchAll"))
                        .unwrap_or(false)
                })
                .map(str::to_owned)
                .collect();
            let many = names.len() > 1;
            (names, many)
        } else if !args.remotes.is_empty() {
            let mut names = Vec::new();
            for r in &args.remotes {
                names.extend(expand(r)?);
            }
            (names, true)
        } else {
            match remote {
                Some(r) if repo.find_remote(r).is_err() => {
                    let names = expand(r)?;
                    if names.len() > 1 && !refspecs.is_empty() {
                        return Err(GitError::Other(
                            "fetching a group and specifying refspecs does not make sense"
                                .to_owned(),
                        ));
                    }
                    let many = names.len() > 1;
                    (names, many)
                }
                Some(r) => (vec![r.to_owned()], false),
                None => (vec![upstream_remote(&repo)?.0], false),
            }
        };
        if multiple {
            self.fetch_many(&repo, &names, args, report, cred)?;
        } else {
            for name in &names {
                self.fetch_remote(&repo, name, refspecs, args, report, cred)?;
            }
        }
        if args.set_upstream && !args.dry_run {
            let name = &names[0];
            let merge = match refspecs.first() {
                Some(s) => {
                    let src = s.trim_start_matches('+').split(':').next().unwrap_or("");
                    match src.starts_with("refs/") {
                        true => src.to_owned(),
                        false => format!("refs/heads/{src}"),
                    }
                }
                None => repo
                    .find_reference(&format!("refs/remotes/{name}/HEAD"))
                    .ok()
                    .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
                    .and_then(|t| map_glob(&format!("refs/remotes/{name}/*"), "refs/heads/*", &t))
                    .ok_or_else(|| {
                        GitError::Other(format!("no branch to track: {name}/HEAD is not set"))
                    })?,
            };
            let branch = current_branch(&repo)?;
            let mut config = repo.config()?;
            config.set_str(&format!("branch.{branch}.remote"), name)?;
            config.set_str(&format!("branch.{branch}.merge"), &merge)?;
        }
        Ok(())
    }

    fn pull(
        &self,
        remote: Option<&str>,
        branch: Option<&str>,
        args: &crate::PullArgs,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let (remote, branch, rebase, ff_only, chosen, autostash) = {
            let repo = self.repo.lock().expect("repo mutex");
            let (upstream, current) = upstream_remote(&repo)?;
            let config = repo.config()?;
            // git refuses a diverged pull until a flag or config picks merge or rebase.
            let chosen = args.rebase.is_some()
                || args.ff_only
                || args.squash
                || config_rebase(&config, &format!("branch.{current}.rebase")).is_some()
                || config_rebase(&config, "pull.rebase").is_some()
                || config.get_string("pull.ff").is_ok();
            let merge_branch = repo
                .branch_upstream_merge(&format!("refs/heads/{current}"))
                .ok()
                .and_then(|b| b.as_str().ok().map(|s| short_ref(s).to_owned()));
            let rebase = !args.ff_only
                && args.rebase.unwrap_or_else(|| {
                    config_rebase(&config, &format!("branch.{current}.rebase"))
                        .or_else(|| config_rebase(&config, "pull.rebase"))
                        .unwrap_or(false)
                });
            let ff_only = args.ff_only || config.get_string("pull.ff").is_ok_and(|v| v == "only");
            let key = if rebase {
                "rebase.autoStash"
            } else {
                "merge.autoStash"
            };
            let autostash = args
                .autostash
                .unwrap_or_else(|| config.get_bool(key).unwrap_or(false));
            (
                remote.map_or(upstream, str::to_owned),
                branch
                    .map(str::to_owned)
                    .or(merge_branch)
                    .unwrap_or(current),
                rebase,
                ff_only,
                chosen,
                autostash,
            )
        };
        self.logged(if rebase { "pull --rebase" } else { "pull" }, || {
            let mut repo = self.repo.lock().expect("repo mutex");
            {
                let cred_guard = self.cred_prompt.lock().expect("cred mutex");
                let cred = cred_guard.as_deref();
                let fetch = crate::FetchArgs {
                    depth: args.depth,
                    ..Default::default()
                };
                let names: Vec<String> = match args.all {
                    true => repo
                        .remotes()?
                        .iter()
                        .flatten()
                        .flatten()
                        .map(str::to_owned)
                        .collect(),
                    false => vec![remote.clone()],
                };
                for name in &names {
                    self.fetch_remote(&repo, name, &[], &fetch, report, cred)?;
                }
            }
            let target = repo.refname_to_id(&format!("refs/remotes/{remote}/{branch}"))?;
            let mut status = StatusOptions::new();
            status.include_untracked(false);
            // A rebase keeps its own autostash, restored when it ends.
            let dirty = autostash
                && !rebase
                && repo
                    .statuses(Some(&mut status))?
                    .iter()
                    .any(|e| e.status() != Status::CURRENT);
            if dirty {
                let sig = stash_signature(&repo, true)?;
                let id = repo.stash_save2(&sig, Some("autostash"), None)?;
                report(OpProgress::Line(format!(
                    "Created autostash: {}",
                    short7(id)
                )));
            }
            let result = if rebase {
                // git's pull narrows the upstream by its reflog, like --fork-point.
                let opts = crate::RebaseOptions {
                    fork_point: Some(true),
                    autostash,
                    strategy_option: args.strategy_option.clone(),
                    ..Default::default()
                };
                let upstream = format!("refs/remotes/{remote}/{branch}");
                crate::rebase::start(&mut repo, Some(&upstream), &opts).map(|out| {
                    for line in out.lines() {
                        report(OpProgress::Line(line.to_owned()));
                    }
                })
            } else {
                let url = remote_urls(&repo, &remote, false)?
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| remote.clone());
                let source = repo.find_commit(target)?;
                let (analysis, _) = repo.merge_analysis(&[&repo.find_annotated_commit(target)?])?;
                if !chosen && !analysis.is_up_to_date() && !analysis.is_fast_forward() {
                    Err(GitError::Other(
                        "you have divergent branches and need to specify how to reconcile \
                         them: pass --rebase, --no-rebase or --ff-only, or set pull.rebase"
                            .to_owned(),
                    ))
                } else {
                    let name = format!("branch '{branch}' of {url}");
                    merge_commit(
                        &repo,
                        &source,
                        &name,
                        &crate::MergeOptions {
                            ff_only,
                            message: Some(format!("Merge {name}")),
                            squash: args.squash,
                            no_ff: args.no_ff,
                            no_commit: args.no_commit,
                            strategy_option: args.strategy_option.clone(),
                            ..Default::default()
                        },
                        report,
                    )
                }
            };
            if dirty {
                if result.is_ok() && repo.stash_pop(0, None).is_ok() {
                    report(OpProgress::Line("Applied autostash.".to_owned()));
                } else {
                    report(OpProgress::Line(
                        "Your changes are safe in the stash; run `rgit stash pop` when ready."
                            .to_owned(),
                    ));
                }
            }
            result
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

    fn fetch_pack(
        &self,
        url: &str,
        refs: &[String],
        all: bool,
        depth: Option<i32>,
    ) -> Result<crate::backend::FetchedRefs, GitError> {
        let repo = self.fresh_handle()?;
        let mut remote = repo.remote_anonymous(url)?;
        remote.connect(git2::Direction::Fetch)?;
        let heads: Vec<(String, String)> = remote
            .list()?
            .iter()
            .filter(|h| !h.name().ends_with("^{}"))
            .map(|h| (h.oid().to_string(), h.name().to_owned()))
            .collect();
        remote.disconnect()?;
        let got: Vec<(String, String)> = heads
            .into_iter()
            .filter(|(_, name)| all || refs.contains(name))
            .collect();
        let absent = refs
            .iter()
            .filter(|r| !got.iter().any(|(_, n)| n == *r))
            .cloned()
            .collect();
        let specs: Vec<&str> = got.iter().map(|(_, n)| n.as_str()).collect();
        if !specs.is_empty() {
            let mut opts = FetchOptions::new();
            opts.update_fetchhead(false);
            if let Some(d) = depth {
                opts.depth(d);
            }
            remote.download(&specs, Some(&mut opts))?;
        }
        Ok((got, absent))
    }

    fn send_pack(
        &self,
        url: &str,
        refspecs: &[String],
        args: &crate::PushArgs,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let repo = self.fresh_handle()?;
        let mut remote = repo.remote_anonymous(url)?;
        let mut specs = refspecs
            .iter()
            .map(|s| expand_push_refspec(&repo, url, s))
            .collect::<Result<Vec<_>, _>>()?;
        let mine = |prefix: &str| -> Result<Vec<String>, GitError> {
            let mut names = Vec::new();
            for r in repo.references()? {
                let r = r?;
                if let (Ok(name), Some(_)) = (r.name(), r.target())
                    && name.starts_with(prefix)
                {
                    names.push(name.to_owned());
                }
            }
            Ok(names)
        };
        if args.mirror {
            specs.extend(mine("refs/")?.into_iter().map(|n| (true, n.clone(), n)));
        } else if args.all {
            specs.extend(
                mine("refs/heads/")?
                    .into_iter()
                    .map(|n| (false, n.clone(), n)),
            );
        } else if specs.is_empty() {
            // git's "matching" refs: the branches the remote has too.
            remote.connect(git2::Direction::Push)?;
            let theirs: Vec<String> = remote.list()?.iter().map(|h| h.name().to_owned()).collect();
            remote.disconnect()?;
            for n in mine("refs/heads/")? {
                if theirs.contains(&n) {
                    specs.push((false, n.clone(), n));
                }
            }
        }
        let args = crate::PushArgs {
            no_verify: true,
            ..args.clone()
        };
        push_one(
            &repo,
            &mut remote,
            url,
            url,
            specs,
            &[],
            &args,
            report,
            None,
            false,
        )
    }

    fn push_to(
        &self,
        remote: Option<&str>,
        refspecs: &[String],
        args: &crate::PushArgs,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let repo = self.fresh_handle()?;
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        let cred = cred_guard.as_deref();
        let remote_name = match remote {
            Some(r) => r.to_owned(),
            None if refspecs.is_empty() && !args.all && !args.tags && !args.mirror => {
                upstream_remote(&repo)?.0
            }
            None => upstream_remote(&repo)
                .map(|(r, _)| r)
                .or_else(|_| default_remote(&repo))?,
        };
        // (force, source ref, destination ref); an empty source deletes.
        let mut specs = refspecs
            .iter()
            .map(|s| expand_push_refspec(&repo, &remote_name, s))
            .collect::<Result<Vec<_>, _>>()?;
        if args.mirror {
            for r in repo.references()? {
                let r = r?;
                if let (Ok(name), Some(_)) = (r.name(), r.target()) {
                    specs.push((true, name.to_owned(), name.to_owned()));
                }
            }
        } else if args.all {
            for b in repo.branches(Some(BranchType::Local))? {
                if let Ok(name) = b?.0.get().name() {
                    specs.push((false, name.to_owned(), name.to_owned()));
                }
            }
        }
        if args.tags && !args.mirror {
            for t in repo.tag_names(None)?.iter().flatten().flatten() {
                let name = format!("refs/tags/{t}");
                specs.push((false, name.clone(), name));
            }
        }
        if specs.is_empty() {
            if args.tags {
                report(OpProgress::Line("Everything up-to-date".to_owned()));
                return Ok(());
            }
            specs = default_push_specs(&repo, &remote_name)?;
        }
        // --follow-tags: annotated tags on the pushed history; one the remote
        // already has with another value is left alone, as in git.
        let mut followed = Vec::new();
        if args.follow_tags || repo.config()?.get_bool("push.followTags").unwrap_or(false) {
            let tips: Vec<Oid> = specs
                .iter()
                .filter_map(|(_, src, _)| {
                    Some(repo.rev_single(src).ok()?.peel_to_commit().ok()?.id())
                })
                .collect();
            for t in repo.tag_names(None)?.iter().flatten().flatten() {
                let name = format!("refs/tags/{t}");
                let Ok(obj) = repo.rev_single(&name) else {
                    continue;
                };
                let Ok(target) = obj.peel_to_commit() else {
                    continue;
                };
                if obj.kind() == Some(ObjectType::Tag)
                    && !specs.iter().any(|(_, _, d)| *d == name)
                    && tips.iter().any(|&tip| {
                        tip == target.id()
                            || repo.graph_descendant_of(tip, target.id()).unwrap_or(false)
                    })
                {
                    specs.push((false, name.clone(), name.clone()));
                    followed.push(name);
                }
            }
        }
        let mode = match &args.recurse_submodules {
            Some(m) => m.clone(),
            None => repo
                .config()?
                .get_string("push.recurseSubmodules")
                .unwrap_or_default(),
        };
        if matches!(mode.as_str(), "check" | "on-demand" | "only") {
            push_submodules(&repo, &remote_name, &specs, refspecs, args, &mode, report)?;
            if mode == "only" {
                return Ok(());
            }
        }
        let urls = remote_urls(&repo, &remote_name, true)?;
        if urls.len() < 2 {
            let mut remote = repo.find_remote(&remote_name)?;
            let url = remote.url().unwrap_or(&remote_name).to_owned();
            return push_one(
                &repo,
                &mut remote,
                &url,
                &remote_name,
                specs,
                &followed,
                args,
                report,
                cred,
                false,
            );
        }
        // Like git, push to each URL in turn and report every one.
        let lines = Mutex::new(Vec::new());
        let buffer = |p: OpProgress| match p {
            OpProgress::Line(l) => lines.lock().expect("lines mutex").push(l),
            other => report(other),
        };
        let mut failed = false;
        for url in &urls {
            let mut remote = repo.remote_anonymous(url)?;
            let pushed = push_one(
                &repo,
                &mut remote,
                url,
                &remote_name,
                specs.clone(),
                &followed,
                args,
                &buffer,
                cred,
                true,
            );
            if let Err(e) = pushed {
                failed = true;
                let text = match e {
                    GitError::PushFailed(t) => t,
                    e => e.to_string(),
                };
                lines.lock().expect("lines mutex").push(text);
            }
        }
        let lines = lines.into_inner().expect("lines mutex");
        if failed {
            return Err(GitError::PushFailed(lines.join("\n")));
        }
        for line in lines {
            report(OpProgress::Line(line));
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
        self.push_to(remote, &refspecs, &crate::PushArgs::default(), report)
    }

    fn push_delete(
        &self,
        remote: Option<&str>,
        branch: &str,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        self.logged("push delete", || {
            // An empty source ref deletes the destination on the remote.
            self.push_to(
                remote,
                &[format!(":refs/heads/{branch}")],
                &crate::PushArgs::default(),
                report,
            )
        })
    }

    fn submodules(&self, recursive: bool) -> Result<Vec<crate::SubmoduleInfo>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut out = Vec::new();
        list_submodules(&repo, "", recursive, &mut out)?;
        Ok(out)
    }

    fn submodule(
        &self,
        op: &crate::SubmoduleOp,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        use crate::SubmoduleOp as Op;
        if let Op::Summary { args } = op {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            return crate::submodule::summary(&repo, args, report);
        }
        self.logged("submodule", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            submodule_op(&repo, op, report)
        })
    }

    fn resolve_object(&self, rev: &str) -> Result<String, GitError> {
        crate::plumbing::resolve(&self.repo.lock().expect("repo mutex"), rev)
    }

    fn object_disk(&self, id: &str) -> Result<(u64, Option<String>), GitError> {
        crate::plumbing::object_disk(&self.repo.lock().expect("repo mutex"), id)
    }

    fn diff_pair(&self, pair: &crate::RawPair, context: Option<u32>) -> Result<FileDiff, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let zero = |id: &str| id.bytes().all(|b| b == b'0');
        let content = |id: &str, side: bool| -> Result<Option<Vec<u8>>, GitError> {
            if !side {
                return Ok(None);
            }
            match repo.find_blob(Oid::from_str(id)?) {
                Ok(blob) => Ok(Some(blob.content().to_vec())),
                Err(_) => Err(GitError::Other(format!("unable to read {id}"))),
            }
        };
        let (added, deleted) = (pair.status == 'A', pair.status == 'D');
        let old = content(&pair.old_id, !added)?;
        let new = content(&pair.new_id, !deleted)?;
        let mut opts = DiffOptions::new();
        if let Some(n) = context {
            opts.context_lines(n);
        }
        let mut patch = Patch::from_buffers(
            old.as_deref().unwrap_or_default(),
            Some(Path::new(&pair.old_path)),
            new.as_deref().unwrap_or_default(),
            Some(Path::new(&pair.new_path)),
            Some(&mut opts),
        )?;
        let mut file = patch_file(&mut patch)?;
        let abbrev = |id: &str| -> Result<String, GitError> {
            if zero(id) {
                Ok("0".repeat(7))
            } else {
                crate::plumbing::abbrev(&repo, id, 0)
            }
        };
        let (a, b) = (&pair.old_path, &pair.new_path);
        let mut h = format!("diff --git a/{a} b/{b}\n");
        let score = |kind: &str| {
            format!(
                "similarity index {}%\n{kind} from {a}\n{kind} to {b}\n",
                pair.score
            )
        };
        let mut xfrm = match pair.status {
            'R' => score("rename"),
            'C' => score("copy"),
            _ => String::new(),
        };
        if pair.old_id != pair.new_id || zero(&pair.new_id) {
            xfrm.push_str(&format!(
                "index {}..{}",
                abbrev(&pair.old_id)?,
                abbrev(&pair.new_id)?
            ));
            if !added && !deleted && pair.old_mode == pair.new_mode {
                xfrm.push_str(&format!(" {:06o}", pair.old_mode));
            }
            xfrm.push('\n');
        }
        if added {
            h.push_str(&format!("new file mode {:06o}\n", pair.new_mode));
        } else if deleted {
            h.push_str(&format!("deleted file mode {:06o}\n", pair.old_mode));
        } else if pair.old_mode != pair.new_mode {
            h.push_str(&format!(
                "old mode {:06o}\nnew mode {:06o}\n",
                pair.old_mode, pair.new_mode
            ));
        }
        h.push_str(&xfrm);
        let (la, lb) = (
            if added {
                "/dev/null".to_owned()
            } else {
                format!("a/{a}")
            },
            if deleted {
                "/dev/null".to_owned()
            } else {
                format!("b/{b}")
            },
        );
        if file.binary {
            h.push_str(&format!("Binary files {la} and {lb} differ\n"));
        } else if !file.hunks.is_empty() {
            h.push_str(&format!("--- {la}\n+++ {lb}\n"));
        }
        file.header = h;
        file.path = b.clone();
        file.old_path = matches!(pair.status, 'R' | 'C').then(|| a.clone());
        file.similarity = pair.score;
        file.status = match pair.status {
            'A' => StatusCode::Added,
            'D' => StatusCode::Deleted,
            'R' => StatusCode::Renamed,
            'C' => StatusCode::Copied,
            'T' => StatusCode::TypeChanged,
            _ => StatusCode::Modified,
        };
        Ok(file)
    }

    fn object_header(&self, id: &str) -> Result<(String, u64), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let (size, kind) = repo.odb()?.read_header(Oid::from_str(id)?)?;
        Ok((kind.str().to_owned(), size as u64))
    }

    fn rev_list_bisect(
        &self,
        tips: &[String],
        hidden: &[String],
        first_parent: bool,
        paths: &[String],
        all: bool,
    ) -> Result<crate::BisectPick<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let ids = |revs: &[String]| -> Result<Vec<Oid>, GitError> {
            revs.iter()
                .map(|r| Ok(repo.revparse_single(r)?.peel_to_commit()?.id()))
                .collect()
        };
        let (list, reaches, nr) = crate::bisect::rev_list_bisect(
            &repo,
            &ids(tips)?,
            &ids(hidden)?,
            first_parent,
            paths,
            all,
        )?;
        let list = list.into_iter().map(|(c, d)| (c.to_string(), d)).collect();
        Ok((list, reaches, nr))
    }

    fn convert_blob(&self, path: &str, data: &[u8], textconv: bool) -> Result<Vec<u8>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        if textconv {
            crate::plumbing::textconv(&repo, path, data)
        } else {
            crate::plumbing::smudge(&repo, path, data)
        }
    }

    fn abbrev_id(&self, id: &str, min: usize) -> Result<String, GitError> {
        crate::plumbing::abbrev(&self.repo.lock().expect("repo mutex"), id, min)
    }

    fn full_ref_name(&self, rev: &str) -> Result<Option<String>, GitError> {
        crate::plumbing::full_ref_name(&self.repo.lock().expect("repo mutex"), rev)
    }

    fn symbolic_ref(&self, name: &str) -> Result<Option<String>, GitError> {
        crate::plumbing::symbolic_ref(&self.repo.lock().expect("repo mutex"), name)
    }

    fn set_symbolic_ref(
        &self,
        name: &str,
        target: &str,
        message: Option<&str>,
    ) -> Result<(), GitError> {
        self.logged("symbolic-ref", || {
            let repo = self.repo.lock().expect("repo mutex");
            repo.reference_symbolic(name, target, true, message.unwrap_or("symbolic-ref"))?;
            Ok(())
        })
    }

    fn git_dir(&self) -> PathBuf {
        self.repo.lock().expect("repo mutex").path().to_path_buf()
    }

    fn read_object(&self, rev: &str) -> Result<crate::RawObject, GitError> {
        crate::plumbing::read_object(&self.repo.lock().expect("repo mutex"), rev)
    }

    fn all_objects(&self) -> Result<Vec<String>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut ids = std::collections::BTreeSet::new();
        repo.odb()?.foreach(|id| {
            ids.insert(id.to_string());
            true
        })?;
        Ok(ids.into_iter().collect())
    }

    fn ls_tree(
        &self,
        rev: &str,
        paths: &[String],
        walk: crate::TreeWalk,
    ) -> Result<Vec<crate::TreeItem>, GitError> {
        crate::plumbing::ls_tree(&self.repo.lock().expect("repo mutex"), rev, paths, walk)
    }

    fn index_entries(&self) -> Result<Vec<crate::IndexItem>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::plumbing::index_entries(&repo)
    }

    fn path_states(&self, ignored: bool) -> Result<Vec<(String, crate::PathState)>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::plumbing::path_states(&repo, ignored)
    }

    fn ref_details(&self) -> Result<Vec<crate::RefDetail>, GitError> {
        crate::plumbing::ref_details(&self.repo.lock().expect("repo mutex"))
    }

    fn rev_walk(&self, walk: &crate::LogOptions) -> Result<Vec<crate::WalkCommit>, GitError> {
        crate::plumbing::rev_walk(&self.repo.lock().expect("repo mutex"), walk)
    }

    fn list_objects(
        &self,
        commits: &[String],
        edges: &[String],
    ) -> Result<Vec<(String, String, bool)>, GitError> {
        crate::plumbing::list_objects(&self.repo.lock().expect("repo mutex"), commits, edges)
    }

    fn merge_bases(&self, a: &str, b: &str, all: bool) -> Result<Vec<String>, GitError> {
        crate::plumbing::merge_bases(&self.repo.lock().expect("repo mutex"), a, b, all)
    }

    fn reflog(&self, name: &str) -> Result<Vec<crate::ReflogItem>, GitError> {
        crate::plumbing::reflog(&self.repo.lock().expect("repo mutex"), name)
    }

    fn git_grep(&self, q: &crate::GitGrep) -> Result<Vec<crate::GrepHit>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::plumbing::grep(&repo, &self.workdir, q)
    }

    fn check_ignore(
        &self,
        path: &str,
        no_index: bool,
    ) -> Result<Option<crate::IgnoreRule>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::plumbing::check_ignore(&repo, &self.workdir, path, no_index)
    }

    fn ident(&self, committer: bool) -> Result<String, GitError> {
        crate::plumbing::ident(&self.repo.lock().expect("repo mutex"), committer)
    }

    fn count_objects(&self) -> Result<crate::ObjectCounts, GitError> {
        crate::plumbing::count_objects(&self.repo.lock().expect("repo mutex"))
    }
}

/// Push `specs` (with the `followed` tags) through `remote` at `url` and
/// report it as git does; `anonymous` says `remote` stands in for
/// `remote_name` at one of its URLs.
#[allow(clippy::too_many_arguments)]
fn push_one(
    repo: &Repository,
    remote: &mut git2::Remote<'_>,
    url: &str,
    remote_name: &str,
    mut specs: Vec<(bool, String, String)>,
    followed: &[String],
    args: &crate::PushArgs,
    report: &dyn Fn(OpProgress),
    cred: Option<&dyn crate::CredentialPrompt>,
    anonymous: bool,
) -> Result<(), GitError> {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    let prune: &[&str] = match (args.mirror, args.prune, args.all, args.tags) {
        (true, ..) => &["refs/"],
        (_, true, true, true) => &["refs/heads/", "refs/tags/"],
        (_, true, true, _) => &["refs/heads/"],
        (_, true, _, true) => &["refs/tags/"],
        _ => &[],
    };
    if !prune.is_empty() {
        let ignored = AtomicBool::new(false);
        let callbacks = remote_callbacks(report, &ignored, cred);
        let conn = remote.connect_auth(git2::Direction::Push, Some(callbacks), None)?;
        let theirs: Vec<String> = conn.list()?.iter().map(|h| h.name().to_owned()).collect();
        drop(conn);
        for name in theirs {
            if prune.iter().any(|p| name.starts_with(p))
                && !name.ends_with("^{}")
                && !specs.iter().any(|(_, _, d)| *d == name)
            {
                specs.push((false, String::new(), name));
            }
        }
    }

    let forced_all = args.force || args.force_with_lease || args.mirror;
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
    let push_options: Vec<&str> = args.push_options.iter().map(String::as_str).collect();
    let rows: Mutex<Vec<PushRow>> = Mutex::new(Vec::new());
    let hook_failed: Mutex<Option<String>> = Mutex::new(None);
    let mut failed: Vec<PushRow> = Vec::new();
    let mut todo = specs.clone();
    // A rejected ref (checked here, before anything is sent) aborts the
    // push; unless atomic, the rest are pushed again without it, as git
    // pushes every ref it can.
    loop {
        let (retry, stop) = (AtomicBool::new(false), AtomicBool::new(false));
        let forced: std::collections::HashMap<String, bool> = todo
            .iter()
            .map(|(f, _, d)| (d.clone(), forced_all || *f))
            .collect();
        let ignored = AtomicBool::new(false);
        let mut callbacks = remote_callbacks(report, &ignored, cred);
        callbacks.update_tips(|_, _, _| true);
        callbacks.push_update_reference(|refname, status| {
            if let Some(msg) = status
                && let Some(row) = rows
                    .lock()
                    .expect("rows mutex")
                    .iter_mut()
                    .find(|r| r.dst == refname)
            {
                row.flag = '!';
                row.summary = "[remote rejected]".to_owned();
                row.reason = Some(msg.to_owned());
            }
            Ok(())
        });
        callbacks.push_negotiation(|updates| {
            let mut out: Vec<PushRow> = updates
                .iter()
                .map(|u| {
                    let dst = u.dst_refname().unwrap_or_default();
                    push_row(
                        repo,
                        u,
                        forced.get(dst).copied().unwrap_or(forced_all),
                        leases.get(dst).copied(),
                    )
                })
                .collect();
            let rejected = out.iter().any(|r| r.flag == '!');
            if rejected && args.atomic {
                for r in out.iter_mut().filter(|r| !matches!(r.flag, '!' | '=')) {
                    r.flag = '!';
                    r.summary = "[rejected]".to_owned();
                    r.reason = Some("atomic push failed".to_owned());
                }
            }
            if rejected && !args.dry_run && !args.atomic {
                *rows.lock().expect("rows mutex") = out;
                retry.store(true, Relaxed);
                return Err(git2::Error::from_str("rejected"));
            }
            if !rejected && !args.no_verify {
                let zero = Oid::ZERO_SHA1.to_string();
                let lines: String = out
                    .iter()
                    .filter(|r| r.flag != '=')
                    .map(|r| {
                        let (local, new) = match r.flag {
                            '-' => ("(delete)", zero.clone()),
                            _ => (r.src.as_str(), r.new.to_string()),
                        };
                        format!("{local} {new} {} {}\n", r.dst, r.old)
                    })
                    .collect();
                let hook = hook_checked(
                    repo,
                    "pre-push",
                    &[remote_name, url],
                    Some(lines.as_bytes()),
                    &[],
                );
                if let Err(e) = hook {
                    *hook_failed.lock().expect("hook mutex") = Some(e.to_string());
                    stop.store(true, Relaxed);
                    return Err(git2::Error::from_str("pre-push hook failed"));
                }
            }
            *rows.lock().expect("rows mutex") = out;
            if args.dry_run || rejected {
                stop.store(true, Relaxed);
                return Err(git2::Error::from_str("not pushed"));
            }
            Ok(())
        });
        let mut opts = PushOptions::new();
        opts.remote_callbacks(callbacks);
        if !push_options.is_empty() {
            opts.remote_push_options(&push_options);
        }
        let wire: Vec<String> = todo
            .iter()
            .map(|(f, src, dst)| {
                let lead = if forced_all || *f { "+" } else { "" };
                format!("{lead}{src}:{dst}")
            })
            .collect();
        let pushed = remote.push(&wire, Some(&mut opts));
        if retry.load(Relaxed) {
            let out = std::mem::take(&mut *rows.lock().expect("rows mutex"));
            let (bad, _): (Vec<PushRow>, Vec<PushRow>) =
                out.into_iter().partition(|r| r.flag == '!');
            todo.retain(|(_, _, d)| !bad.iter().any(|r| r.dst == *d));
            failed.extend(bad);
            if todo.is_empty() {
                break;
            }
            continue;
        }
        if !stop.load(Relaxed) {
            pushed?;
        }
        break;
    }
    if let Some(msg) = hook_failed.into_inner().expect("hook mutex") {
        return Err(GitError::PushFailed(
            format!("{msg}\nerror: failed to push some refs to '{url}'")
                .trim_start()
                .to_owned(),
        ));
    }

    let mut all = failed;
    all.extend(rows.into_inner().expect("rows mutex"));
    all.retain(|r| !(r.summary == "[rejected]" && followed.contains(&r.dst)));
    let at = |d: &str| specs.iter().position(|(_, _, x)| x == d);
    all.sort_by_key(|r| at(&r.dst));
    let shown: Vec<String> = all
        .iter()
        .filter(|r| args.porcelain || args.verbose || r.flag != '=')
        .map(|r| r.line(args.porcelain))
        .collect();
    let mut text = Vec::new();
    if shown.is_empty() {
        text.push("Everything up-to-date".to_owned());
    } else {
        text.push(format!("To {url}"));
        text.extend(shown);
        if args.porcelain {
            text.push("Done".to_owned());
        }
    }
    if all.iter().any(|r| r.flag == '!') {
        text.push(format!("error: failed to push some refs to '{url}'"));
        text.extend(push_hints(repo, &all));
        return Err(GitError::PushFailed(text.join("\n")));
    }
    for line in text {
        report(OpProgress::Line(line));
    }
    if anonymous && !args.dry_run {
        track_pushed(repo, remote_name, &all)?;
    }
    if args.set_upstream && !args.dry_run {
        for (_, src, dst) in &todo {
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

/// Move `remote`'s remote-tracking refs to what `rows` pushed, as git does
/// after a push; libgit2 does it only through a named remote.
fn track_pushed(repo: &Repository, remote: &str, rows: &[PushRow]) -> Result<(), GitError> {
    let specs = config_values(repo, &format!("remote.{remote}.fetch"))?;
    for row in rows.iter().filter(|r| r.flag != '!') {
        let Some(local) = specs.iter().find_map(|s| {
            let (src, dst) = s.trim_start_matches('+').split_once(':')?;
            map_glob(src, dst, &row.dst)
        }) else {
            continue;
        };
        if row.flag != '-' {
            repo.reference(&local, row.new, true, "update by push")?;
        } else if let Ok(mut r) = repo.find_reference(&local) {
            r.delete()?;
        }
    }
    Ok(())
}

/// git's `push --recurse-submodules`: submodule commits that the pushed
/// history records and no remote-tracking ref of the submodule has are
/// refused (`check`) or pushed first (`on-demand`, `only`).
fn push_submodules(
    repo: &Repository,
    remote_name: &str,
    specs: &[(bool, String, String)],
    refspecs: &[String],
    args: &crate::PushArgs,
    mode: &str,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let tips: Vec<Oid> = specs
        .iter()
        .filter(|(_, src, _)| !src.is_empty())
        .filter_map(|(_, src, _)| Some(repo.rev_single(src).ok()?.peel_to_commit().ok()?.id()))
        .collect();
    let mut pending = unpushed_submodules(repo, remote_name, &tips)?;
    if mode != "check" {
        let workdir = repo.workdir().unwrap_or(repo.path());
        for path in &pending {
            report(OpProgress::Line(format!("Pushing submodule '{path}'")));
            let sub_args = crate::PushArgs {
                dry_run: args.dry_run,
                push_options: args.push_options.clone(),
                ..Default::default()
            };
            Git2Backend::discover(workdir.join(path))
                .and_then(|sub| sub.push_to(Some(remote_name), refspecs, &sub_args, report))
                .map_err(|e| {
                    GitError::Other(format!(
                        "{e}\nUnable to push submodule '{path}'\nfailed to push all needed submodules"
                    ))
                })?;
        }
        if mode == "only" || args.dry_run {
            return Ok(());
        }
        pending = unpushed_submodules(repo, remote_name, &tips)?;
    }
    if pending.is_empty() {
        return Ok(());
    }
    let paths: String = pending.iter().map(|p| format!("  {p}\n")).collect();
    Err(GitError::Other(format!(
        "The following submodule paths contain changes that can\n\
         not be found on any remote:\n{paths}\n\
         Please try\n\n\
         \tgit push --recurse-submodules=on-demand\n\n\
         or cd to the path and use\n\n\
         \tgit push\n\n\
         to push them to a remote.\n\n\
         Aborting."
    )))
}

/// The paths of submodules whose commits, recorded by the history from
/// `tips` not yet on `remote`, the submodule has but no remote-tracking ref
/// of it reaches (git's find_unpushed_submodules).
fn unpushed_submodules(
    repo: &Repository,
    remote: &str,
    tips: &[Oid],
) -> Result<Vec<String>, GitError> {
    let mut walk = repo.revwalk()?;
    for tip in tips {
        walk.push(*tip)?;
    }
    walk.hide_glob(&format!("refs/remotes/{remote}/*"))?;
    let mut recorded: std::collections::BTreeMap<String, Vec<Oid>> = Default::default();
    for id in walk {
        let commit = repo.find_commit(id?)?;
        let tree = commit.tree()?;
        let mut parents: Vec<Option<git2::Tree>> = Vec::new();
        for p in commit.parents() {
            parents.push(Some(p.tree()?));
        }
        if parents.is_empty() {
            parents.push(None);
        }
        for parent in &parents {
            let diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&tree), None)?;
            for delta in diff.deltas() {
                let file = delta.new_file();
                if file.mode() == git2::FileMode::Commit
                    && !file.id().is_zero()
                    && let Some(path) = file.path()
                {
                    let path = path.to_string_lossy().into_owned();
                    recorded.entry(path).or_default().push(file.id());
                }
            }
        }
    }
    let workdir = repo.workdir().unwrap_or(repo.path());
    let mut out = Vec::new();
    for (path, commits) in recorded {
        // A submodule without these commits cannot push them; git lets it be.
        let Ok(sub) = Repository::open(workdir.join(&path)) else {
            continue;
        };
        if commits.iter().any(|c| sub.find_commit(*c).is_err()) {
            continue;
        }
        let mut walk = sub.revwalk()?;
        for c in &commits {
            walk.push(*c)?;
        }
        walk.hide_glob("refs/remotes/*")?;
        if walk.next().is_some() {
            out.push(path);
        }
    }
    Ok(out)
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

/// One fetched ref update: source, destination, old and new id.
type FetchUpdate = (String, String, Option<Oid>, Oid);

/// Serializes the ref and FETCH_HEAD updates of fetches running at once.
static FETCH_UPDATE: Mutex<()> = Mutex::new(());

/// The remote to fetch `name` through, the URL it reads and whether it is
/// an anonymous stand-in: git fetches only from a remote's first URL, so a
/// remote with several is fetched through an anonymous remote for that one.
fn fetch_source<'r>(
    repo: &'r Repository,
    name: &str,
) -> Result<(git2::Remote<'r>, String, bool), GitError> {
    let remote = repo.find_remote(name)?;
    match remote_urls(repo, name, false)?.as_slice() {
        [first, _, ..] => Ok((repo.remote_anonymous(first)?, first.clone(), true)),
        _ => {
            let url = remote.url().unwrap_or_default().to_owned();
            Ok((remote, url, false))
        }
    }
}

fn do_fetch(
    repo: &Repository,
    name: &str,
    refspecs: &[String],
    args: &crate::FetchArgs,
    report: &dyn Fn(OpProgress),
    cred: Option<&dyn crate::CredentialPrompt>,
) -> Result<(), GitError> {
    let (mut remote, url, anonymous) = fetch_source(repo, name)?;
    let configured = config_values(repo, &format!("remote.{name}.fetch"))?;
    let mut specs: Vec<String> = refspecs
        .iter()
        .map(|s| match args.force && !s.starts_with('+') {
            true => format!("+{s}"),
            false => s.clone(),
        })
        .collect();
    if let Some(map) = &args.refmap {
        // An anonymous remote has no configured refspecs to update through.
        specs = specs.iter().map(|s| refmap_spec(s, map)).collect();
        if !anonymous {
            remote = repo.remote_anonymous(&url)?;
        }
    } else if anonymous {
        specs = match specs.is_empty() {
            true => configured.clone(),
            false => specs.iter().map(|s| refmap_spec(s, &configured)).collect(),
        };
    }
    if args.prune_tags {
        if specs.is_empty() {
            specs = configured.clone();
        }
        specs.push("refs/tags/*:refs/tags/*".to_owned());
    }
    let tracking = tracking_specs(&remote, &specs)?;
    let updates: Mutex<Vec<FetchUpdate>> = Mutex::new(Vec::new());
    let on_tip = |dst: &str, old: Oid, new: Oid| {
        let src = tracking
            .iter()
            .find_map(|(s, d)| map_glob(d, s, dst))
            .unwrap_or_else(|| dst.to_owned());
        updates
            .lock()
            .expect("updates mutex")
            .push((src, dst.to_owned(), Some(old), new));
        true
    };
    let tag_opt = repo
        .config()?
        .get_string(&format!("remote.{name}.tagOpt"))
        .unwrap_or_default();
    let tags = if args.no_tags || (anonymous && tag_opt == "--no-tags") {
        git2::AutotagOption::None
    } else if args.tags || (anonymous && tag_opt == "--tags") {
        git2::AutotagOption::All
    } else if !refspecs.is_empty() {
        // git follows no tags when the refs to fetch are named.
        git2::AutotagOption::None
    } else {
        git2::AutotagOption::Unspecified
    };
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(remote_callbacks(report, &ignored, cred));
    opts.download_tags(tags);
    if args.unshallow {
        // libgit2's GIT_FETCH_DEPTH_UNSHALLOW.
        opts.depth(i32::MAX);
    } else if args.depth > 0 {
        opts.depth(args.depth);
    }
    // git_remote_fetch's steps, with the ref updates one fetch at a time so
    // fetches running at once can share FETCH_HEAD.
    remote.download(&specs, Some(&mut opts))?;
    let their_head = remote
        .default_branch()
        .ok()
        .and_then(|b| b.as_str().ok().map(str::to_owned));
    remote.disconnect()?;
    let fetch_head = repo.path().join("FETCH_HEAD");
    let fetched = {
        let _serial = FETCH_UPDATE.lock().unwrap_or_else(|e| e.into_inner());
        let before = match args.append {
            true => std::fs::read_to_string(&fetch_head).unwrap_or_default(),
            false => String::new(),
        };
        let mut callbacks = RemoteCallbacks::new();
        callbacks.update_tips(|d, o, n| on_tip(d, o, n));
        let msg = format!("fetch {}", if anonymous { &url } else { name });
        remote.update_tips(
            Some(&mut callbacks),
            git2::RemoteUpdateFlags::UPDATE_FETCHHEAD,
            tags,
            Some(&msg),
        )?;
        if args.prune {
            let mut callbacks = RemoteCallbacks::new();
            callbacks.update_tips(|d, o, n| on_tip(d, o, n));
            remote.prune(Some(callbacks))?;
        }
        // git names the URL in FETCH_HEAD as it shows it after `From`.
        let shown = crate::fetch_display::display_url(&url);
        let written: String = std::fs::read_to_string(&fetch_head)
            .unwrap_or_default()
            .lines()
            .map(|l| match l.strip_suffix(url.as_str()) {
                Some(head) => format!("{head}{shown}\n"),
                None => format!("{l}\n"),
            })
            .collect();
        std::fs::write(&fetch_head, format!("{before}{written}"))?;
        fetch_head_refs(&written)
    };
    if refspecs.is_empty()
        && args.refmap.is_none()
        && let Some(head) = their_head
    {
        follow_remote_head(repo, name, &configured, &head)?;
    }
    let updates = updates.into_inner().expect("updates mutex");
    let rows = fetch_rows(repo, name, refspecs, &updates, &fetched, true);
    for line in crate::fetch_display::render(&url, &rows, compact_fetch(repo)) {
        report(OpProgress::Line(line));
    }
    Ok(())
}

/// Fetch `name`, making its history as shallow as `args` asks. From a local
/// repository the objects are copied here, just those of the history the
/// fetch takes (libgit2's local transport sends every object, and no shallow
/// history); over the network rgit's own protocol client fetches what
/// libgit2 cannot ask for: a filter, a date or a deepening.
fn fetch_one(
    repo: &Repository,
    name: &str,
    refspecs: &[String],
    args: &crate::FetchArgs,
    report: &dyn Fn(OpProgress),
    cred: Option<&dyn crate::CredentialPrompt>,
) -> Result<(), GitError> {
    let since = match &args.shallow_since {
        Some(s) => Some(
            crate::config::expiry_date(s)
                .ok_or_else(|| GitError::Other(format!("invalid date: {s}")))?,
        ),
        None => None,
    };
    let plain = |depth: i32| crate::FetchArgs {
        depth,
        deepen: 0,
        shallow_since: None,
        unshallow: false,
        ..args.clone()
    };
    let url = repo.find_remote(name)?.url().unwrap_or_default().to_owned();
    // A promisor remote is fetched through its own filter, as in git.
    let given = match &args.filter {
        Some(s) => Some(crate::promisor::Filter::parse(s)?.spec()),
        None => None,
    };
    if let Some(spec) = &given {
        crate::promisor::mark(repo, name, spec)?;
    }
    let spec = given.or_else(|| crate::promisor::remote_filter(repo, name));
    if is_local_url(&url) {
        let cut = match since {
            _ if args.unshallow => Some(shallow::Cut::Full),
            Some(t) => Some(shallow::Cut::Since(t)),
            None if args.deepen > 0 => Some(shallow::Cut::Deepen(args.deepen as usize)),
            None if args.depth > 0 => Some(shallow::Cut::Depth(args.depth as usize)),
            None => None,
        };
        let src = Repository::open(local_path(repo, &url))?;
        copy_history(
            repo,
            &src,
            name,
            refspecs,
            args,
            cut,
            spec.as_deref(),
            report,
        )?;
        return do_fetch(repo, name, refspecs, &plain(0), report, cred);
    }
    // libgit2 can neither filter nor cut history at a date or deepen it.
    if spec.is_some() || since.is_some() || args.deepen > 0 {
        fetch_smart(
            repo,
            name,
            &url,
            refspecs,
            args,
            since,
            spec.as_deref(),
            report,
        )?;
        return do_fetch(repo, name, refspecs, &plain(0), report, cred);
    }
    let args = crate::FetchArgs {
        unshallow: args.unshallow,
        ..plain(args.depth)
    };
    do_fetch(repo, name, refspecs, &args, report, cred)
}

/// Copy from the local repository `src` the history of the refs a fetch of
/// `name` takes, as far back as `cut` says (else down to what is here), with
/// the annotated tags on it, and mark where it now stops shallow. With a
/// filter `spec` the objects go into a promisor pack, without what the
/// filter leaves out when `src` allows filtering.
#[allow(clippy::too_many_arguments)]
fn copy_history(
    repo: &Repository,
    src: &Repository,
    name: &str,
    refspecs: &[String],
    args: &crate::FetchArgs,
    cut: Option<shallow::Cut>,
    spec: Option<&str>,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let remote = repo.find_remote(name)?;
    let configured = remote.fetch_refspecs()?;
    let patterns: Vec<&str> = match refspecs.is_empty() {
        true => configured.iter().flatten().flatten().collect(),
        false => refspecs.iter().map(String::as_str).collect(),
    };
    let patterns: Vec<&str> = patterns
        .iter()
        .map(|s| s.trim_start_matches('+').split(':').next().unwrap_or(""))
        .collect();
    let all_tags = args.tags || args.prune_tags;
    let (mut tips, mut tags) = (Vec::new(), Vec::new());
    let mut wanted = String::new();
    if patterns.contains(&"HEAD") {
        tips.extend(
            src.head()
                .ok()
                .and_then(|h| h.peel_to_commit().ok())
                .map(|c| c.id()),
        );
    }
    for r in src.references()? {
        let r = r?;
        let (Ok(refname), Some(target)) = (r.name(), r.target()) else {
            continue;
        };
        let peeled = r.peel_to_commit().ok().map(|c| c.id());
        let tag = refname.starts_with("refs/tags/");
        if tag {
            tags.push((target, peeled));
        }
        if (tag && all_tags) || patterns.iter().any(|p| ref_matches(p, refname)) {
            tips.extend(peeled);
            wanted.push_str(&format!("{target} {refname}\n"));
        }
    }
    let odb = repo.odb()?;
    let old: std::collections::HashSet<Oid> = shallow::roots(repo).into_iter().collect();
    // A fetch that does not change the depth leaves the boundary alone.
    let boundary = match cut {
        Some(_) => old.clone(),
        None => Default::default(),
    };
    let cut = cut.unwrap_or(shallow::Cut::Full);
    let (kept, cutoff) = shallow::select(src, &tips, &cut, &boundary, Some(&odb))?;
    let followed: Vec<Oid> = tags
        .iter()
        .filter(|(_, peeled)| peeled.is_some_and(|p| kept.contains(&p) || odb.exists(p)))
        .map(|(t, _)| *t)
        .collect();
    match spec {
        Some(spec) => {
            let filter = match src.config()?.get_bool("uploadpack.allowFilter") {
                Ok(true) => Some(crate::promisor::Filter::parse(spec)?),
                _ => {
                    report(OpProgress::Line(
                        "warning: filtering not recognized by server, ignoring".to_owned(),
                    ));
                    None
                }
            };
            let set: std::collections::HashSet<Oid> = kept.iter().copied().collect();
            let pack = crate::promisor::local_pack(src, &set, &followed, filter, &odb)?;
            crate::promisor::store(repo, Some(&wanted), |w| Ok(w.write_all(&pack)?))?;
        }
        None => shallow::copy(src, repo, &kept, &followed)?,
    }
    let mut roots = shallow::incomplete(repo, &old);
    roots.extend(cutoff);
    shallow::write_roots(repo, &roots)
}

/// Fetch the objects of the refs a fetch of `name` takes from `url` through
/// rgit's own protocol client, for what libgit2 cannot ask a server for: a
/// filter `spec` (the pack then kept as a promisor pack) and a history cut
/// at a depth or date. The refs are left for the fetch to update.
#[allow(clippy::too_many_arguments)]
fn fetch_smart(
    repo: &Repository,
    name: &str,
    url: &str,
    refspecs: &[String],
    args: &crate::FetchArgs,
    since: Option<i64>,
    spec: Option<&str>,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    use std::io::Write;
    let config = repo.config()?;
    let configured = config_values(repo, &format!("remote.{name}.fetch"))?;
    let patterns: Vec<&str> = match refspecs.is_empty() {
        true => configured.iter().map(String::as_str).collect(),
        false => refspecs.iter().map(String::as_str).collect(),
    };
    let patterns: Vec<&str> = patterns
        .iter()
        .map(|s| s.trim_start_matches('+').split(':').next().unwrap_or(""))
        .collect();
    let tag_opt = config
        .get_string(&format!("remote.{name}.tagOpt"))
        .unwrap_or_default();
    let all_tags = args.tags || args.prune_tags || (tag_opt == "--tags" && !args.no_tags);
    let mut session = crate::smart::Session::open(url, Some(&config))?;
    let odb = repo.odb()?;
    let (mut wants, mut wanted) = (Vec::new(), String::new());
    for r in session.ls_refs(&[])? {
        let tag = r.name.starts_with("refs/tags/");
        if r.id.is_zero()
            || !((tag && all_tags) || patterns.iter().any(|p| ref_matches(p, &r.name)))
        {
            continue;
        }
        wanted.push_str(&format!("{} {}\n", r.id, r.name));
        let deepen = args.unshallow || args.deepen > 0 || args.depth > 0 || since.is_some();
        if (deepen || !odb.exists(r.id)) && !wants.contains(&r.id) {
            wants.push(r.id);
        }
    }
    if wants.is_empty() {
        return session.finish();
    }
    let haves = repo
        .references()?
        .flatten()
        .filter_map(|r| r.peel_to_commit().ok().map(|c| c.id()))
        .collect::<std::collections::BTreeSet<_>>();
    let filter = match spec {
        Some(_) if !session.filters() => {
            report(OpProgress::Line(
                "warning: filtering not recognized by server, ignoring".to_owned(),
            ));
            None
        }
        s => s.map(str::to_owned),
    };
    let old: std::collections::HashSet<Oid> = shallow::roots(repo).into_iter().collect();
    let req = crate::smart::FetchRequest {
        wants,
        haves: haves.into_iter().collect(),
        shallow: old.iter().copied().collect(),
        depth: match () {
            _ if args.unshallow => i32::MAX,
            _ if args.deepen > 0 => args.deepen,
            _ => args.depth,
        },
        deepen_since: since,
        deepen_relative: args.deepen > 0,
        filter,
        include_tag: !args.no_tags,
    };
    let got = crate::promisor::store(repo, spec.map(|_| wanted.as_str()), |w: &mut dyn Write| {
        session.fetch(&req, w, report)
    })?;
    session.finish()?;
    let mut roots = old;
    roots.extend(got.shallow);
    for id in got.unshallow {
        roots.remove(&id);
    }
    shallow::write_roots(repo, &roots)
}

/// Whether the refspec source `pattern` (a glob, full name or short name)
/// names the ref `name`.
fn ref_matches(pattern: &str, name: &str) -> bool {
    if pattern.contains('*') {
        return map_glob(pattern, pattern, name).is_some();
    }
    name == pattern
        || ["refs/", "refs/heads/", "refs/tags/"]
            .iter()
            .any(|p| name.strip_prefix(p) == Some(pattern))
}

/// The folder a local remote URL names; a relative one is taken from the
/// working tree, where git runs.
fn local_path(repo: &Repository, url: &str) -> PathBuf {
    let path = Path::new(url.strip_prefix("file://").unwrap_or(url));
    match path.is_relative() {
        true => repo.workdir().unwrap_or(repo.path()).join(path),
        false => path.to_path_buf(),
    }
}

/// Point `refs/remotes/<name>/HEAD` at the remote's HEAD branch after a
/// fetch, as `remote.<name>.followRemoteHEAD` says (git 2.48's default,
/// `create`, sets it only when missing).
fn follow_remote_head(
    repo: &Repository,
    name: &str,
    specs: &[String],
    head: &str,
) -> Result<(), GitError> {
    let mode = repo
        .config()?
        .get_string(&format!("remote.{name}.followRemoteHEAD"))
        .unwrap_or_default();
    if mode == "never" {
        return Ok(());
    }
    let link = format!("refs/remotes/{name}/HEAD");
    let Some(target) = specs.iter().find_map(|s| {
        let (src, dst) = s.trim_start_matches('+').split_once(':')?;
        map_glob(src, dst, head)
    }) else {
        return Ok(());
    };
    let current = repo
        .find_reference(&link)
        .ok()
        .map(|r| r.symbolic_target().ok().flatten().map(str::to_owned));
    let wanted = match current {
        None => true,
        Some(t) => mode == "always" && t.as_deref() != Some(target.as_str()),
    };
    let tracking = target.starts_with(&format!("refs/remotes/{name}/"));
    if wanted && tracking && link != target && repo.find_reference(&target).is_ok() {
        repo.reference_symbolic(&link, &target, true, "remote set-head")?;
    }
    Ok(())
}

/// The refs a FETCH_HEAD file lists, as full names.
fn fetch_head_refs(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let desc = l.splitn(3, '\t').nth(2)?;
            let desc = desc.rsplit_once(" of ").map_or(desc, |(d, _)| d);
            let quoted = |d: &str| Some(d.strip_prefix('\'')?.strip_suffix('\'')?.to_owned());
            Some(if let Some(b) = desc.strip_prefix("branch ") {
                format!("refs/heads/{}", quoted(b)?)
            } else if let Some(t) = desc.strip_prefix("tag ") {
                format!("refs/tags/{}", quoted(t)?)
            } else {
                quoted(desc).unwrap_or_else(|| "HEAD".to_owned())
            })
        })
        .collect()
}

/// Whether `fetch.output` asks for git's compact report.
fn compact_fetch(repo: &Repository) -> bool {
    repo.config()
        .and_then(|c| c.get_string("fetch.output"))
        .is_ok_and(|v| v.eq_ignore_ascii_case("compact"))
}

/// git's report rows for a fetch from `remote`, in git's order: pruned refs,
/// then the refs to merge, the other refs, and last the remote-tracking
/// refs updated along the way. `fetched` names the remote refs fetched, for
/// the `-> FETCH_HEAD` lines of refspecs with no destination.
fn fetch_rows(
    repo: &Repository,
    remote: &str,
    cli: &[String],
    updates: &[FetchUpdate],
    fetched: &[String],
    done: bool,
) -> Vec<crate::fetch_display::Row> {
    let cli: Vec<(&str, Option<&str>)> = cli
        .iter()
        .map(|s| {
            let s = s.trim_start_matches('+');
            match s.split_once(':') {
                Some((src, dst)) if !dst.is_empty() => (src, Some(dst)),
                Some((src, _)) => (src, None),
                None => (s, None),
            }
        })
        .collect();
    let named = |pat: &str, name: &str| {
        name == pat
            || short_ref(name) == pat
            || name.strip_prefix("refs/") == Some(pat)
            || (pat.contains('*') && map_glob(pat, pat, name).is_some())
    };
    let merge = current_branch(repo).ok().and_then(|b| {
        let key = format!("refs/heads/{b}");
        let theirs = repo.branch_upstream_remote(&key).ok()?;
        (theirs.as_str().ok()? == remote).then_some(())?;
        repo.branch_upstream_merge(&key)
            .ok()?
            .as_str()
            .ok()
            .map(str::to_owned)
    });
    let mut keyed = Vec::new();
    for (src, dst, old, new) in updates {
        let Some(row) = fetch_line(repo, src, dst, *old, *new, done) else {
            continue;
        };
        let key = if new.is_zero() {
            (0, 0)
        } else if cli.is_empty() {
            (if merge.as_deref() == Some(src) { 1 } else { 2 }, 0)
        } else {
            match cli.iter().position(|(s, _)| named(s, src)) {
                Some(i) if cli[i].1.is_some() => (1, i),
                None if dst.starts_with("refs/tags/") => (2, 0),
                _ => (3, 0),
            }
        };
        keyed.push((key, row));
    }
    for name in fetched {
        let Some(i) = cli.iter().position(|(s, d)| d.is_none() && named(s, name)) else {
            continue;
        };
        let (kind, what) = if let Some(b) = name.strip_prefix("refs/heads/") {
            ("branch", b)
        } else if let Some(t) = name.strip_prefix("refs/tags/") {
            ("tag", t)
        } else if let Some(r) = name.strip_prefix("refs/remotes/") {
            ("remote-tracking branch", r)
        } else {
            ("branch", name.as_str())
        };
        keyed.push((
            (1, i),
            crate::fetch_display::Row {
                code: '*',
                summary: kind.to_owned(),
                remote: what.to_owned(),
                local: "FETCH_HEAD".to_owned(),
                error: None,
                counted: false,
            },
        ));
    }
    keyed.sort_by_key(|(k, _)| *k);
    keyed.into_iter().map(|(_, r)| r).collect()
}

/// The (source, destination) patterns that name remote-tracking refs: the
/// `src:dst` refspecs given, then the remote's configured ones.
fn tracking_specs(
    remote: &git2::Remote<'_>,
    specs: &[String],
) -> Result<Vec<(String, String)>, GitError> {
    let configured = remote.fetch_refspecs()?;
    Ok(specs
        .iter()
        .map(String::as_str)
        .chain(configured.iter().flatten().flatten())
        .filter_map(|s| {
            let (src, dst) = s.trim_start_matches('+').split_once(':')?;
            Some((src.to_owned(), dst.to_owned()))
        })
        .filter(|(_, d)| !d.is_empty())
        .collect())
}

/// `name` matched against the refspec side `from` (one `*` at most) and
/// rewritten through `to`.
fn map_glob(from: &str, to: &str, name: &str) -> Option<String> {
    match from.split_once('*') {
        None => (from == name).then(|| to.to_owned()),
        Some((pre, post)) => {
            let mid = name.strip_prefix(pre)?.strip_suffix(post)?;
            Some(to.replacen('*', mid, 1))
        }
    }
}

/// A command-line fetch refspec as `--refmap` maps it: a bare ref gets the
/// destination the first matching refmap entry gives it.
fn refmap_spec(spec: &str, map: &[String]) -> String {
    if spec.contains(':') {
        return spec.to_owned();
    }
    let (force, name) = match spec.strip_prefix('+') {
        Some(n) => ("+", n),
        None => ("", spec),
    };
    let full = match name.starts_with("refs/") {
        true => name.to_owned(),
        false => format!("refs/heads/{name}"),
    };
    map.iter()
        .find_map(|m| {
            let plus = if m.starts_with('+') { "+" } else { force };
            let (src, dst) = m.trim_start_matches('+').split_once(':')?;
            Some(format!("{plus}{full}:{}", map_glob(src, dst, &full)?))
        })
        .unwrap_or(full)
}

/// git's report row for `dst` moving from `old` to `new`; `None` when
/// nothing changes. `done` says the update happened (else a dry run).
fn fetch_line(
    repo: &Repository,
    src: &str,
    dst: &str,
    old: Option<Oid>,
    new: Oid,
    done: bool,
) -> Option<crate::fetch_display::Row> {
    let row = |code, summary: &str, error: Option<&str>| {
        Some(crate::fetch_display::Row {
            code,
            summary: summary.to_owned(),
            remote: short_ref(src).to_owned(),
            local: short_ref(dst).to_owned(),
            error: error.map(str::to_owned),
            counted: true,
        })
    };
    let tag = dst.starts_with("refs/tags/");
    if new.is_zero() {
        return Some(crate::fetch_display::Row {
            code: '-',
            summary: "[deleted]".to_owned(),
            remote: "(none)".to_owned(),
            local: short_ref(dst).to_owned(),
            error: None,
            counted: false,
        });
    }
    match old.filter(|o| !o.is_zero()) {
        Some(old) if old == new => None,
        None if src.starts_with("refs/tags/") || (tag && src == dst) => row('*', "[new tag]", None),
        None if src.starts_with("refs/heads/") || !src.starts_with("refs/") => {
            row('*', "[new branch]", None)
        }
        None => row('*', "[new ref]", None),
        Some(_) if tag && done => row('t', "[tag update]", None),
        Some(_) if tag => row('!', "[rejected]", Some("would clobber existing tag")),
        Some(old) if repo.graph_descendant_of(new, old).unwrap_or(false) => {
            row(' ', &format!("{}..{}", short7(old), short7(new)), None)
        }
        Some(old) => row(
            '+',
            &format!("{}...{}", short7(old), short7(new)),
            Some("forced update"),
        ),
    }
}

/// `git fetch --dry-run`: download the objects as git does, then report the
/// ref updates a fetch would make, including auto-followed tags, without
/// writing any ref.
fn fetch_dry_run(
    repo: &Repository,
    name: &str,
    refspecs: &[String],
    args: &crate::FetchArgs,
    report: &dyn Fn(OpProgress),
    cred: Option<&dyn crate::CredentialPrompt>,
) -> Result<(), GitError> {
    let (mut remote, url, anonymous) = fetch_source(repo, name)?;
    let mut tracking = tracking_specs(&remote, refspecs)?;
    if anonymous {
        let configured = config_values(repo, &format!("remote.{name}.fetch"))?;
        tracking.extend(configured.iter().filter_map(|s| {
            let (src, dst) = s.trim_start_matches('+').split_once(':')?;
            Some((src.to_owned(), dst.to_owned()))
        }));
    }
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let heads: Vec<(String, Oid)> = {
        let callbacks = remote_callbacks(report, &ignored, cred);
        let mut conn = remote.connect_auth(git2::Direction::Fetch, Some(callbacks), None)?;
        let heads = conn
            .list()?
            .iter()
            .map(|h| (h.name().to_owned(), h.oid()))
            .collect();
        let mut opts = FetchOptions::new();
        opts.remote_callbacks(remote_callbacks(report, &ignored, cred));
        conn.remote().download(refspecs, Some(&mut opts))?;
        heads
    };
    let wanted = |head: &str| {
        refspecs.is_empty()
            || refspecs.iter().any(|s| {
                let src = s.trim_start_matches('+').split(':').next().unwrap_or("");
                head == src || short_ref(head) == src
            })
    };
    // A tag is followed when what it points at is now here.
    let followed = |head: &str, id: Oid| {
        let peeled = heads
            .iter()
            .find(|(h, _)| *h == format!("{head}^{{}}"))
            .map_or(id, |(_, o)| *o);
        repo.find_commit(peeled).is_ok()
    };
    let all_tags = args.tags || args.prune_tags;
    let mut updates: Vec<FetchUpdate> = Vec::new();
    let mut backfill: Vec<FetchUpdate> = Vec::new();
    let mut fetched = Vec::new();
    for (head, new) in &heads {
        if head.ends_with("^{}") {
            continue;
        }
        let tag = head.starts_with("refs/tags/");
        let dst = if tag
            && (all_tags || (!args.no_tags && refspecs.is_empty() && followed(head, *new)))
        {
            head.clone()
        } else if !wanted(head) {
            continue;
        } else {
            fetched.push(head.clone());
            match tracking.iter().find_map(|(s, d)| map_glob(s, d, head)) {
                Some(dst) => dst,
                None => continue,
            }
        };
        let old = repo.refname_to_id(&dst).ok();
        // git's dry run lists a followed tag again as it backfills tags.
        if tag && !all_tags && refspecs.is_empty() {
            backfill.push((head.clone(), dst.clone(), old, *new));
        }
        updates.push((head.clone(), dst, old, *new));
    }
    if args.prune {
        for (src, dst) in &tracking {
            let Some((pre, _)) = dst.split_once('*') else {
                continue;
            };
            for r in repo.references_glob(&format!("{pre}*"))? {
                let r = r?;
                if r.symbolic_target().is_ok_and(|t| t.is_some()) {
                    continue;
                }
                if let Ok(local) = r.name()
                    && let Some(theirs) = map_glob(dst, src, local)
                    && !heads.iter().any(|(h, _)| *h == theirs)
                {
                    updates.push((theirs, local.to_owned(), r.target(), Oid::ZERO_SHA1));
                }
            }
        }
    }
    let mut rows = fetch_rows(repo, name, refspecs, &updates, &fetched, args.force);
    rows.extend(fetch_rows(repo, name, refspecs, &backfill, &[], args.force));
    for line in crate::fetch_display::render(&url, &rows, compact_fetch(repo)) {
        report(OpProgress::Line(line));
    }
    Ok(())
}

/// One ref of a push, as git reports it: `flag` is git's status column
/// (` ` fast-forward, `+` forced, `-` deleted, `*` new, `=` up to date, `!` rejected).
struct PushRow {
    flag: char,
    summary: String,
    src: String,
    dst: String,
    old: Oid,
    new: Oid,
    reason: Option<String>,
}

impl PushRow {
    fn line(&self, porcelain: bool) -> String {
        let reason = self
            .reason
            .as_ref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        if porcelain {
            return format!(
                "{}\t{}:{}\t{}{reason}",
                self.flag, self.src, self.dst, self.summary
            );
        }
        if self.flag == '-' {
            return format!(" - {:<17} {}", self.summary, short_ref(&self.dst));
        }
        format!(
            " {} {:<17} {} -> {}{reason}",
            self.flag,
            self.summary,
            short_ref(&self.src),
            short_ref(&self.dst)
        )
    }
}

/// Classify one negotiated ref update the way git does before sending.
fn push_row(
    repo: &Repository,
    update: &git2::PushUpdate<'_>,
    force: bool,
    lease: Option<Oid>,
) -> PushRow {
    let (old, new) = (update.src(), update.dst());
    let dst = update.dst_refname().unwrap_or_default().to_owned();
    let tag = dst.starts_with("refs/tags/");
    let (flag, summary, reason) = if lease.is_some_and(|l| l != old) {
        ('!', "[rejected]".to_owned(), Some("stale info"))
    } else if new.is_zero() {
        ('-', "[deleted]".to_owned(), None)
    } else if old == new {
        ('=', "[up to date]".to_owned(), None)
    } else if old.is_zero() {
        let kind = match dst.split('/').nth(1) {
            Some("tags") => "[new tag]",
            Some("heads") => "[new branch]",
            _ => "[new reference]",
        };
        ('*', kind.to_owned(), None)
    } else {
        let have = repo.odb().is_ok_and(|db| db.exists(old));
        if have && !tag && repo.graph_descendant_of(new, old).unwrap_or(false) {
            (' ', format!("{}..{}", short7(old), short7(new)), None)
        } else if force {
            (
                '+',
                format!("{}...{}", short7(old), short7(new)),
                Some("forced update"),
            )
        } else if tag {
            ('!', "[rejected]".to_owned(), Some("already exists"))
        } else if !have {
            ('!', "[rejected]".to_owned(), Some("fetch first"))
        } else {
            ('!', "[rejected]".to_owned(), Some("non-fast-forward"))
        }
    };
    PushRow {
        flag,
        summary,
        src: update.src_refname().unwrap_or_default().to_owned(),
        dst,
        old,
        new,
        reason: reason.map(str::to_owned),
    }
}

/// git's advice for the first kind of rejection in `rows`, in git's order.
fn push_hints(repo: &Repository, rows: &[PushRow]) -> Vec<String> {
    let head = current_branch(repo).map(|b| format!("refs/heads/{b}")).ok();
    let why = |reason: &str, on_head: Option<bool>| {
        rows.iter().any(|r| {
            r.summary == "[rejected]"
                && r.reason.as_deref() == Some(reason)
                && on_head.is_none_or(|h| h == (head.as_deref() == Some(r.dst.as_str())))
        })
    };
    let text = if why("non-fast-forward", Some(true)) {
        "Updates were rejected because the tip of your current branch is behind\n\
         its remote counterpart. If you want to integrate the remote changes,\n\
         use 'git pull' before pushing again.\n\
         See the 'Note about fast-forwards' in 'git push --help' for details."
    } else if why("non-fast-forward", None) {
        "Updates were rejected because a pushed branch tip is behind its remote\n\
         counterpart. If you want to integrate the remote changes, use 'git pull'\n\
         before pushing again.\n\
         See the 'Note about fast-forwards' in 'git push --help' for details."
    } else if why("already exists", None) {
        "Updates were rejected because the tag already exists in the remote."
    } else if why("fetch first", None) {
        "Updates were rejected because the remote contains work that you do not\n\
         have locally. This is usually caused by another repository pushing to\n\
         the same ref. If you want to integrate the remote changes, use\n\
         'git pull' before pushing again.\n\
         See the 'Note about fast-forwards' in 'git push --help' for details."
    } else {
        return Vec::new();
    };
    text.lines().map(|l| format!("hint: {l}")).collect()
}

/// What a bare `push` sends, per `push.default` (`simple` when unset).
fn default_push_specs(
    repo: &Repository,
    remote: &str,
) -> Result<Vec<(bool, String, String)>, GitError> {
    let branch = current_branch(repo)?;
    let head = format!("refs/heads/{branch}");
    let upstream = repo
        .branch_upstream_remote(&head)
        .ok()
        .filter(|r| r.as_str().ok() == Some(remote))
        .and_then(|_| repo.branch_upstream_merge(&head).ok())
        .and_then(|m| m.as_str().ok().map(str::to_owned));
    let mode = repo
        .config()?
        .get_string("push.default")
        .unwrap_or_else(|_| "simple".to_owned());
    Ok(match mode.as_str() {
        "nothing" => {
            return Err(GitError::Other(
                "You didn't specify any refspecs to push, and push.default is \"nothing\"."
                    .to_owned(),
            ));
        }
        "current" => vec![(false, head.clone(), head)],
        "upstream" | "tracking" => match upstream {
            Some(up) => vec![(false, head, up)],
            None => {
                return Err(GitError::Other(format!(
                    "The current branch {branch} has no upstream branch."
                )));
            }
        },
        "matching" => {
            let mut specs = Vec::new();
            for b in repo.branches(Some(BranchType::Local))? {
                let b = b?.0;
                if let Ok(name) = b.get().name()
                    && repo
                        .refname_to_id(&format!("refs/remotes/{remote}/{}", short_ref(name)))
                        .is_ok()
                {
                    specs.push((false, name.to_owned(), name.to_owned()));
                }
            }
            specs
        }
        _ => match upstream {
            Some(up) if up != head => {
                return Err(GitError::Other(format!(
                    "The upstream branch of your current branch does not match the name of \
                     your current branch. To push to the upstream branch on the remote, use \
                     `rgit push {remote} HEAD:{}`; to push to the branch of the same name, use \
                     `rgit push {remote} HEAD`",
                    short_ref(&up)
                )));
            }
            _ => vec![(false, head.clone(), head)],
        },
    })
}

/// Expand a git push refspec (`branch`, `src:dst`, `:dst`, `+src:dst`) to
/// (force, full source ref, full destination ref); an empty source deletes.
fn expand_push_refspec(
    repo: &Repository,
    remote: &str,
    spec: &str,
) -> Result<(bool, String, String), GitError> {
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
    } else if let Some(name) = repo
        .resolve_reference_from_short_name(src)
        .ok()
        .and_then(|r| r.name().ok().map(str::to_owned))
    {
        name
    } else if dst.is_some_and(|d| !d.is_empty()) && repo.rev_single(src).is_ok() {
        src.to_owned()
    } else {
        return Err(GitError::Other(format!(
            "src refspec {src} does not match any"
        )));
    };
    let exists = |name: String| repo.find_reference(&name).is_ok();
    let dst = match dst {
        None | Some("") if src.is_empty() => {
            return Err(GitError::Other(format!("invalid refspec '{spec}'")));
        }
        None | Some("") => src.clone(),
        Some(d) if d.starts_with("refs/") => d.to_owned(),
        // `:name` deletes a tag when name is a tag here and no branch.
        Some(d)
            if src.is_empty()
                && exists(format!("refs/tags/{d}"))
                && !exists(format!("refs/heads/{d}"))
                && !exists(format!("refs/remotes/{remote}/{d}")) =>
        {
            format!("refs/tags/{d}")
        }
        Some(d) if src.starts_with("refs/tags/") => format!("refs/tags/{d}"),
        Some(d) => format!("refs/heads/{d}"),
    };
    Ok((force, src, dst))
}

/// A ref name without its `refs/heads/`, `refs/tags/` or `refs/remotes/` prefix.
pub(crate) fn short_ref(name: &str) -> &str {
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
    let head = repo.head()?.peel_to_commit()?;
    if !opts.allow_unrelated
        && let Err(e) = repo.merge_base(head.id(), source.id())
    {
        return Err(if e.code() == ErrorCode::NotFound {
            GitError::Other("refusing to merge unrelated histories".into())
        } else {
            e.into()
        });
    }
    let ours = opts.strategy.as_deref() == Some("ours");
    if analysis.is_fast_forward() && !opts.no_ff && !opts.squash && !ours {
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
        safe_checkout(repo, source.as_object(), "merge")?;
        let name = repo.head()?.name().unwrap_or("HEAD").to_owned();
        repo.reference(&name, source.id(), true, "merge: fast-forward")?;
        repo.set_head(&name)?;
        report(OpProgress::Line("Fast-forward".to_owned()));
        return Ok(());
    }

    // `-s ours` records the merge but keeps HEAD's tree as it is.
    if ours {
        std::fs::write(repo.path().join("MERGE_HEAD"), format!("{}\n", source.id()))?;
    } else {
        let mut merge_opts = git2::MergeOptions::new();
        if let Some(side) = &opts.strategy_option {
            merge_opts.file_favor(file_favor(side)?);
        }
        // git labels their side of a conflict with the name merged.
        let hits = std::cell::RefCell::new(Vec::new());
        let mut co = CheckoutBuilder::new();
        co.safe();
        co.their_label(name);
        if let Err(e) = repo.merge(
            &[&annotated],
            Some(&mut merge_opts),
            Some(overwrite_notes(&mut co, &hits)),
        ) {
            let mut hits = hits.take();
            if hits.is_empty() {
                // libgit2 refuses a dirty index or tree before its checkout
                // runs: name the local changes the merge touches.
                let paths = |d: git2::Diff| -> Vec<String> {
                    d.deltas()
                        .filter_map(|d| Some(d.new_file().path()?.to_string_lossy().into_owned()))
                        .collect()
                };
                let theirs = paths(repo.diff_tree_to_tree(
                    Some(&head.tree()?),
                    Some(&source.tree()?),
                    None,
                )?);
                let mut dirty = paths(repo.diff_tree_to_index(Some(&head.tree()?), None, None)?);
                dirty.extend(paths(repo.diff_index_to_workdir(None, None)?));
                hits = theirs.into_iter().filter(|p| dirty.contains(p)).collect();
            }
            return Err(match overwritten(repo, "merge", &hits, e) {
                GitError::Conflict(m) => {
                    GitError::Conflict(format!("{m}\nMerge with strategy ort failed."))
                }
                other => other,
            });
        }
        let base = merge_base_tree(repo, &all_merge_bases(repo, head.id(), source.id())?)?;
        let trees = [&base, &head.tree()?, &source.tree()?];
        for line in merge_report(repo, &repo.index()?, trees, name)? {
            report(OpProgress::Line(line));
        }
    }
    let strategy = if ours { "ours" } else { "ort" };
    conclude_merge(repo, &head, &[(source, name)], true, opts, strategy, report)
}

/// The commits `tips` bring that `head` lacks, newest first.
fn merged_commits<'r>(
    repo: &'r Repository,
    tips: &[Oid],
    head: Oid,
) -> Result<Vec<git2::Commit<'r>>, GitError> {
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
    for tip in tips {
        walk.push(*tip)?;
    }
    walk.hide(head)?;
    Ok(walk
        .filter_map(|oid| repo.find_commit(oid.ok()?).ok())
        .collect())
}

/// Finish a merge whose result is in the index and MERGE_HEAD: write the
/// message, stop for conflicts, `--squash` or `--no-commit`, else commit it
/// with HEAD (when `with_head`) and `heads` as parents.
fn conclude_merge(
    repo: &Repository,
    head: &git2::Commit<'_>,
    heads: &[(&git2::Commit<'_>, &str)],
    with_head: bool,
    opts: &crate::MergeOptions,
    strategy: &str,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let names: Vec<&str> = heads.iter().map(|(_, n)| *n).collect();
    let mut message = match &opts.message {
        Some(m) => m.clone(),
        None => merge_title(repo, &names, opts.into_name.as_deref())?,
    };
    if let Some(limit) = opts.log.filter(|n| *n > 0) {
        for (source, name) in heads {
            let merged = merged_commits(repo, &[source.id()], head.id())?;
            let more = if merged.len() > limit {
                format!(" ({} commits)", merged.len())
            } else {
                String::new()
            };
            message.push_str(&format!("\n\n* {name}:{more}"));
            for c in merged.iter().take(limit) {
                message.push_str(&format!("\n  {}", c.summary().ok().flatten().unwrap_or("")));
            }
            if merged.len() > limit {
                message.push_str("\n  ...");
            }
        }
    }
    let tips: Vec<Oid> = heads.iter().map(|(c, _)| c.id()).collect();
    let merged = merged_commits(repo, &tips, head.id())?;
    if opts.squash {
        // A squash stages the result as an ordinary change: no MERGE_HEAD, and
        // SQUASH_MSG for the commit that follows.
        end_operation(repo)?;
        let mut squash = "Squashed commit of the following:\n".to_owned();
        for c in &merged {
            let author = c.author();
            squash.push_str(&format!(
                "\ncommit {}\nAuthor: {} <{}>\nDate:   {}\n\n",
                c.id(),
                author.name().unwrap_or(""),
                author.email().unwrap_or(""),
                git_date(author.when())
            ));
            for line in c.message().unwrap_or("").trim_end().lines() {
                squash.push_str(format!("    {line}").trim_end());
                squash.push('\n');
            }
        }
        std::fs::write(repo.path().join("SQUASH_MSG"), squash)?;
    } else {
        std::fs::write(repo.path().join("MERGE_MSG"), format!("{message}\n"))?;
    }
    let index = repo.index()?;
    if index.has_conflicts() && !opts.squash {
        let hint = format!(
            "{message}\n{}",
            conflicts_hint(&index, opts.cleanup.as_deref())
        );
        std::fs::write(repo.path().join("MERGE_MSG"), hint)?;
    }
    if index.has_conflicts() {
        for line in crate::rerere::report(repo, opts.rerere_autoupdate).lines() {
            report(OpProgress::Line(line.to_owned()));
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
    let sig = repo.committer_from_env()?;
    if !opts.no_verify {
        run_hook_file(repo, "pre-merge-commit", &[])?;
    }
    if opts.signoff {
        message = signoff(&message, &sig);
    }
    if opts.edit {
        message = edit_message(repo, "MERGE_MSG", &message)?;
    }
    if !opts.no_verify {
        commit_msg_hook(repo, &mut message)?;
    }
    let message = cleanup_message(&message, opts.cleanup.as_deref(), opts.edit)?;
    let tree = repo.find_tree(repo.index()?.write_tree()?)?;
    let parents: Vec<&git2::Commit> = with_head
        .then_some(head)
        .into_iter()
        .chain(heads.iter().map(|(c, _)| *c))
        .collect();
    let key = crate::sign::commit_key(repo, opts.sign.as_deref(), opts.no_sign);
    let id = crate::sign::commit(
        repo,
        None,
        &sig,
        &sig,
        &message,
        &tree,
        &parents,
        key.as_deref(),
    )?;
    let log = format!(
        "merge {}: Merge made by the '{strategy}' strategy.",
        names.join(" ")
    );
    repo.head()?.resolve()?.set_target(id, &log)?;
    end_operation(repo)?;
    report(OpProgress::Line(format!(
        "Merge made by the '{strategy}' strategy."
    )));
    Ok(())
}

/// A commit message cleaned up as git's `--cleanup=<mode>` does; `default` is
/// `strip` for an edited message and `whitespace` otherwise.
fn cleanup_message(msg: &str, mode: Option<&str>, edited: bool) -> Result<String, GitError> {
    const SCISSORS: &str = "# ------------------------ >8 ------------------------\n";
    Ok(match mode.unwrap_or("default") {
        "verbatim" => msg.to_owned(),
        "strip" => git2::message_prettify(msg, Some(b'#'))?,
        "default" if edited => git2::message_prettify(msg, Some(b'#'))?,
        "whitespace" | "default" => git2::message_prettify(msg, None)?,
        "scissors" => {
            let cut = std::iter::once(0)
                .chain(msg.match_indices('\n').map(|(i, _)| i + 1))
                .find(|&i| msg[i..].starts_with(SCISSORS))
                .unwrap_or(msg.len());
            git2::message_prettify(&msg[..cut], None)?
        }
        other => return Err(GitError::Other(format!("Invalid cleanup mode {other}"))),
    })
}

/// git's default merge title for `names`, grouped by kind as fmt-merge-msg
/// does: `Merge branch 'x'`, `Merge branches 'a' and 'b', tag 'v1'`, with
/// `into <branch>` (or `into`'s name) unless that is main or master.
fn merge_title(repo: &Repository, names: &[&str], into: Option<&str>) -> Result<String, GitError> {
    let mut kinds: [(&str, &str, Vec<String>); 4] = [
        ("branch", "branches", Vec::new()),
        (
            "remote-tracking branch",
            "remote-tracking branches",
            Vec::new(),
        ),
        ("tag", "tags", Vec::new()),
        ("commit", "commits", Vec::new()),
    ];
    for name in names {
        let kind = match repo.resolve_reference_from_short_name(name) {
            Ok(r) if r.is_branch() => 0,
            Ok(r) if r.is_remote() => 1,
            Ok(r) if r.is_tag() => 2,
            _ => 3,
        };
        kinds[kind].2.push(format!("'{name}'"));
    }
    let parts: Vec<String> = kinds
        .iter()
        .filter_map(|(one, many, list)| match list.split_last()? {
            (last, []) => Some(format!("{one} {last}")),
            (last, rest) => Some(format!("{many} {} and {last}", rest.join(", "))),
        })
        .collect();
    let mut title = format!("Merge {}", parts.join(", "));
    let branch = match into {
        Some(b) => b.to_owned(),
        None if repo.head_detached().unwrap_or(false) => "HEAD".to_owned(),
        None => current_branch(repo).unwrap_or_default(),
    };
    if !matches!(branch.as_str(), "" | "main" | "master") {
        title.push_str(&format!(" into {branch}"));
    }
    Ok(title)
}

/// Where `repo`'s hooks are: `core.hooksPath` (relative to the working
/// directory), else the hooks folder every worktree shares.
pub(crate) fn hooks_dir(repo: &Repository) -> PathBuf {
    let workdir = repo.workdir().unwrap_or(repo.path());
    match repo.config().and_then(|c| c.get_path("core.hooksPath")) {
        Ok(p) => workdir.join(p),
        Err(_) => repo.commondir().join("hooks"),
    }
}

/// Run the `name` hook from `hooks_dir` (see [`GitBackend::hooks_dir`]) in
/// `cwd` with `args`, feeding it `stdin`, if it is an executable file there.
/// Returns its output, or None when there is no such hook. When hooks stream
/// ([`crate::stream_hooks`]) the output went to stderr and comes back empty.
pub fn run_hook(
    hooks_dir: &Path,
    cwd: &Path,
    name: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<Option<std::process::Output>, GitError> {
    run_hook_env(hooks_dir, cwd, name, args, stdin, &[])
}

pub(crate) fn run_hook_env(
    hooks_dir: &Path,
    cwd: &Path,
    name: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
    env: &[(&str, &Path)],
) -> Result<Option<std::process::Output>, GitError> {
    use std::process::Stdio;
    let Some(hook) = crate::hooks::find_hook(hooks_dir, name) else {
        return Ok(None);
    };
    let mut cmd = std::process::Command::new(&hook);
    cmd.args(args).current_dir(cwd).stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    for (k, v) in env {
        cmd.env(k, v);
    }
    if crate::hooks::streaming() {
        cmd.stdout(std::io::stderr()).stderr(Stdio::inherit());
    } else {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let mut child = cmd.spawn()?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A hook that exits without reading its input is not an error.
        let _ = std::io::Write::write_all(&mut pipe, data);
    }
    Ok(Some(child.wait_with_output()?))
}

/// Run a hook in the working tree with `args`, if the hooks directory has
/// it as an executable file; a failing one is an error carrying its output
/// (none when it streamed).
pub(crate) fn run_hook_file(repo: &Repository, name: &str, args: &[&Path]) -> Result<(), GitError> {
    let args: Vec<String> = args.iter().map(|a| a.display().to_string()).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    hook_checked(repo, name, &args, None, &[])
}

pub(crate) fn hook_checked(
    repo: &Repository,
    name: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
    env: &[(&str, &Path)],
) -> Result<(), GitError> {
    let workdir = repo.workdir().unwrap_or(repo.path());
    let Some(out) = run_hook_env(&hooks_dir(repo), workdir, name, args, stdin, env)? else {
        return Ok(());
    };
    if out.status.success() {
        return Ok(());
    }
    if crate::hooks::streaming() {
        return Err(GitError::Hook(String::new()));
    }
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_owned();
    let detail = match text(&out.stderr) {
        e if e.is_empty() => text(&out.stdout),
        e => e,
    };
    Err(GitError::Hook(format!("{name} hook failed: {detail}")))
}

/// Run the commit-msg hook on `msg` through `.git/COMMIT_EDITMSG`, taking
/// back what it leaves there.
fn commit_msg_hook(repo: &Repository, msg: &mut String) -> Result<(), GitError> {
    if crate::hooks::find_hook(&hooks_dir(repo), "commit-msg").is_none() {
        return Ok(());
    }
    let file = repo.path().join("COMMIT_EDITMSG");
    std::fs::write(&file, msg.as_bytes())?;
    let workdir = repo.workdir().unwrap_or(repo.path());
    let arg = file.strip_prefix(workdir).unwrap_or(&file);
    run_hook_file(repo, "commit-msg", &[arg])?;
    *msg = std::fs::read_to_string(&file)?;
    Ok(())
}

/// Create a repository at `path` (`git init`), via libgit2, and return git's
/// report line.
pub fn init(path: &Path, args: &crate::InitArgs) -> Result<String, GitError> {
    let branch = args.initial_branch.clone().or_else(default_initial_branch);
    let mut opts = git2::RepositoryInitOptions::new();
    opts.bare(args.bare);
    if let Some(b) = branch.as_deref() {
        opts.initial_head(b);
    }
    if let Some(t) = &args.template {
        opts.template_path(Path::new(t));
    }
    if let Some(shared) = &args.shared {
        use git2::RepositoryInitMode as Mode;
        opts.mode(match shared.as_str() {
            "umask" | "false" | "no" | "off" => Mode::SHARED_UMASK,
            "group" | "true" | "yes" | "on" => Mode::SHARED_GROUP,
            "all" | "world" | "everybody" => Mode::SHARED_ALL,
            octal => Mode::from_bits_retain(
                u32::from_str_radix(octal, 8)
                    .map_err(|_| GitError::Other(format!("invalid value for --shared: {octal}")))?,
            ),
        });
    }
    let at = match &args.separate_git_dir {
        // The repository lives there; `path` gets a `.git` file pointing at it.
        Some(dir) => {
            opts.no_dotgit_dir(true)
                .workdir_path(&std::path::absolute(path)?);
            PathBuf::from(dir)
        }
        None => path.to_path_buf(),
    };
    let existed = Repository::open(&at).is_ok();
    let repo = Repository::init_opts(&at, &opts)?;
    if !existed {
        order_core(&repo.path().join("config"))?;
    }
    let shared = if args
        .shared
        .as_deref()
        .is_some_and(|s| !matches!(s, "umask" | "false" | "no" | "off"))
    {
        "shared "
    } else {
        ""
    };
    let verb = if existed {
        "Reinitialized existing"
    } else {
        "Initialized empty"
    };
    Ok(format!(
        "{verb} {shared}Git repository in {}",
        repo.path().display()
    ))
}

/// Put the `[core]` keys libgit2's init wrote in the order git's init
/// writes them.
fn order_core(path: &Path) -> Result<(), GitError> {
    const ORDER: [&str; 7] = [
        "repositoryformatversion",
        "filemode",
        "bare",
        "logallrefupdates",
        "symlinks",
        "ignorecase",
        "precomposeunicode",
    ];
    let text = std::fs::read_to_string(path)?;
    let mut lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim() == "[core]") else {
        return Ok(());
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    lines[start + 1..end].sort_by_key(|l| {
        let key = l.trim().split([' ', '=']).next().unwrap_or("");
        ORDER
            .iter()
            .position(|k| k.eq_ignore_ascii_case(key))
            .unwrap_or(ORDER.len())
    });
    std::fs::write(path, lines.join("\n") + "\n")?;
    Ok(())
}

/// Open the repository git would use from `start`, honouring GIT_DIR and the
/// rest of git's environment. With GIT_DIR but no work tree, the current
/// folder is the top of the work tree, as in git.
pub fn open_env(start: &Path) -> Result<Repository, GitError> {
    crate::config::env_search_paths();
    let not_found = |e: git2::Error| match e.code() {
        ErrorCode::NotFound => GitError::NotARepository(start.to_path_buf()),
        _ => GitError::Git(e),
    };
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let repo = if env("GIT_DIR").is_some() {
        let repo = Repository::open_from_env().map_err(not_found)?;
        let set = repo.config().is_ok_and(|c| {
            c.get_entry("core.worktree").is_ok() || c.get_bool("core.bare").unwrap_or(false)
        });
        if env("GIT_WORK_TREE").is_none() && !set {
            repo.set_workdir(&std::env::current_dir()?, false)?;
        }
        repo
    } else {
        let ceilings: Vec<PathBuf> = env("GIT_CEILING_DIRECTORIES")
            .map(|v| {
                std::env::split_paths(&v)
                    .filter(|p| p.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        Repository::open_ext(start, git2::RepositoryOpenFlags::FROM_ENV, &ceilings)
            .map_err(not_found)?
    };
    crate::config::add_command_line(&repo.config()?)?;
    crate::promisor::install(&repo);
    Ok(repo)
}

fn default_initial_branch() -> Option<String> {
    crate::config::default_config()
        .ok()?
        .get_string("init.defaultBranch")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// A ref a remote advertises: name, object id and symbolic target.
pub type RemoteRef = (String, String, Option<String>);

/// The refs `remote` advertises (`git ls-remote`): its URL, then each ref's
/// name, object id and symbolic target. `remote` is a remote name or a URL, or
/// the current branch's remote when `None`; `dir` is where to look for a
/// repository, if anywhere. With `list` unset only the URL is looked up.
pub fn ls_remote(
    dir: Option<&Path>,
    remote: Option<&str>,
    list: bool,
) -> Result<(String, Vec<RemoteRef>), GitError> {
    let repo = dir.and_then(|d| Repository::discover(d).ok());
    let mut remote = match (&repo, remote) {
        (Some(repo), Some(r)) => repo.find_remote(r).or_else(|_| repo.remote_anonymous(r))?,
        (Some(repo), None) => {
            let name = upstream_remote(repo)
                .map(|(r, _)| r)
                .or_else(|_| default_remote(repo))?;
            repo.find_remote(&name)?
        }
        (None, Some(url)) => git2::Remote::create_detached(url)?,
        (None, None) => return Err(GitError::NotARepository(PathBuf::from("."))),
    };
    let url = remote.url().unwrap_or_default().to_owned();
    if !list {
        return Ok((url, Vec::new()));
    }
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let quiet = |_: OpProgress| {};
    let callbacks = remote_callbacks(&quiet, &ignored, None);
    let conn = remote.connect_auth(git2::Direction::Fetch, Some(callbacks), None)?;
    let refs = conn
        .list()?
        .iter()
        .map(|h| {
            (
                h.name().to_owned(),
                h.oid().to_string(),
                h.symref_target().map(str::to_owned),
            )
        })
        .collect();
    Ok((url, refs))
}

/// Clone `url` into `path` (`git clone`), via libgit2, using the same
/// credentials as fetch/push and reporting git-style progress. What a failed
/// clone made is removed again, as git does.
pub fn clone(
    url: &str,
    path: &Path,
    args: &crate::CloneArgs,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    if let Some(spec) = &args.filter {
        crate::promisor::Filter::parse(spec)?;
    }
    if path.is_file() || path.read_dir().is_ok_and(|mut d| d.next().is_some()) {
        return Err(GitError::Other(format!(
            "destination path '{}' already exists and is not an empty directory.",
            path.display()
        )));
    }
    let made: Vec<PathBuf> = std::iter::once(path.to_path_buf())
        .chain(args.separate_git_dir.iter().map(PathBuf::from))
        .filter(|p| !p.exists())
        .collect();
    let result = clone_into(url, path, args, report);
    if result.is_err() {
        for p in made {
            let _ = std::fs::remove_dir_all(p);
        }
    }
    result
}

fn clone_into(
    url: &str,
    path: &Path,
    args: &crate::CloneArgs,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let plain = is_local_url(url) && !url.starts_with("file://");
    // git records a local path as an absolute one, symlinks and all.
    let url = match plain {
        true if Path::new(url).exists() => std::path::absolute(url)?.to_string_lossy().into_owned(),
        true => {
            return Err(GitError::Other(format!(
                "repository '{url}' does not exist"
            )));
        }
        false => url.to_owned(),
    };
    let (mut depth, mut since) = (args.depth, args.shallow_since.clone());
    if plain && depth > 0 {
        report(OpProgress::Line(
            "warning: --depth is ignored in local clones; use file:// instead.".to_owned(),
        ));
        depth = 0;
    }
    if plain && since.is_some() {
        report(OpProgress::Line(
            "warning: --shallow-since is ignored in local clones; use file:// instead.".to_owned(),
        ));
        since = None;
    }
    if plain && args.filter.is_some() {
        report(OpProgress::Line(
            "warning: --filter is ignored in local clones; use file:// instead.".to_owned(),
        ));
    }
    let bare = args.bare || args.mirror;
    init(
        path,
        &crate::InitArgs {
            bare,
            template: args.template.clone(),
            separate_git_dir: args.separate_git_dir.clone(),
            ..Default::default()
        },
    )?;
    let repo = Repository::open(path)?;

    let mut alternates = Vec::new();
    for r in args.reference.iter().chain(args.shared.then_some(&url)) {
        alternates.push(objects_dir(r)?);
    }
    for r in &args.reference_if_able {
        match objects_dir(r) {
            Ok(dir) => alternates.push(dir),
            Err(e) => report(OpProgress::Line(format!(
                "info: Could not add alternate for '{r}': {e}"
            ))),
        }
    }
    let alternates_file = repo.path().join("objects/info/alternates");
    if !alternates.is_empty() {
        std::fs::create_dir_all(repo.path().join("objects/info"))?;
        std::fs::write(&alternates_file, alternates.concat())?;
    }
    // Opened again to read the objects through the alternates.
    let repo = Repository::open(path)?;

    let origin = args.origin.clone().unwrap_or_else(|| "origin".to_owned());
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let (heads, default) = {
        let mut remote = repo.remote_anonymous(&url)?;
        let callbacks = remote_callbacks(report, &ignored, None);
        let conn = remote.connect_auth(git2::Direction::Fetch, Some(callbacks), None)?;
        let heads: Vec<String> = conn.list()?.iter().map(|h| h.name().to_owned()).collect();
        let default = conn
            .default_branch()
            .ok()
            .and_then(|b| b.as_str().ok().map(|s| short_ref(s).to_owned()));
        (heads, default)
    };
    let has = |r: String| heads.contains(&r);
    let tag_head = match &args.branch {
        Some(b) if has(format!("refs/heads/{b}")) => false,
        Some(b) if has(format!("refs/tags/{b}")) => true,
        Some(b) => {
            return Err(GitError::Other(format!(
                "Remote branch {b} not found in upstream {origin}"
            )));
        }
        None => false,
    };
    // An empty remote's HEAD names the branch to start, as git's ls-refs
    // `unborn` says.
    let unborn = match heads.is_empty() && args.branch.is_none() {
        true => unborn_head(&repo, &url),
        false => None,
    };
    let branch = args.branch.clone().or(default.clone()).or(unborn.clone());
    // A shallow clone takes one branch unless told otherwise, as in git.
    let single = !args.mirror
        && (args.single_branch || ((depth > 0 || since.is_some()) && !args.no_single_branch));
    let top = match bare {
        true => "refs/heads/".to_owned(),
        false => format!("refs/remotes/{origin}/"),
    };
    let fetch = match &branch {
        _ if args.mirror => "+refs/*:refs/*".to_owned(),
        Some(b) if single && tag_head => format!("+refs/tags/{b}:refs/tags/{b}"),
        Some(b) if single => format!("+refs/heads/{b}:{top}{b}"),
        _ => format!("+refs/heads/*:{top}*"),
    };
    if !git2::Remote::is_valid_name(&origin) {
        return Err(GitError::Other(format!(
            "'{origin}' is not a valid remote name"
        )));
    }
    // In the order git writes them: -c settings, then the remote's keys.
    let mut config = repo.config()?;
    for kv in &args.config {
        let (key, value) = kv.split_once('=').unwrap_or((kv, "true"));
        config.set_multivar(&canonical_key(key), "$^", value)?;
    }
    config.set_str(&format!("remote.{origin}.url"), &url)?;
    if args.no_tags || args.mirror {
        config.set_str(&format!("remote.{origin}.tagOpt"), "--no-tags")?;
    }
    config.set_multivar(&format!("remote.{origin}.fetch"), "$^", &fetch)?;
    if args.mirror {
        config.set_bool(&format!("remote.{origin}.mirror"), true)?;
    }

    // Like git: every tag with all branches, those on the history with one.
    let fetch_args = crate::FetchArgs {
        tags: !single && !args.mirror && !args.no_tags,
        no_tags: args.no_tags,
        depth,
        shallow_since: since,
        filter: args.filter.clone().filter(|_| !plain),
        ..Default::default()
    };
    fetch_one(&repo, &origin, &[], &fetch_args, report, None)?;
    if let Some(spec) = args.filter.as_deref().filter(|_| plain) {
        crate::promisor::mark(&repo, &origin, spec)?;
    }
    let _ = std::fs::remove_file(repo.path().join("FETCH_HEAD"));
    if bare && !args.mirror {
        config.remove_multivar(&format!("remote.{origin}.fetch"), ".*")?;
    }
    if args.dissociate && !alternates.is_empty() {
        let mut pack = repo.packbuilder()?;
        let mut walk = repo.revwalk()?;
        for r in repo.references()? {
            let r = r?;
            if let Ok(c) = r.peel_to_commit() {
                walk.push(c.id())?;
            }
            if let Some(id) = r.target() {
                pack.insert_recursive(id, None)?;
            }
        }
        pack.insert_walk(&mut walk)?;
        shallow::write_pack(&mut pack, &repo.odb()?)?;
        std::fs::remove_file(&alternates_file)?;
    }
    // git writes the refs a clone fetches straight into packed-refs, with
    // no reflog; the tags one branch's history brings stay loose.
    let _ = std::fs::remove_dir_all(repo.path().join("logs/refs"));
    let dst = fetch
        .trim_start_matches('+')
        .split_once(':')
        .map_or("", |(_, d)| d);
    pack_refs_where(&repo, |name| {
        !single || ref_matches(dst, name) || map_glob(dst, dst, name).is_some()
    })?;
    let repo = Repository::open(path)?;
    crate::promisor::install(&repo);

    if heads.is_empty() {
        report(OpProgress::Line(
            "warning: You appear to have cloned an empty repository.".to_owned(),
        ));
    }
    let tracking = |b: &str| repo.refname_to_id(&format!("{top}{b}")).ok();
    let msg = format!("clone: from {url}");
    let mut logged = vec!["HEAD".to_owned()];
    match &branch {
        Some(b) if tag_head => {
            let tag = repo.rev_single(&format!("refs/tags/{b}"))?;
            repo.set_head_detached(tag.peel_to_commit()?.id())?;
        }
        Some(b) if bare => repo.set_head(&format!("refs/heads/{b}"))?,
        Some(b) => {
            let id = tracking(b);
            if let Some(id) = id {
                repo.reference(&format!("refs/heads/{b}"), id, true, &msg)?;
                logged.push(format!("refs/heads/{b}"));
            }
            if id.is_some() || unborn.is_some() {
                config.set_str(&format!("branch.{b}.remote"), &origin)?;
                config.set_str(&format!("branch.{b}.merge"), &format!("refs/heads/{b}"))?;
            }
            repo.set_head(&format!("refs/heads/{b}"))?;
        }
        None => {}
    }
    if let Some(d) = default.as_deref().filter(|_| !bare)
        && tracking(d).is_some()
    {
        repo.reference_symbolic(
            &format!("refs/remotes/{origin}/HEAD"),
            &format!("refs/remotes/{origin}/{d}"),
            true,
            &msg,
        )?;
        logged.push(format!("refs/remotes/{origin}/HEAD"));
    }
    if !bare {
        clone_reflogs(&repo, &logged, &msg)?;
    }
    let checkout = !bare && !args.no_checkout && repo.head().is_ok();
    if checkout && args.filter.is_some() {
        let tree = repo.head()?.peel_to_tree()?.id();
        crate::promisor::prefetch(&repo, tree, args.sparse)?;
    }
    if args.sparse && !bare {
        sparse_checkout(&repo, checkout)?;
    } else if checkout {
        repo.checkout_head(Some(CheckoutBuilder::new().force()))?;
    }
    if args.recurse_submodules && checkout {
        update_submodules(&repo, report)?;
    }
    Ok(())
}

/// The objects folder of the local repository at `path`, as a line of an
/// alternates file.
fn objects_dir(path: &str) -> Result<String, GitError> {
    let path = path.strip_prefix("file://").unwrap_or(path);
    let repo = Repository::open(path).map_err(|_| {
        GitError::Other(format!(
            "reference repository '{path}' is not a local repository."
        ))
    })?;
    let dir = std::fs::canonicalize(repo.commondir().join("objects"))?;
    Ok(format!("{}\n", dir.display()))
}

/// Start a cone-mode sparse checkout of the top-level files only, as
/// `clone --sparse` does: every other file is marked skip-worktree in the
/// index and left out of the working tree.
fn sparse_checkout(repo: &Repository, checkout: bool) -> Result<(), GitError> {
    // git keeps these in the worktree's own config.
    repo.config()?.set_bool("extensions.worktreeConfig", true)?;
    let mut config = git2::Config::open(&repo.path().join("config.worktree"))?;
    config.set_bool("core.sparseCheckout", true)?;
    config.set_bool("core.sparseCheckoutCone", true)?;
    std::fs::create_dir_all(repo.path().join("info"))?;
    std::fs::write(repo.path().join("info/sparse-checkout"), "/*\n!/*/\n")?;
    if !checkout {
        return Ok(());
    }
    let mut index = repo.index()?;
    index.read_tree(&repo.head()?.peel_to_tree()?)?;
    let mut top = CheckoutBuilder::new();
    top.force();
    let mut any = false;
    for mut e in index.iter().collect::<Vec<_>>() {
        if e.path.contains(&b'/') {
            e.flags_extended |= libgit2_sys::GIT_INDEX_ENTRY_SKIP_WORKTREE as u16;
            index.add(&e)?;
        } else {
            top.path(&e.path);
            any = true;
        }
    }
    index.write()?;
    if any {
        repo.checkout_index(Some(&mut index), Some(&mut top))?;
    }
    Ok(())
}

/// A config key as git writes it: section and name lowercased, the
/// subsection as given.
fn canonical_key(key: &str) -> String {
    match (key.split_once('.'), key.rsplit_once('.')) {
        (Some((section, _)), Some((head, name))) => format!(
            "{}{}.{}",
            section.to_ascii_lowercase(),
            &head[section.len()..],
            name.to_ascii_lowercase()
        ),
        _ => key.to_owned(),
    }
}

/// The branch an empty remote's HEAD points at, as ls-refs' `unborn` (or,
/// from a local repository, its HEAD) says.
fn unborn_head(repo: &Repository, url: &str) -> Option<String> {
    let target = if is_local_url(url) {
        let src = Repository::open(local_path(repo, url)).ok()?;
        let head = src.find_reference("HEAD").ok()?;
        head.symbolic_target().ok()??.to_owned()
    } else {
        let mut session = crate::smart::Session::open(url, repo.config().ok().as_ref()).ok()?;
        let refs = session.ls_refs(&["HEAD".to_owned()]).ok()?;
        let _ = session.finish();
        refs.into_iter()
            .find(|r| r.name == "HEAD" && r.id.is_zero())?
            .symref?
    };
    target.strip_prefix("refs/heads/").map(str::to_owned)
}

/// Write the direct refs `keep` names into packed-refs, fully peeled and
/// sorted as git writes them, and drop their loose files.
fn pack_refs_where(repo: &Repository, keep: impl Fn(&str) -> bool) -> Result<(), GitError> {
    let mut refs = Vec::new();
    for r in repo.references()? {
        let r = r?;
        if let (Ok(name), Some(id)) = (r.name(), r.target())
            && keep(name)
        {
            refs.push((name.to_owned(), id));
        }
    }
    if refs.is_empty() {
        return Ok(());
    }
    refs.sort();
    let mut out = String::from("# pack-refs with: peeled fully-peeled sorted \n");
    for (name, id) in &refs {
        out.push_str(&format!("{id} {name}\n"));
        if let Ok(tag) = repo.find_tag(*id)
            && let Ok(peeled) = tag.as_object().peel(ObjectType::Any)
        {
            out.push_str(&format!("^{}\n", peeled.id()));
        }
    }
    let common = repo.commondir();
    std::fs::write(common.join("packed-refs"), out)?;
    for (name, _) in &refs {
        let _ = std::fs::remove_file(common.join(name));
    }
    Ok(())
}

/// Leave each of `refs` a reflog of one entry, `msg`, as a clone does.
fn clone_reflogs(repo: &Repository, refs: &[String], msg: &str) -> Result<(), GitError> {
    let sig =
        ident_signature(repo, true).or_else(|_| git2::Signature::now("unknown", "unknown"))?;
    for name in refs {
        let Ok(id) = repo.refname_to_id(name) else {
            continue;
        };
        let mut log = repo.reflog(name)?;
        while !log.is_empty() {
            log.remove(0, false)?;
        }
        log.append(id, &sig, Some(msg))?;
        log.write()?;
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

/// `repo`'s submodules whose paths match `paths` (all when empty).
fn chosen_submodules<'r>(
    repo: &'r Repository,
    paths: &[String],
) -> Result<Vec<git2::Submodule<'r>>, GitError> {
    let paths: Vec<String> = paths
        .iter()
        .map(|p| p.trim_end_matches('/').to_owned())
        .collect();
    Ok(repo
        .submodules()?
        .into_iter()
        .filter(|sm| paths.is_empty() || pathspec_matches(&paths, &sm.path().to_string_lossy()))
        .collect())
}

/// Append `repo`'s submodules to `out`, their paths under `prefix`.
fn list_submodules(
    repo: &Repository,
    prefix: &str,
    recursive: bool,
    out: &mut Vec<crate::SubmoduleInfo>,
) -> Result<(), GitError> {
    let conflicted: Vec<String> = repo
        .index()?
        .conflicts()?
        .flatten()
        .filter_map(|c| String::from_utf8(c.our.or(c.their)?.path).ok())
        .collect();
    for sm in repo.submodules()? {
        let rel = sm.path().to_string_lossy().into_owned();
        let path = format!("{prefix}{rel}");
        let recorded = sm.index_id().or(sm.head_id());
        let sub = sm.open().ok().filter(|r| r.head().is_ok());
        let checked_out = sub.as_ref().and_then(|r| r.head().ok()?.target());
        let state = if conflicted.contains(&rel) {
            'U'
        } else if sub.is_none() {
            '-'
        } else if checked_out != recorded {
            '+'
        } else {
            ' '
        };
        out.push(crate::SubmoduleInfo {
            name: sm.name().unwrap_or_default().to_owned(),
            path: path.clone(),
            url: sm.url().ok().flatten().map(str::to_owned),
            branch: sm.branch().ok().flatten().map(str::to_owned),
            recorded: recorded.map(|o| o.to_string()),
            checked_out: checked_out.map(|o| o.to_string()),
            state,
            describe: sub.as_ref().and_then(describe_head),
        });
        if recursive && let Some(sub) = &sub {
            list_submodules(sub, &format!("{path}/"), true, out)?;
        }
    }
    Ok(())
}

/// HEAD as `git submodule status` names it: a tag, else any ref, else the id.
fn describe_head(repo: &Repository) -> Option<String> {
    let name = |opts: &git2::DescribeOptions| repo.describe(opts).ok()?.format(None).ok();
    name(git2::DescribeOptions::new().describe_tags()).or_else(|| {
        name(
            git2::DescribeOptions::new()
                .describe_all()
                .show_commit_oid_as_fallback(true),
        )
    })
}

/// Check out `branch` from `origin` in a freshly cloned submodule.
fn checkout_remote_branch(sub: &Repository, branch: &str) -> Result<(), GitError> {
    let id = sub.refname_to_id(&format!("refs/remotes/origin/{branch}"))?;
    let commit = sub.find_commit(id)?;
    sub.branch(branch, &commit, true)?
        .set_upstream(Some(&format!("origin/{branch}")))?;
    sub.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().force()))?;
    sub.set_head(&format!("refs/heads/{branch}"))?;
    Ok(())
}

/// The superproject's `.gitmodules`, for editing.
fn gitmodules(repo: &Repository) -> Result<git2::Config, GitError> {
    let dir = repo
        .workdir()
        .ok_or_else(|| GitError::Bare(repo.path().to_path_buf()))?;
    Ok(git2::Config::open(&dir.join(".gitmodules"))?)
}

fn submodule_op(
    repo: &Repository,
    op: &crate::SubmoduleOp,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    use crate::SubmoduleOp as Op;
    let top = repo
        .workdir()
        .ok_or_else(|| GitError::Bare(repo.path().to_path_buf()))?
        .to_path_buf();
    let find = |path: &str| {
        chosen_submodules(repo, &[path.to_owned()])?
            .into_iter()
            .next()
            .ok_or_else(|| GitError::Other(format!("no submodule mapping found for path '{path}'")))
    };
    match op {
        Op::Add {
            url,
            path,
            branch,
            name,
        } => {
            let path = path.clone().unwrap_or_else(|| {
                let base = url.trim_end_matches('/').rsplit(['/', ':']).next();
                base.unwrap_or(url).trim_end_matches(".git").to_owned()
            });
            if let Some(name) = name.as_deref().filter(|n| *n != path) {
                return crate::submodule::add_named(
                    repo,
                    url,
                    &path,
                    name,
                    branch.as_deref(),
                    report,
                );
            }
            report(OpProgress::Line(format!(
                "Cloning into '{}'...",
                top.join(&path).display()
            )));
            let mut sm = repo.submodule(url, Path::new(&path), true)?;
            let sub = sm.clone(None)?;
            if let Some(b) = branch {
                checkout_remote_branch(&sub, b)?;
                gitmodules(repo)?.set_str(&format!("submodule.{path}.branch"), b)?;
            }
            sm.add_finalize()?;
        }
        Op::Init { paths } => {
            let config = repo.config()?;
            for mut sm in chosen_submodules(repo, paths)? {
                let name = sm.name().unwrap_or_default().to_owned();
                if config.get_string(&format!("submodule.{name}.url")).is_ok() {
                    continue;
                }
                sm.init(false)?;
                let url = repo
                    .config()?
                    .get_string(&format!("submodule.{name}.url"))
                    .unwrap_or_default();
                report(OpProgress::Line(format!(
                    "Submodule '{name}' ({url}) registered for path '{}'",
                    sm.path().display()
                )));
            }
        }
        Op::Update {
            paths,
            init,
            recursive,
            remote,
            jobs,
        } => {
            let jobs = jobs.unwrap_or_else(|| {
                let n = repo.config().and_then(|c| c.get_i64("submodule.fetchJobs"));
                n.map_or(1, |n| n.max(1) as usize)
            });
            let o = UpdateOpts {
                init: *init,
                recursive: *recursive,
                remote: *remote,
            };
            update_each(repo, "", paths, o, jobs, report)?
        }
        Op::Sync { paths, recursive } => sync_each(repo, "", paths, *recursive, report)?,
        Op::Deinit { paths, force, all } => {
            if paths.is_empty() && !all {
                return Err(GitError::Other(
                    "Use '--all' if you really want to deinitialize all submodules".to_owned(),
                ));
            }
            for sm in chosen_submodules(repo, paths)? {
                let name = sm.name().unwrap_or_default().to_owned();
                let path = sm.path().to_string_lossy().into_owned();
                let dir = top.join(&path);
                if dir.join(".git").is_dir() {
                    return Err(GitError::Other(format!(
                        "Submodule work tree '{path}' contains a .git directory (use 'rm -rf' if \
                         you really want to remove it including all of its history)"
                    )));
                }
                let status = repo.submodule_status(&name, git2::SubmoduleIgnore::None)?;
                let dirty = git2::SubmoduleStatus::WD_INDEX_MODIFIED
                    | git2::SubmoduleStatus::WD_WD_MODIFIED
                    | git2::SubmoduleStatus::WD_UNTRACKED;
                if !force && status.intersects(dirty) {
                    return Err(GitError::Other(format!(
                        "Submodule work tree '{path}' contains local modifications; use '-f' to \
                         discard them"
                    )));
                }
                if dir.read_dir().is_ok_and(|mut d| d.next().is_some()) {
                    std::fs::remove_dir_all(&dir)?;
                    std::fs::create_dir_all(&dir)?;
                    report(OpProgress::Line(format!("Cleared directory '{path}'")));
                }
                let mut local = repo.config()?.open_level(git2::ConfigLevel::Local)?;
                let url = local.get_string(&format!("submodule.{name}.url"));
                let mut keys = Vec::new();
                let pattern = format!("^submodule\\.{}\\.", regex_escape(&name));
                local
                    .entries(Some(&pattern))?
                    .for_each(|e| keys.extend(e.name().map(str::to_owned)))?;
                for key in &keys {
                    local.remove(key)?;
                }
                if let Ok(url) = url {
                    report(OpProgress::Line(format!(
                        "Submodule '{name}' ({url}) unregistered for path '{path}'"
                    )));
                }
            }
        }
        Op::SetUrl { path, url } => {
            let name = find(path)?.name().unwrap_or_default().to_owned();
            gitmodules(repo)?.set_str(&format!("submodule.{name}.url"), url)?;
            let mut sm = repo.find_submodule(&name)?;
            sm.sync()?;
        }
        Op::SetBranch { path, branch } => {
            let name = find(path)?.name().unwrap_or_default().to_owned();
            let key = format!("submodule.{name}.branch");
            let mut config = gitmodules(repo)?;
            match branch {
                Some(b) => config.set_str(&key, b)?,
                None => {
                    let _ = config.remove(&key);
                }
            }
        }
        Op::AbsorbGitDirs { paths } => crate::submodule::absorb(repo, "", paths, report)?,
        Op::Summary { args } => crate::submodule::summary(repo, args, report)?,
    }
    Ok(())
}

/// Options `submodule update` passes down to nested submodules.
#[derive(Clone, Copy)]
struct UpdateOpts {
    init: bool,
    recursive: bool,
    remote: bool,
}

/// `submodule update` over `repo`'s chosen submodules, reporting paths under
/// `prefix`, `jobs` of them at once.
fn update_each(
    repo: &Repository,
    prefix: &str,
    paths: &[String],
    o: UpdateOpts,
    jobs: usize,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let mut names = Vec::new();
    for mut sm in chosen_submodules(repo, paths)? {
        let name = sm.name().unwrap_or_default().to_owned();
        let path = format!("{prefix}{}", sm.path().display());
        let registered = repo
            .config()?
            .get_string(&format!("submodule.{name}.url"))
            .is_ok();
        if !registered && !o.init {
            continue;
        }
        if !registered {
            sm.init(false)?;
            let url = repo
                .config()?
                .get_string(&format!("submodule.{name}.url"))
                .unwrap_or_default();
            report(OpProgress::Line(format!(
                "Submodule '{name}' ({url}) registered for path '{path}'"
            )));
        }
        names.push(name);
    }
    if jobs <= 1 || names.len() <= 1 {
        for name in &names {
            update_one(repo, prefix, name, o, report)?;
        }
        return Ok(());
    }
    // Each thread works on its own handle; output is replayed in order.
    use rayon::prelude::*;
    let gitdir = repo.path().to_path_buf();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|e| GitError::Other(e.to_string()))?;
    let results: Vec<(Vec<String>, Result<(), GitError>)> = pool.install(|| {
        names
            .par_iter()
            .map(|name| {
                let lines = Mutex::new(Vec::new());
                let result = Repository::open(&gitdir)
                    .map_err(GitError::from)
                    .and_then(|r| {
                        update_one(&r, prefix, name, o, &|p| {
                            if let OpProgress::Line(l) = p {
                                lines.lock().expect("lines mutex").push(l);
                            }
                        })
                    });
                (lines.into_inner().expect("lines mutex"), result)
            })
            .collect()
    });
    for (lines, result) in results {
        for line in lines {
            report(OpProgress::Line(line));
        }
        result?;
    }
    Ok(())
}

/// Clone (if needed) and check out one registered submodule, by name.
fn update_one(
    repo: &Repository,
    prefix: &str,
    name: &str,
    o: UpdateOpts,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let top = repo.workdir().unwrap_or(repo.path()).to_path_buf();
    let mut sm = repo.find_submodule(name)?;
    let path = format!("{prefix}{}", sm.path().display());
    {
        let before = sm.open().ok().and_then(|r| r.head().ok()?.target());
        if before.is_none() {
            report(OpProgress::Line(format!(
                "Cloning into '{}'...",
                top.join(sm.path()).display()
            )));
        }
        sm.update(true, None)?;
        let sub = sm.open()?;
        let after = if o.remote {
            // --remote: the submodule's remote-tracking branch, not the
            // recorded commit.
            sub.find_remote("origin")?
                .fetch(&[] as &[&str], None, None)?;
            let tracking = match sm.branch().ok().flatten() {
                Some(b) => format!("refs/remotes/origin/{b}"),
                None => sub
                    .find_reference("refs/remotes/origin/HEAD")
                    .ok()
                    .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
                    .unwrap_or_else(|| "refs/remotes/origin/HEAD".to_owned()),
            };
            let id = sub.refname_to_id(&tracking)?;
            sub.checkout_tree(
                &sub.find_object(id, None)?,
                Some(CheckoutBuilder::new().safe()),
            )?;
            sub.set_head_detached(id)?;
            Some(id)
        } else {
            sub.head().ok().and_then(|h| h.target())
        };
        if let Some(id) = after.filter(|a| Some(*a) != before) {
            report(OpProgress::Line(format!(
                "Submodule path '{path}': checked out '{id}'"
            )));
        }
        if o.recursive {
            update_each(&sub, &format!("{path}/"), &[], o, 1, report)?;
        }
    }
    Ok(())
}

/// `submodule sync`: copy each chosen submodule's URL from `.gitmodules`.
fn sync_each(
    repo: &Repository,
    prefix: &str,
    paths: &[String],
    recursive: bool,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    for mut sm in chosen_submodules(repo, paths)? {
        let path = format!("{prefix}{}", sm.path().display());
        report(OpProgress::Line(format!(
            "Synchronizing submodule url for '{path}'"
        )));
        sm.sync()?;
        if recursive && let Ok(sub) = sm.open() {
            sync_each(&sub, &format!("{path}/"), &[], true, report)?;
        }
    }
    Ok(())
}

/// `s` with regex metacharacters escaped, for a config-name pattern.
fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            let special = r"\.+*?()|[]{}^$".contains(c);
            special.then_some('\\').into_iter().chain([c])
        })
        .collect()
}

/// A 7-character short oid, as git prints in ref-update lines.
pub(crate) fn short7(oid: git2::Oid) -> String {
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

/// The prefetch maintenance task: fetch each remote's branches into
/// refs/prefetch/<its tracking refs>, leaving tags, FETCH_HEAD and the
/// remote-tracking refs alone.
fn prefetch(repo: &Repository, cred: Option<&dyn crate::CredentialPrompt>) -> Result<(), GitError> {
    let names = repo.remotes()?;
    for name in names.iter().flatten().flatten() {
        let skip = repo
            .config()
            .and_then(|c| c.get_bool(&format!("remote.{name}.skipFetchAll")))
            .unwrap_or(false);
        if skip {
            continue;
        }
        let configured = repo.find_remote(name)?;
        let Some(url) = configured.url().map(str::to_owned).ok() else {
            continue;
        };
        let specs: Vec<String> = configured
            .fetch_refspecs()?
            .iter()
            .flatten()
            .flatten()
            .filter_map(|s| {
                let (src, dst) = s.split_once(':')?;
                let dst = dst.strip_prefix("refs/")?;
                let force = if src.starts_with('+') { "" } else { "+" };
                Some(format!("{force}{src}:refs/prefetch/{dst}"))
            })
            .collect();
        if specs.is_empty() {
            continue;
        }
        let mut remote = repo.remote_anonymous(&url)?;
        let ignored = std::sync::atomic::AtomicBool::new(false);
        let mut opts = FetchOptions::new();
        opts.remote_callbacks(remote_callbacks(&|_| {}, &ignored, cred))
            .download_tags(git2::AutotagOption::None)
            .update_fetchhead(false);
        let specs: Vec<&str> = specs.iter().map(String::as_str).collect();
        remote.fetch(&specs, Some(&mut opts), None)?;
    }
    Ok(())
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
                if let Ok(config) = crate::config::default_config()
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
    // The remote's own messages, without the object-counting progress git
    // shows only on a terminal.
    cb.sideband_progress(move |data| {
        const PROGRESS: [&str; 5] = [
            "Counting objects",
            "Compressing objects",
            "Enumerating objects",
            "Total ",
            "Resolving deltas",
        ];
        for line in String::from_utf8_lossy(data)
            .split(['\r', '\n'])
            .map(str::trim)
            .filter(|l| !l.is_empty() && !PROGRESS.iter().any(|p| l.starts_with(p)))
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

/// Create a commit on HEAD from the current index (or, with `paths`, from HEAD
/// plus those paths), or rewrite HEAD when amending, running the pre-commit and
/// commit-msg hooks unless `no_verify`. The message is cleaned up as git does.
fn write_commit(repo: &Repository, message: &str, o: &CommitOptions) -> Result<(), GitError> {
    // Refresh from disk first: a plain `git add` since the backend last touched
    // the index must not be lost by the hook machinery writing the stale cached
    // index back out.
    sync_index(repo)?;
    let head = match repo.head() {
        Ok(head) => Some(head.peel_to_commit()?),
        Err(_) if !o.amend => None,
        Err(e) => return Err(e.into()),
    };
    let mut only = if o.paths.is_empty() {
        None
    } else {
        Some(only_paths_tree(repo, head.as_ref(), &o.paths)?)
    };

    let mut msg = git2::message_prettify(message, None)?;
    if msg.is_empty() {
        return Err(GitError::Other(
            "aborting commit due to empty commit message".to_owned(),
        ));
    }
    let sig = ident_signature(repo, true)?;
    let me = ident_signature(repo, false)?;
    if o.signoff {
        msg = signoff(&msg, &sig);
    }
    if !o.no_verify {
        // git runs pre-commit for `commit <paths>` against the partial index.
        if let Some(only) = only.as_mut() {
            let tmp = repo.path().join("next-index-rgit.lock");
            let mut index = git2::Index::open(&tmp)?;
            index.read_tree(&repo.find_tree(*only)?)?;
            index.write()?;
            let hook = pre_commit_with_index(repo, &tmp)
                .and_then(|()| Ok(git2::Index::open(&tmp)?.write_tree_to(repo)?));
            let _ = std::fs::remove_file(&tmp);
            *only = hook?;
            commit_msg_hook(repo, &mut msg)?;
        } else {
            run_commit_hooks(repo, &mut msg)?;
        }
    }
    // After the commit-msg hook, which a Gerrit setup may use to add its own
    // Change-Id, so rgit never stamps a second one. Amend carries the original
    // change id forward so the logical change keeps its identity.
    let msg = match head.as_ref().filter(|_| o.amend) {
        Some(head) => crate::change_id::preserve(repo, head.message().unwrap_or(""), &msg),
        None => crate::change_id::ensure(repo, &msg),
    };

    let tree_oid = match only {
        Some(oid) => oid,
        None => index_tree(repo)?,
    };
    let tree = repo.find_tree(tree_oid)?;
    let mut author = match (&o.author, &o.author_from) {
        (Some(a), _) => {
            let found = find_author(repo, a)?;
            Some(git2::Signature::new(
                found.name().unwrap_or(""),
                found.email().unwrap_or(""),
                &me.when(),
            )?)
        }
        _ if o.reset_author => Some(me.clone()),
        (None, Some(rev)) => Some(repo.rev_single(rev)?.peel_to_commit()?.author().to_owned()),
        (None, None) => None,
    };
    if let Some((secs, offset)) = o.date {
        let base = match (&author, &head) {
            (Some(a), _) => a.clone(),
            (None, Some(h)) if o.amend => h.author().to_owned(),
            _ => me.clone(),
        };
        author = Some(git2::Signature::new(
            base.name().unwrap_or(""),
            base.email().unwrap_or(""),
            &git2::Time::new(secs, offset),
        )?);
    }
    // Committing a stopped merge records every merged head as a parent, as git does.
    let merging = repo.state() == git2::RepositoryState::Merge;
    let mut merge_heads = Vec::new();
    if merging {
        for line in std::fs::read_to_string(repo.path().join("MERGE_HEAD"))?.lines() {
            merge_heads.push(repo.find_commit(Oid::from_str(line.trim())?)?);
        }
    }
    let key = crate::sign::commit_key(repo, o.sign.as_deref(), o.no_sign);
    match head {
        Some(head) if o.amend && key.is_some() => {
            let parents: Vec<git2::Commit> = head.parents().collect();
            let parents: Vec<&git2::Commit> = parents.iter().collect();
            let author = author.unwrap_or_else(|| head.author().to_owned());
            let id = crate::sign::commit(
                repo,
                None,
                &author,
                &sig,
                &msg,
                &tree,
                &parents,
                key.as_deref(),
            )?;
            let summary = msg.lines().next().unwrap_or("");
            repo.head()?
                .set_target(id, &format!("commit (amend): {summary}"))?;
        }
        // Commit::amend rewrites HEAD keeping its parents, which the ref-updating
        // commit refuses ("current tip is not the first parent").
        Some(head) if o.amend => {
            let new = head.amend(
                Some("HEAD"),
                author.as_ref(),
                Some(&sig),
                None,
                Some(&msg),
                Some(&tree),
            )?;
            crate::notes::copy_for_rewrite(repo, "amend", &[(head.id(), new)])?;
        }
        head => {
            if !o.allow_empty && !merging && head.as_ref().is_some_and(|p| p.tree_id() == tree_oid)
            {
                return Err(GitError::NothingToCommit);
            }
            let parents: Vec<&git2::Commit> = head.iter().chain(&merge_heads).collect();
            let author = author.as_ref().unwrap_or(&me);
            crate::sign::commit(
                repo,
                Some("HEAD"),
                author,
                &sig,
                &msg,
                &tree,
                &parents,
                key.as_deref(),
            )?;
        }
    }
    // Like `git commit`: the in-progress merge, cherry-pick or revert is done, but
    // a cherry-pick sequence keeps its todo for `--continue`.
    for file in [
        "MERGE_HEAD",
        "MERGE_MODE",
        "MERGE_MSG",
        "SQUASH_MSG",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        let _ = std::fs::remove_file(repo.path().join(file));
    }
    crate::rerere::say(repo, None);
    if merging && let Some(id) = take_merge_autostash(repo) {
        eprintln!("{}", autostash_apply(repo, id)?);
    }

    if !o.no_verify {
        let _ = run_hook_file(repo, "post-commit", &[]);
    }
    Ok(())
}

/// Stash local changes to tracked files for `--autostash`, off the stash list
/// as git keeps it, and reset them away.
fn autostash_create(
    repo: &Repository,
    report: &dyn Fn(OpProgress),
) -> Result<Option<Oid>, GitError> {
    let mut status = StatusOptions::new();
    status.include_untracked(false);
    if repo
        .statuses(Some(&mut status))?
        .iter()
        .all(|e| e.status() == Status::CURRENT)
    {
        return Ok(None);
    }
    let mut own = Repository::open(repo.path())?;
    let sig = stash_signature(&own, true)?;
    let id = own.stash_save2(&sig, Some("autostash"), None)?;
    own.stash_drop(0)?;
    sync_index(repo)?;
    report(OpProgress::Line(format!(
        "Created autostash: {}",
        short7(id)
    )));
    Ok(Some(id))
}

/// Reapply an autostash; when it conflicts it goes to the stash list instead,
/// as git does. Returns git's report line.
fn autostash_apply(repo: &Repository, id: Oid) -> Result<String, GitError> {
    stash_store(repo, id)?;
    let mut own = Repository::open(repo.path())?;
    let applied = own.stash_pop(0, None);
    sync_index(repo)?;
    Ok(match applied {
        Ok(()) => "Applied autostash.".to_owned(),
        Err(_) => {
            "Applying autostash resulted in conflicts.\nYour changes are safe in the stash.\n\
                   You can run \"git stash pop\" or \"git stash drop\" at any time."
                .to_owned()
        }
    })
}

/// Put stash commit `id` on top of the stash list (git's `stash store`).
fn stash_store(repo: &Repository, id: Oid) -> Result<(), GitError> {
    repo.reference("refs/stash", id, true, "autostash")?;
    let mut log = repo.reflog("refs/stash")?;
    if log.get(0).map(|e| e.id_new()) != Some(id) {
        log.append(id, &ident_signature(repo, true)?, Some("autostash"))?;
        log.write()?;
    }
    Ok(())
}

/// The autostash a stopped merge keeps in MERGE_AUTOSTASH, removing the file.
fn take_merge_autostash(repo: &Repository) -> Option<Oid> {
    let path = repo.path().join("MERGE_AUTOSTASH");
    let id = Oid::from_str(std::fs::read_to_string(&path).ok()?.trim()).ok()?;
    let _ = std::fs::remove_file(path);
    Some(id)
}

/// The tree for `commit <paths>`: HEAD's tree with `paths` as they are in the
/// working tree. The index gets the same update, as in git.
fn only_paths_tree(
    repo: &Repository,
    head: Option<&git2::Commit>,
    paths: &[String],
) -> Result<Oid, GitError> {
    let paths = &root_dots(paths);
    let head_tree = head.map(|h| h.tree()).transpose()?;
    let mut index = repo.index()?;
    for p in paths {
        known(Some(&index), head_tree.as_ref(), p)?;
    }
    let skip = crate::sparse::keep_skipped(repo, &index);
    crate::pathspec::index_walk(paths, skip, |s, cb| index.update_all(s, cb))?;
    index.write()?;
    let spec = Pathspec::new(paths)?;
    let path_of = |e: &git2::IndexEntry| PathBuf::from(String::from_utf8_lossy(&e.path).as_ref());
    let hit = |e: &git2::IndexEntry| spec.matches_path(&path_of(e));
    let mut partial = git2::Index::new()?;
    if let Some(tree) = &head_tree {
        partial.read_tree(tree)?;
    }
    let gone: Vec<PathBuf> = partial
        .iter()
        .filter(|e| hit(e))
        .map(|e| path_of(&e))
        .collect();
    for p in gone {
        partial.remove_path(&p)?;
    }
    for e in index.iter().filter(|e| hit(e)) {
        partial.add(&e)?;
    }
    Ok(partial.write_tree_to(repo)?)
}

/// `msg` with the committer's `Signed-off-by` trailer, as `git commit -s` adds it.
pub(crate) fn signoff(msg: &str, sig: &git2::Signature) -> String {
    let line = format!(
        "Signed-off-by: {} <{}>",
        sig.name().unwrap_or(""),
        sig.email().unwrap_or("")
    );
    let body = msg.trim_end();
    if body.lines().last() == Some(line.as_str()) {
        return msg.to_owned();
    }
    let last = body.rsplit("\n\n").next().unwrap_or("");
    let trailers = body.contains("\n\n")
        && last.lines().all(|l| {
            l.starts_with("(cherry picked from commit ")
                || l.split_once(": ")
                    .is_some_and(|(k, _)| !k.is_empty() && !k.contains(' '))
        });
    let sep = if trailers { "\n" } else { "\n\n" };
    format!("{body}{sep}{line}\n")
}

fn file_mode(mode: u32) -> git2::FileMode {
    match mode {
        0o100755 => git2::FileMode::BlobExecutable,
        0o120000 => git2::FileMode::Link,
        0o160000 => git2::FileMode::Commit,
        _ => git2::FileMode::Blob,
    }
}

/// One entry of the index's resolve-undo record, which libgit2 keeps but
/// git2 does not wrap.
#[repr(C)]
struct ReucEntry {
    mode: [u32; 3],
    oid: [libgit2_sys::git_oid; 3],
    path: *const std::ffi::c_char,
}

unsafe extern "C" {
    fn git_index_reuc_entrycount(index: *mut libgit2_sys::git_index) -> usize;
    fn git_index_reuc_get_byindex(index: *mut libgit2_sys::git_index, n: usize)
    -> *const ReucEntry;
    fn git_index_reuc_remove(index: *mut libgit2_sys::git_index, n: usize) -> std::ffi::c_int;
}

/// git's unmerge_index: the paths `hit` picks that were resolved get their
/// conflict stages back from the resolve-undo record.
pub(crate) fn unmerge_index(
    index: &mut git2::Index,
    hit: &dyn Fn(&str) -> bool,
) -> Result<(), GitError> {
    let raw = git2::Binding::raw(&*index);
    // SAFETY: `raw` is the live index; entries are read before removal.
    let count = unsafe { git_index_reuc_entrycount(raw) };
    for n in (0..count).rev() {
        let (path, stages) = unsafe {
            let e = &*git_index_reuc_get_byindex(raw, n);
            let path = std::ffi::CStr::from_ptr(e.path)
                .to_string_lossy()
                .into_owned();
            let stages: Vec<(u16, u32, Oid)> = (0..3)
                .filter(|&s| e.mode[s] != 0)
                .map(|s| {
                    let oid = Oid::from_bytes(&e.oid[s].id).unwrap_or(Oid::ZERO_SHA1);
                    (s as u16 + 1, e.mode[s], oid)
                })
                .collect();
            (path, stages)
        };
        if !hit(&path) {
            continue;
        }
        let _ = index.remove(Path::new(&path), 0);
        for (stage, mode, id) in stages {
            index.add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode,
                uid: 0,
                gid: 0,
                file_size: 0,
                id,
                flags: (stage << 12) | (path.len().min(0xfff) as u16),
                flags_extended: 0,
                path: path.clone().into_bytes(),
            })?;
        }
        // SAFETY: `n` is still in range, as removals go from the end.
        unsafe { git_index_reuc_remove(raw, n) };
    }
    Ok(())
}

/// The tree of the index as a commit records it: intent-to-add entries
/// (`add -N`) are left out, as git leaves them.
fn index_tree(repo: &Repository) -> Result<Oid, GitError> {
    let mut index = repo.index()?;
    const INTENT_TO_ADD: u16 = 1 << 13;
    if !index.iter().any(|e| e.flags_extended & INTENT_TO_ADD != 0) {
        return Ok(index.write_tree()?);
    }
    let mut kept = git2::Index::new()?;
    for e in index
        .iter()
        .filter(|e| e.flags_extended & INTENT_TO_ADD == 0)
    {
        kept.add(&e)?;
    }
    Ok(kept.write_tree_to(repo)?)
}

/// The committer or author as git takes it: GIT_{COMMITTER,AUTHOR}_{NAME,
/// EMAIL,DATE} first, then user.name and user.email, dated now.
pub(crate) fn ident_signature(
    repo: &Repository,
    committer: bool,
) -> Result<git2::Signature<'static>, GitError> {
    signature_of(&crate::plumbing::ident(repo, committer)?)
}

/// The ident git stash records: git's usual rules, but `git stash
/// <git@stash>` fills in a name or email nothing configures.
pub(crate) fn stash_signature(
    repo: &Repository,
    committer: bool,
) -> Result<git2::Signature<'static>, GitError> {
    signature_of(&crate::plumbing::ident_or(
        repo,
        committer,
        Some(("git stash", "git@stash")),
    )?)
}

fn signature_of(ident: &str) -> Result<git2::Signature<'static>, GitError> {
    let (name, rest) = ident.split_once(" <").unwrap_or((ident, ""));
    let (email, when) = rest.split_once("> ").unwrap_or((rest, ""));
    let (secs, zone) = when.split_once(' ').unwrap_or((when, "+0000"));
    let n: i32 = zone.get(1..).and_then(|n| n.parse().ok()).unwrap_or(0);
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let time = git2::Time::new(secs.parse().unwrap_or(0), sign * (n / 100 * 60 + n % 100));
    Ok(git2::Signature::new(name, email, &time)?)
}

/// A `Name <email>` identity, stamped now.
fn parse_ident(ident: &str) -> Result<git2::Signature<'static>, GitError> {
    let bad = || GitError::Other(format!("--author '{ident}' is not 'Name <email>'"));
    let (name, rest) = ident.split_once('<').ok_or_else(bad)?;
    let email = rest.trim_end().strip_suffix('>').ok_or_else(bad)?;
    Ok(git2::Signature::now(name.trim(), email.trim())?)
}

/// git's `--author`: `Name <email>`, or else the newest existing author whose
/// `Name <email>` matches `ident` as a case-insensitive regex.
fn find_author(repo: &Repository, ident: &str) -> Result<git2::Signature<'static>, GitError> {
    if let Ok(sig) = parse_ident(ident) {
        return Ok(sig);
    }
    let re = regex::RegexBuilder::new(ident)
        .case_insensitive(true)
        .build()
        .map_err(|e| GitError::Other(e.to_string()))?;
    let mut walk = repo.revwalk()?;
    walk.push_glob("refs/*")?;
    let _ = walk.push_head();
    walk.set_sorting(git2::Sort::TIME)?;
    for oid in walk {
        let a = repo.find_commit(oid?)?.author().to_owned();
        let (name, email) = (a.name().unwrap_or(""), a.email().unwrap_or(""));
        if re.is_match(&format!("{name} <{email}>")) {
            return Ok(git2::Signature::now(name, email)?);
        }
    }
    Err(GitError::Other(format!(
        "--author '{ident}' is not 'Name <email>' and matches no existing author"
    )))
}

/// Run the pre-commit hook with `GIT_INDEX_FILE` set to `index`, as git does
/// for `commit <paths>`.
fn pre_commit_with_index(repo: &Repository, index: &Path) -> Result<(), GitError> {
    hook_checked(repo, "pre-commit", &[], None, &[("GIT_INDEX_FILE", index)])
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
/// Every value of a multi-valued config key, in file order.
fn config_values(repo: &Repository, key: &str) -> Result<Vec<String>, GitError> {
    let config = open_config(repo, ConfigScope::Any, false)?;
    let mut out = Vec::new();
    let mut entries = match config.multivar(key, None) {
        Ok(entries) => entries,
        Err(e) if e.code() == ErrorCode::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = entries.next() {
        out.extend(entry?.value().map(str::to_owned));
    }
    Ok(out)
}

/// A remote's URLs as git uses them: `pushurl`s, else `url`s, for `push`.
fn remote_urls(repo: &Repository, name: &str, push: bool) -> Result<Vec<String>, GitError> {
    let push_urls = if push {
        config_values(repo, &format!("remote.{name}.pushurl"))?
    } else {
        Vec::new()
    };
    if push_urls.is_empty() {
        config_values(repo, &format!("remote.{name}.url"))
    } else {
        Ok(push_urls)
    }
}

/// A stash commit of worktree `tree` as git makes one: the index as its
/// second parent, and git's `WIP on <branch>: ...` or `On <branch>: <msg>`.
fn stash_commit<'r>(
    repo: &'r Repository,
    message: Option<&str>,
    tree: &git2::Tree,
) -> Result<git2::Commit<'r>, GitError> {
    let head = repo.head()?;
    let commit = head.peel_to_commit()?;
    let branch = if head.is_branch() {
        head.shorthand().unwrap_or("HEAD")
    } else {
        "(no branch)"
    };
    let short = commit.as_object().short_id()?;
    let base = format!(
        "{branch}: {} {}",
        short.as_str().unwrap_or_default(),
        commit.summary().ok().flatten().unwrap_or_default()
    );
    let (author, sig) = (stash_signature(repo, false)?, stash_signature(repo, true)?);
    let index_tree = repo.find_tree(repo.index()?.write_tree()?)?;
    let index = repo.commit(
        None,
        &author,
        &sig,
        &format!("index on {base}\n"),
        &index_tree,
        &[&commit],
    )?;
    let message = match message {
        Some(m) => format!("On {branch}: {m}\n"),
        None => format!("WIP on {base}\n"),
    };
    let id = repo.commit(
        None,
        &author,
        &sig,
        &message,
        tree,
        &[&commit, &repo.find_commit(index)?],
    )?;
    Ok(repo.find_commit(id)?)
}

/// libgit2 stamps a stash's commits with one signature, `sig`; git makes the
/// author ident their author. Recommit them so, keeping one reflog entry.
fn restamp_stash(repo: &Repository, sig: &git2::Signature) -> Result<(), GitError> {
    let author = stash_signature(repo, false)?;
    if author.name_bytes() == sig.name_bytes() && author.email_bytes() == sig.email_bytes() {
        return Ok(());
    }
    let recommit = |c: &git2::Commit, parents: &[git2::Commit]| -> Result<Oid, GitError> {
        let parents: Vec<&git2::Commit> = parents.iter().collect();
        let msg = c.message_raw().unwrap_or("");
        Ok(repo.commit(None, &author, sig, msg, &c.tree()?, &parents)?)
    };
    let w = repo.find_reference("refs/stash")?.peel_to_commit()?;
    let mut parents = vec![w.parent(0)?];
    for p in w.parents().skip(1) {
        let id = recommit(&p, &p.parents().collect::<Vec<_>>())?;
        parents.push(repo.find_commit(id)?);
    }
    let new = recommit(&w, &parents)?;
    let mut log = repo.reflog("refs/stash")?;
    let msg = log
        .get(0)
        .and_then(|e| e.message().ok().flatten().map(str::to_owned))
        .unwrap_or_default();
    repo.reference("refs/stash", new, true, &msg)?;
    log = repo.reflog("refs/stash")?;
    log.remove(1, true)?;
    log.write()?;
    Ok(())
}

/// Point `refs/stash` at `id`, logging `message` in its reflog.
pub(crate) fn store_stash(repo: &Repository, id: Oid, message: &str) -> Result<(), GitError> {
    // libgit2 logs refs/stash only once its reflog exists.
    repo.reference_ensure_log("refs/stash")?;
    repo.reference("refs/stash", id, true, message)?;
    Ok(())
}

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
    let sig = ident_signature(repo, true)?;
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
        new_tip = crate::sign::commit_configured(
            repo,
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
    let sig = ident_signature(repo, true)?;
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
        tip = crate::sign::commit_configured(
            repo,
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
    run_hook_file(repo, "pre-commit", &[])?;
    commit_msg_hook(repo, msg)
}

/// Apply a single hunk (selected by its new-side start line) of `diff` to the
/// index, using libgit2's own patch machinery.
/// Whether a pathspec (file, folder or glob) matches `path`, as git matches it.
pub fn pathspec_matches(specs: &[String], path: &str) -> bool {
    Pathspec::new(specs).is_ok_and(|spec| spec.matches(path))
}

fn did_not_match(path: &str) -> GitError {
    GitError::Other(format!("pathspec '{path}' did not match any files"))
}

/// git rm's safety rules: refuse files whose index differs from HEAD (staged)
/// or from the working tree (local), unless `cached`, which refuses only both.
fn rm_check(
    repo: &Repository,
    index: &git2::Index,
    hits: &[String],
    cached: bool,
) -> Result<(), GitError> {
    let head = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let (mut both, mut staged, mut local) = (Vec::new(), Vec::new(), Vec::new());
    for hit in hits {
        let Some(entry) = index.get_path(Path::new(hit), 0) else {
            continue;
        };
        let in_head = head.as_ref().and_then(|t| t.get_path(Path::new(hit)).ok());
        let is_staged =
            in_head.is_none_or(|t| t.id() != entry.id || t.filemode() as u32 != entry.mode);
        let full = repo.workdir().unwrap_or(repo.path()).join(hit);
        let is_local = full.symlink_metadata().is_ok()
            && !full.is_dir()
            && repo
                .status_file(Path::new(hit))?
                .intersects(Status::WT_MODIFIED | Status::WT_TYPECHANGE);
        match (is_staged, is_local) {
            (true, true) => both.push(hit.as_str()),
            (true, false) if !cached => staged.push(hit.as_str()),
            (false, true) if !cached => local.push(hit.as_str()),
            _ => {}
        }
    }
    let mut msg = Vec::new();
    for (list, what, hint) in [
        (
            both,
            "staged content different from both the file and the HEAD",
            "(use -f to force removal)",
        ),
        (
            staged,
            "changes staged in the index",
            "(use --cached to keep the file, or -f to force removal)",
        ),
        (
            local,
            "local modifications",
            "(use --cached to keep the file, or -f to force removal)",
        ),
    ] {
        if list.is_empty() {
            continue;
        }
        let (files, has) = if list.len() == 1 {
            ("file", "has")
        } else {
            ("files", "have")
        };
        msg.push(format!(
            "the following {files} {has} {what}:\n    {}\n{hint}",
            list.join("\n    ")
        ));
    }
    if msg.is_empty() {
        Ok(())
    } else {
        Err(GitError::Other(msg.join("\n")))
    }
}

/// Index paths a pathspec matches.
fn index_matches(index: &git2::Index, path: &str) -> Result<Vec<String>, GitError> {
    Ok(Pathspec::new([path])?.match_index(index))
}

/// Fail like git when a pathspec names nothing in the working tree, the index
/// or HEAD.
fn no_match(repo: &Repository, path: &str) -> Result<(), GitError> {
    let spec = Pathspec::new([path])?;
    let hit = !spec.match_index(&repo.index()?).is_empty()
        || repo
            .head()
            .and_then(|h| h.peel_to_tree())
            .is_ok_and(|t| !spec.match_tree(&t).is_empty())
        || repo.workdir().is_some_and(|w| spec.any_in_workdir(w));
    if hit {
        Ok(())
    } else {
        Err(did_not_match(path))
    }
}

/// git's `.` (the whole repo, at the root) spelled as libgit2 pathspecs take it.
fn root_dot(path: &str) -> &str {
    if path == "." && crate::pathspec::plain_env() {
        "*"
    } else {
        path
    }
}

fn root_dots(paths: &[String]) -> Vec<String> {
    paths.iter().map(|p| root_dot(p).to_owned()).collect()
}

/// Fail like git when a pathspec names nothing in `index` or `tree`, the files
/// git knows.
fn known(
    index: Option<&git2::Index>,
    tree: Option<&git2::Tree>,
    path: &str,
) -> Result<(), GitError> {
    let spec = Pathspec::new([path])?;
    let hit = index.is_some_and(|i| !spec.match_index(i).is_empty())
        || tree.is_some_and(|t| !spec.match_tree(t).is_empty());
    if hit {
        Ok(())
    } else {
        Err(GitError::Other(format!(
            "pathspec '{path}' did not match any file(s) known to git"
        )))
    }
}

/// The second the index file was last written, if it exists.
fn index_second(repo: &Repository) -> Option<i64> {
    let modified = std::fs::metadata(repo.path().join("index"))
        .ok()?
        .modified()
        .ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(secs).ok()
}

/// Clear merge, cherry-pick, revert and rebase state like libgit2's
/// `cleanup_state`, which also deletes BISECT_LOG; keep that, since a bisect
/// can be in progress around the operation.
fn end_operation(repo: &Repository) -> Result<(), GitError> {
    let log = repo.path().join("BISECT_LOG");
    let saved = std::fs::read(&log).ok();
    repo.cleanup_state()?;
    let _ = std::fs::remove_file(repo.path().join("MERGE_RR"));
    if let Some(bytes) = saved {
        std::fs::write(&log, bytes)?;
    }
    Ok(())
}

/// The new-side start line of each hunk of `path` in `diff`.
fn hunk_starts(diff: &Diff, path: &str) -> Vec<u32> {
    let mut starts = Vec::new();
    for idx in 0..diff.deltas().len() {
        let same = diff
            .get_delta(idx)
            .and_then(|d| d.new_file().path().map(|p| p.to_string_lossy() == path))
            .unwrap_or(false);
        if let (true, Ok(Some(patch))) = (same, Patch::from_diff(diff, idx)) {
            starts.extend(
                (0..patch.num_hunks())
                    .filter_map(|h| patch.hunk(h).ok().map(|(hunk, _)| hunk.new_start())),
            );
        }
    }
    starts
}

/// The error for a `--hunk` line that starts no hunk of `path` in `diff`,
/// listing the lines that do.
fn hunk_not_found(diff: &Diff, path: &str, new_start: u32) -> GitError {
    GitError::HunkNotFound {
        path: path.to_owned(),
        new_start,
        starts: hunk_starts(diff, path),
    }
}

fn apply_one_hunk(
    repo: &Repository,
    diff: &Diff,
    path: &str,
    new_start: u32,
) -> Result<(), GitError> {
    if !hunk_starts(diff, path).contains(&new_start) {
        return Err(hunk_not_found(diff, path, new_start));
    }
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

/// Check out the local branch `name`, updating the worktree and HEAD. A safe
/// checkout errors rather than clobbering conflicting local changes.
pub(crate) fn checkout(repo: &Repository, name: &str) -> Result<(), GitError> {
    let refname = format!("refs/heads/{name}");
    let object = repo.rev_single(&refname)?;
    safe_checkout(repo, &object, "checkout")?;
    repo.set_head(&refname)?;
    Ok(())
}

/// A safe checkout of `target` that fails as git does, naming the local
/// changes and untracked files it would overwrite.
fn safe_checkout(repo: &Repository, target: &git2::Object, verb: &str) -> Result<(), GitError> {
    let hits = std::cell::RefCell::new(Vec::new());
    let mut co = CheckoutBuilder::new();
    co.safe();
    let result = repo.checkout_tree(target, Some(overwrite_notes(&mut co, &hits)));
    match result {
        Err(e) if e.code() == ErrorCode::Conflict => Err(overwritten(repo, verb, &hits.take(), e)),
        other => Ok(other?),
    }
}

/// Collect the paths a checkout refuses to overwrite into `hits`.
fn overwrite_notes<'a, 'b>(
    co: &'a mut CheckoutBuilder<'b>,
    hits: &'b std::cell::RefCell<Vec<String>>,
) -> &'a mut CheckoutBuilder<'b> {
    co.notify_on(git2::CheckoutNotificationType::CONFLICT)
        .notify(move |_, path, _, _, _| {
            if let Some(p) = path {
                hits.borrow_mut().push(p.to_string_lossy().into_owned());
            }
            true
        })
}

/// git's refusal to overwrite local changes or untracked files by `verb`
/// (checkout or merge), or `e` itself when no path was reported.
fn overwritten(repo: &Repository, verb: &str, hits: &[String], e: git2::Error) -> GitError {
    if hits.is_empty() {
        return e.into();
    }
    let index = repo.index().ok();
    let (tracked, untracked): (Vec<&String>, Vec<&String>) = hits.iter().partition(|p| {
        index
            .as_ref()
            .is_some_and(|i| i.get_path(Path::new(p.as_str()), 0).is_some())
    });
    let before = if verb == "checkout" {
        "switch branches"
    } else {
        "merge"
    };
    let list = |v: &[&String]| {
        v.iter()
            .map(|p| format!("\t{p}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut msg = Vec::new();
    if !tracked.is_empty() {
        msg.push(format!(
            "Your local changes to the following files would be overwritten by {verb}:\n{}\n\
             Please commit your changes or stash them before you {before}.",
            list(&tracked)
        ));
    }
    if !untracked.is_empty() {
        msg.push(format!(
            "The following untracked working tree files would be overwritten by {verb}:\n{}\n\
             Please move or remove them before you {before}.",
            list(&untracked)
        ));
    }
    msg.push("Aborting".to_owned());
    GitError::Conflict(msg.join("\n"))
}

/// The working-tree half of git's `reset --merge`: files that differ between
/// HEAD and `target`, or that are staged or unmerged, take `target`'s version,
/// but a file with unstaged changes is kept, and refused when `target` changes it.
fn reset_merge(repo: &Repository, target: &git2::Commit) -> Result<(), GitError> {
    let paths = |diff: Diff| -> std::collections::BTreeSet<String> {
        diff.deltas()
            .flat_map(|d| [d.old_file().path(), d.new_file().path()])
            .flatten()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    };
    let head = repo.head()?.peel_to_tree()?;
    let tree = target.tree()?;
    let index = repo.index()?;
    let unmerged: std::collections::BTreeSet<String> = index
        .conflicts()?
        .filter_map(|c| {
            let c = c.ok()?;
            let e = c.our.or(c.their).or(c.ancestor)?;
            Some(String::from_utf8_lossy(&e.path).into_owned())
        })
        .collect();
    let changed = paths(repo.diff_tree_to_tree(Some(&head), Some(&tree), None)?);
    let unstaged: std::collections::BTreeSet<String> =
        paths(repo.diff_index_to_workdir(None, None)?)
            .difference(&unmerged)
            .cloned()
            .collect();
    if let Some(p) = unstaged.intersection(&changed).next() {
        return Err(GitError::Other(format!(
            "Entry '{p}' not uptodate. Cannot merge."
        )));
    }
    let mut update = paths(repo.diff_tree_to_index(Some(&head), None, None)?);
    update.extend(changed);
    update.extend(unmerged);
    let update: Vec<String> = update.difference(&unstaged).cloned().collect();
    if update.is_empty() {
        return Ok(());
    }
    let mut checkout = CheckoutBuilder::new();
    checkout.force().update_index(false);
    for p in &update {
        checkout.path(p);
    }
    repo.checkout_tree(target.as_object(), Some(&mut checkout))?;
    let workdir = repo.workdir().unwrap_or(repo.path());
    for p in update
        .iter()
        .filter(|p| tree.get_path(Path::new(p)).is_err())
    {
        let _ = std::fs::remove_file(workdir.join(p));
    }
    Ok(())
}

/// git's `checkout -m`: switch to `target`, carrying local changes over with a
/// three-way merge of HEAD, `target` and the working tree when a plain switch
/// would overwrite them. Local changes stay unstaged and conflicts unmerged.
fn checkout_merge(
    repo: &Repository,
    target: &git2::Commit,
    label: &str,
    diff3: bool,
) -> Result<(), GitError> {
    let safe = repo.checkout_tree(target.as_object(), Some(CheckoutBuilder::new().safe()));
    match safe {
        Err(e) if e.code() == ErrorCode::Conflict => {}
        other => return Ok(other?),
    }
    let head = repo.head()?.peel_to_tree()?;
    let mut index = repo.index()?;
    index.update_all(["*"], Some(&mut crate::sparse::keep_skipped(repo, &index)))?;
    let local = repo.find_tree(index.write_tree()?)?;
    let target_tree = target.tree()?;
    let mut merged = repo.merge_trees(&head, &target_tree, &local, None)?;
    let mut checkout = CheckoutBuilder::new();
    checkout
        .force()
        .allow_conflicts(true)
        .our_label(label)
        .their_label("local");
    if diff3 {
        checkout.conflict_style_diff3(true);
    } else {
        checkout.conflict_style_merge(true);
    }
    repo.checkout_index(Some(&mut merged), Some(&mut checkout))?;
    index.read_tree(&target_tree)?;
    for c in merged.conflicts()? {
        let c = c?;
        if let Some(e) = c.our.as_ref().or(c.their.as_ref()).or(c.ancestor.as_ref()) {
            let _ = index.remove_path(Path::new(&*String::from_utf8_lossy(&e.path)));
        }
        // An entry's stage lives in bits 12-13 of its flags.
        for (stage, side) in [(1, c.ancestor), (2, c.our), (3, c.their)] {
            if let Some(mut e) = side {
                e.flags = (e.flags & !0x3000) | (stage << 12);
                index.add(&e)?;
            }
        }
    }
    index.write()?;
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
    Err(hunk_not_found(diff, path, new_start))
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
    Err(hunk_not_found(diff, path, new_start))
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

/// Drop the deletions libgit2 reports for skip-worktree files, which a
/// sparse checkout leaves out on purpose.
fn drop_skipped(repo: &Repository, files: &mut Vec<crate::FileDiff>) -> Result<(), GitError> {
    let skips = crate::sparse::skipped_paths(&repo.index()?);
    files.retain(|f| f.status != StatusCode::Deleted || !skips.contains(&f.path));
    Ok(())
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
    let skips = crate::sparse::skipped_paths(&repo.index()?);

    for entry in statuses.iter() {
        let mut status = entry.status();
        if status.contains(Status::IGNORED) {
            continue;
        }
        if status.contains(Status::WT_DELETED) && entry.path().is_ok_and(|p| skips.contains(p)) {
            status.remove(Status::WT_DELETED);
            if status.is_empty() {
                continue;
            }
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

/// A stopped rebase's progress from `.git/rebase-merge`, with todo lines'
/// commit ids abbreviated as `git status` shows them.
fn rebase_progress(repo: &Repository) -> Option<crate::RebaseProgress> {
    let dir = repo.path().join("rebase-merge");
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
    let onto = read("onto")?;
    let lines = |f: &str| -> Vec<String> {
        read(f)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| {
                let mut words: Vec<&str> = l.splitn(3, ' ').collect();
                if words.get(1).is_some_and(|w| w.len() == 40) {
                    words[1] = &words[1][..7];
                }
                words.join(" ")
            })
            .collect()
    };
    Some(crate::RebaseProgress {
        branch: read("head-name")
            .map(|h| short_ref(h.trim()).to_owned())
            .filter(|h| h != "detached HEAD"),
        onto: onto.trim().chars().take(7).collect(),
        done: lines("done"),
        todo: lines("git-rebase-todo"),
    })
}

/// The commits `revs` name, in the order a cherry-pick (oldest first) or a
/// revert (newest first) applies them. A rev is a commit or a range `A..B`.
fn pick_list(repo: &Repository, revs: &[String], revert: bool) -> Result<Vec<Oid>, GitError> {
    let commit = |rev: &str| -> Result<Oid, GitError> {
        let rev = if rev.is_empty() { "HEAD" } else { rev };
        Ok(repo.rev_single(rev)?.peel_to_commit()?.id())
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
        list.push(repo.rev_single(rev)?.peel_to_commit()?.id());
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
        opts.sign = cfg.get_string("options.gpg-sign").ok();
        opts.strategy = cfg.get_string("options.strategy").ok();
        opts.cleanup = cfg.get_string("options.default-msg-cleanup").ok();
        opts.rerere_autoupdate = cfg.get_bool("options.allow-rerere-auto").ok();
        let flag = |key: &str| cfg.get_bool(&format!("options.{key}")).unwrap_or(false);
        opts.edit = flag("edit");
        opts.signoff = flag("signoff");
        opts.allow_empty = flag("allow-empty");
        opts.ff = flag("allow-ff");
        opts.empty = if flag("drop-redundant-commits") {
            crate::EmptyCommit::Drop
        } else if flag("keep-redundant-commits") {
            crate::EmptyCommit::Keep
        } else {
            crate::EmptyCommit::Stop
        };
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
    if let Some(key) = &opts.sign {
        cfg.set_str("options.gpg-sign", key)?;
    }
    if let Some(s) = &opts.strategy {
        cfg.set_str("options.strategy", s)?;
    }
    if let Some(mode) = &opts.cleanup {
        cfg.set_str("options.default-msg-cleanup", mode)?;
    }
    if let Some(auto) = opts.rerere_autoupdate {
        cfg.set_bool("options.allow-rerere-auto", auto)?;
    }
    for (on, key) in [
        (opts.edit, "edit"),
        (opts.signoff, "signoff"),
        (opts.allow_empty, "allow-empty"),
        (opts.ff, "allow-ff"),
        (
            opts.empty == crate::EmptyCommit::Drop,
            "drop-redundant-commits",
        ),
        (
            opts.empty == crate::EmptyCommit::Keep,
            "keep-redundant-commits",
        ),
    ] {
        if on {
            cfg.set_bool(&format!("options.{key}"), true)?;
        }
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
    let start = repo.find_tree(repo.index()?.write_tree()?)?;
    let mut ours = start.clone();
    if !todo.is_empty() && !opts.no_commit && ours.id() != repo.head()?.peel_to_tree()?.id() {
        return Err(GitError::Conflict(
            "you have staged changes; commit or stash them first".into(),
        ));
    }
    let mut staged = None;
    for (i, &oid) in todo.iter().enumerate() {
        let commit = repo.find_commit(oid)?;
        let head = repo.head()?.peel_to_commit()?;
        if opts.ff && commit.parent_count() == 1 && commit.parent_id(0)? == head.id() {
            repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe()))?;
            repo.head()?
                .set_target(oid, &format!("cherry-pick: fast-forward to {oid}"))?;
            ours = commit.tree()?;
            continue;
        }
        let mut merged = pick_index(repo, &commit, &ours, opts)?;
        let mut message = pick_message(&commit, opts);
        if opts.signoff {
            message = signoff(&message, &repo.committer_from_env()?);
        }
        let subject = commit.summary().ok().flatten().unwrap_or("").to_owned();
        // With -n the index on disk still holds the starting tree.
        let on_disk = if opts.no_commit { &start } else { &ours };
        let stop = if merged.has_conflicts() {
            let labels = pick_labels(&commit, opts.revert);
            checkout_merged(repo, &mut merged, on_disk, verb, Some(&labels))?;
            let parent = match commit.parent(opts.mainline.map_or(0, |m| m as usize - 1)) {
                Ok(p) => p.tree()?,
                Err(_) => repo.find_tree(repo.treebuilder(None)?.write()?)?,
            };
            let tree = commit.tree()?;
            let (base, theirs) = if opts.revert {
                (&tree, &parent)
            } else {
                (&parent, &tree)
            };
            let mut text: String = merge_report(repo, &merged, [base, &ours, theirs], &labels[2])?
                .iter()
                .map(|l| format!("{l}\n"))
                .collect();
            let me = &labels[if opts.revert { 0 } else { 2 }];
            let short = me.split(' ').next().unwrap_or_default();
            let what = if opts.revert { "revert" } else { "apply" };
            text.push_str(&format!("error: could not {what} {short}... {subject}\n"));
            text.push_str(&conflict_advice(repo, verb, opts.no_commit));
            Some(text)
        } else {
            None
        };
        let tree = match stop {
            Some(_) => None,
            None => Some(repo.find_tree(merged.write_tree_to(repo)?)?),
        };
        let empty = tree
            .as_ref()
            .is_some_and(|t| !opts.no_commit && t.id() == ours.id());
        let was_empty =
            commit.parent_count() == 1 && commit.parent(0)?.tree_id() == commit.tree_id();
        if empty && !was_empty && opts.empty == crate::EmptyCommit::Drop {
            eprintln!("dropping {oid} {subject} -- patch contents already upstream");
            continue;
        }
        let keep = if was_empty {
            opts.allow_empty
        } else {
            opts.empty == crate::EmptyCommit::Keep
        };
        let stop = match stop {
            None if empty && !keep => Some(format!(
                "the {verb} of {} ({subject}) is now empty; run `rgit {verb} --skip`",
                short7(oid)
            )),
            stop => stop,
        };
        if let Some(mut why) = stop {
            if merged.has_conflicts() {
                message.push_str(&conflicts_hint(&merged, opts.cleanup.as_deref()));
            }
            write_pick_state(repo, &todo[i..], opts, orig_head, &message)?;
            if merged.has_conflicts() {
                why.push_str(&crate::rerere::report(repo, opts.rerere_autoupdate));
            }
            return Err(GitError::Conflict(why.trim_end().to_owned()));
        }
        let tree = tree.expect("no stop means a clean tree");
        if opts.no_commit {
            ours = tree;
            staged = Some(merged);
            continue;
        }
        if opts.edit {
            message = edit_message(repo, "COMMIT_EDITMSG", &message)?;
        }
        if opts.cleanup.is_some() {
            message = cleanup_message(&message, opts.cleanup.as_deref(), opts.edit)?;
        }
        repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe()))?;
        let committer = repo.committer_from_env()?;
        let author = if opts.revert {
            committer.clone()
        } else {
            commit.author().to_owned()
        };
        let key = crate::sign::commit_key(repo, opts.sign.as_deref(), opts.no_sign);
        crate::sign::commit(
            repo,
            Some("HEAD"),
            &author,
            &committer,
            &message,
            &tree,
            &[&head],
            key.as_deref(),
        )?;
        ours = tree;
    }
    if let Some(mut index) = staged {
        checkout_merged(repo, &mut index, &start, verb, None)?;
    }
    Ok(())
}

/// Write the merge result `index` over the index and working tree, which hold
/// `from`: only the paths that differ are touched, so other staged changes
/// stay, and a local change in one of them stops it, as git does.
pub(crate) fn checkout_merged(
    repo: &Repository,
    index: &mut git2::Index,
    from: &git2::Tree<'_>,
    verb: &str,
    labels: Option<&[String; 3]>,
) -> Result<(), GitError> {
    let mut paths = Vec::new();
    for delta in repo
        .diff_tree_to_index(Some(from), Some(index), None)?
        .deltas()
    {
        paths.extend(
            [delta.old_file().path(), delta.new_file().path()]
                .into_iter()
                .flatten()
                .map(Path::to_path_buf),
        );
    }
    for conflict in index.conflicts()? {
        let conflict = conflict?;
        for e in [conflict.ancestor, conflict.our, conflict.their]
            .into_iter()
            .flatten()
        {
            paths.push(PathBuf::from(String::from_utf8_lossy(&e.path).as_ref()));
        }
    }
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok(());
    }
    let mut opts = DiffOptions::new();
    opts.include_untracked(true).disable_pathspec_match(true);
    for p in &paths {
        opts.pathspec(p);
    }
    if repo
        .diff_index_to_workdir(None, Some(&mut opts))?
        .deltas()
        .len()
        > 0
    {
        return Err(GitError::Conflict(format!(
            "your local changes would be overwritten by {verb}; commit or stash them first"
        )));
    }
    // ponytail: zdiff3 is written as diff3; libgit2's checkout has no zdiff3.
    let diff3 = repo
        .config()
        .and_then(|c| c.get_string("merge.conflictStyle"))
        .is_ok_and(|s| s.ends_with("diff3"));
    let mut checkout = CheckoutBuilder::new();
    checkout
        .force()
        .allow_conflicts(true)
        .conflict_style_merge(!diff3)
        .conflict_style_diff3(diff3)
        .disable_pathspec_match(true);
    if let Some([base, ours, theirs]) = labels {
        checkout
            .ancestor_label(base)
            .our_label(ours)
            .their_label(theirs);
    }
    for p in &paths {
        checkout.path(p);
    }
    repo.checkout_index(Some(index), Some(&mut checkout))?;
    Ok(())
}

/// git's append_conflicts_hint: the `# Conflicts:` list MERGE_MSG ends with.
fn conflicts_hint(index: &git2::Index, cleanup: Option<&str>) -> String {
    let mut hint = String::new();
    if cleanup == Some("scissors") {
        hint.push_str("\n# ------------------------ >8 ------------------------\n#");
    }
    hint.push_str("\n# Conflicts:\n");
    let mut paths: Vec<String> = index
        .iter()
        .filter(|e| (e.flags >> 12) & 3 != 0)
        .map(|e| String::from_utf8_lossy(&e.path).into_owned())
        .collect();
    paths.dedup();
    for p in paths {
        hint.push_str(&format!("#\t{p}\n"));
    }
    hint
}

/// What git's ort says of a three-way merge, path by path: `Auto-merging`
/// for each file both sides changed, then its `CONFLICT` line if it has one.
/// `theirs_label` names their side in modify/delete conflicts.
pub(crate) fn merge_report(
    repo: &Repository,
    index: &git2::Index,
    [base, ours, theirs]: [&git2::Tree<'_>; 3],
    theirs_label: &str,
) -> Result<Vec<String>, GitError> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut conflicts: BTreeMap<String, (bool, bool, bool)> = BTreeMap::new();
    for c in index.conflicts()? {
        let c = c?;
        let Some(e) = c.our.as_ref().or(c.their.as_ref()).or(c.ancestor.as_ref()) else {
            continue;
        };
        let path = String::from_utf8_lossy(&e.path).into_owned();
        conflicts.insert(
            path,
            (c.ancestor.is_some(), c.our.is_some(), c.their.is_some()),
        );
    }
    let changed = |to: &git2::Tree| -> Result<BTreeSet<String>, GitError> {
        Ok(repo
            .diff_tree_to_tree(Some(base), Some(to), None)?
            .deltas()
            .filter_map(|d| d.new_file().path().or(d.old_file().path()))
            .map(|p| p.to_string_lossy().into_owned())
            .collect())
    };
    let blob = |t: &git2::Tree, p: &str| {
        t.get_path(Path::new(p))
            .ok()
            .filter(|e| e.kind() == Some(ObjectType::Blob))
            .map(|e| e.id())
    };
    let auto: BTreeSet<String> = changed(ours)?
        .intersection(&changed(theirs)?)
        .filter(|p| matches!((blob(ours, p), blob(theirs, p)), (Some(a), Some(b)) if a != b))
        .cloned()
        .collect();
    let paths: BTreeSet<&String> = auto.iter().chain(conflicts.keys()).collect();
    let mut out = Vec::new();
    for p in paths {
        if auto.contains(p) {
            out.push(format!("Auto-merging {p}"));
        }
        out.extend(conflicts.get(p).map(|kind| match kind {
            (false, true, true) => format!("CONFLICT (add/add): Merge conflict in {p}"),
            (_, true, true) => format!("CONFLICT (content): Merge conflict in {p}"),
            (_, false, _) => format!(
                "CONFLICT (modify/delete): {p} deleted in HEAD and modified in {theirs_label}.  \
                 Version {theirs_label} of {p} left in tree."
            ),
            (_, true, false) => format!(
                "CONFLICT (modify/delete): {p} deleted in {theirs_label} and modified in HEAD.  \
                 Version HEAD of {p} left in tree."
            ),
        }));
    }
    Ok(out)
}

/// git's hint lines (`advice.mergeConflict`) for a stopped `verb`
/// (cherry-pick, revert or rebase).
pub(crate) fn conflict_advice(repo: &Repository, verb: &str, no_commit: bool) -> String {
    if repo
        .config()
        .and_then(|c| c.get_bool("advice.mergeConflict"))
        .is_ok_and(|on| !on)
    {
        return String::new();
    }
    let text = match verb {
        _ if no_commit => "after resolving the conflicts, mark the corrected paths\nwith 'git add \
                           <paths>' or 'git rm <paths>'"
            .to_owned(),
        "rebase" => "Resolve all conflicts manually, mark them as resolved with\n\"git add/rm \
                     <conflicted_files>\", then run \"git rebase --continue\".\nYou can instead \
                     skip this commit: run \"git rebase --skip\".\nTo abort and get back to the \
                     state before \"git rebase\", run \"git rebase --abort\"."
            .to_owned(),
        v => format!(
            "After resolving the conflicts, mark them with\n\"git add/rm <pathspec>\", then \
             run\n\"git {v} --continue\".\nYou can instead skip this commit with \"git {v} \
             --skip\".\nTo abort and get back to the state before \"git {v}\",\nrun \"git {v} \
             --abort\"."
        ),
    };
    text.lines()
        .chain(["Disable this message with \"git config set advice.mergeConflict false\""])
        .map(|l| format!("hint: {l}\n"))
        .collect()
}

/// git's conflict labels for applying `commit`: base, ours and theirs.
pub(crate) fn pick_labels(commit: &git2::Commit<'_>, revert: bool) -> [String; 3] {
    let me = format!(
        "{} ({})",
        commit
            .as_object()
            .short_id()
            .ok()
            .and_then(|b| b.as_str().ok().map(str::to_owned))
            .unwrap_or_else(|| short7(commit.id())),
        commit.summary().ok().flatten().unwrap_or("")
    );
    let parent = match commit.parent_count() {
        0 => "(empty tree)".to_owned(),
        _ => format!("parent of {me}"),
    };
    match revert {
        false => [parent, "HEAD".into(), me],
        true => [me, "HEAD".into(), parent],
    }
}

/// Let the user edit `msg` in their editor (GIT_EDITOR, core.editor, VISUAL,
/// EDITOR), as git's `-e` does, through `.git/<file>`. Comment lines are
/// dropped and an empty message aborts.
pub(crate) fn edit_message(repo: &Repository, file: &str, msg: &str) -> Result<String, GitError> {
    let path = repo.path().join(file);
    std::fs::write(&path, msg)?;
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let editor = env("GIT_EDITOR")
        .or_else(|| repo.config().ok()?.get_string("core.editor").ok())
        .or_else(|| env("VISUAL"))
        .or_else(|| env("EDITOR"))
        .unwrap_or_else(|| "vi".to_owned());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor)
        .arg(&path)
        .status()
        .map_err(|e| GitError::Cli(format!("could not run the editor: {e}")))?;
    if !status.success() {
        return Err(GitError::Other(format!(
            "there was a problem with the editor '{editor}'"
        )));
    }
    let edited = strip_comments(&std::fs::read_to_string(&path)?);
    if edited.trim().is_empty() {
        return Err(GitError::Other(
            "aborting commit due to empty commit message".into(),
        ));
    }
    Ok(edited)
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
    match opts.strategy.as_deref() {
        None | Some("ort" | "recursive" | "resolve") => {}
        // `-s ours` keeps HEAD's tree, so the pick comes out empty.
        Some("ours") => {
            let mut index = git2::Index::new()?;
            index.read_tree(ours)?;
            return Ok(index);
        }
        Some(other) => {
            return Err(GitError::Other(format!(
                "unknown merge strategy '{other}'; use ort, recursive, resolve or ours"
            )));
        }
    }
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
pub(crate) fn file_favor(side: &str) -> Result<git2::FileFavor, GitError> {
    match side {
        "ours" => Ok(git2::FileFavor::Ours),
        "theirs" => Ok(git2::FileFavor::Theirs),
        _ => Err(GitError::Other(format!(
            "unknown strategy option '{side}'; use ours or theirs"
        ))),
    }
}

/// The tree to merge against when `bases` are the merge bases: the empty
/// tree, the one base's, or, like git's ort/recursive, a virtual base made by
/// merging the bases (each such merge recursive too) with any conflict left
/// in the file as markers.
pub(crate) fn merge_base_tree<'r>(
    repo: &'r Repository,
    bases: &[Oid],
) -> Result<git2::Tree<'r>, GitError> {
    let Some((first, rest)) = bases.split_first() else {
        return Ok(repo.find_tree(repo.treebuilder(None)?.write()?)?);
    };
    let mut acc = repo.find_commit(*first)?;
    for b in rest {
        let other = repo.find_commit(*b)?;
        let mut index = repo.merge_commits(&acc, &other, None)?;
        let conflicts: Vec<git2::IndexConflict> = index.conflicts()?.collect::<Result<_, _>>()?;
        for c in conflicts {
            let text = |e: &Option<git2::IndexEntry>| -> Result<Option<Vec<u8>>, GitError> {
                Ok(match e {
                    Some(e) => Some(repo.find_blob(e.id)?.content().to_vec()),
                    None => None,
                })
            };
            let Some(side) = c.our.as_ref().or(c.their.as_ref()) else {
                continue;
            };
            let path = String::from_utf8_lossy(&side.path).into_owned();
            let content = match (text(&c.our)?, text(&c.their)?) {
                (Some(o), Some(t)) => {
                    let base = text(&c.ancestor)?.unwrap_or_default();
                    fn input(t: &[u8]) -> git2::MergeFileInput<'_> {
                        let mut i = git2::MergeFileInput::new();
                        i.content(t);
                        i
                    }
                    let mut opts = git2::MergeFileOptions::new();
                    opts.our_label("Temporary merge branch 1")
                        .their_label("Temporary merge branch 2");
                    git2::merge_file(&input(&base), &input(&o), &input(&t), Some(&mut opts))?
                        .content()
                        .to_vec()
                }
                (Some(x), None) | (None, Some(x)) => x,
                (None, None) => continue,
            };
            index.conflict_remove(Path::new(&path))?;
            index_set(&mut index, &path, repo.blob(&content)?, side.mode as i32)?;
        }
        let tree = repo.find_tree(index.write_tree_to(repo)?)?;
        let sig = git2::Signature::new("rgit", "rgit", &git2::Time::new(0, 0))?;
        let id = repo.commit(
            None,
            &sig,
            &sig,
            "virtual merge base",
            &tree,
            &[&acc, &other],
        )?;
        acc = repo.find_commit(id)?;
    }
    Ok(acc.tree()?)
}

/// Every merge base of `a` and `b` (none for unrelated histories).
pub(crate) fn all_merge_bases(repo: &Repository, a: Oid, b: Oid) -> Result<Vec<Oid>, GitError> {
    match repo.merge_bases(a, b) {
        Ok(bases) => Ok(bases.iter().copied().collect()),
        Err(e) if e.code() == ErrorCode::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// The message git gives a cherry-picked or reverted commit.
fn pick_message(commit: &git2::Commit<'_>, opts: &crate::PickOptions) -> String {
    if opts.revert && opts.reference {
        let (y, m, d) = civil_date(commit.author().when());
        return format!(
            "# *** SAY WHY WE ARE REVERTING ON THE TITLE LINE ***\n\nThis reverts commit {} ({}, \
             {y}-{m:02}-{d:02}).\n",
            short7(commit.id()),
            commit.summary().ok().flatten().unwrap_or("")
        );
    }
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

/// Merge several heads at once with git's octopus strategy: each head is merged
/// in turn onto the result so far, and only the last one may leave conflicts.
/// git's words when the octopus strategy gives up; it exits 2 with them.
pub const OCTOPUS_FAILED: &str = "Merge with strategy octopus failed.";

fn octopus(
    repo: &Repository,
    revs: &[String],
    opts: &crate::MergeOptions,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let strategy = opts.strategy.as_deref().unwrap_or("octopus");
    if !matches!(strategy, "octopus" | "ours") {
        return Err(GitError::Other(
            "Not handling anything other than two heads merge.".into(),
        ));
    }
    let head = repo.head()?.peel_to_commit()?;
    let mut given = Vec::new();
    for rev in revs {
        given.push((repo.rev_single(rev)?.peel_to_commit()?, rev.as_str()));
    }
    // Like git's reduce_heads: drop heads another head (or HEAD) contains; HEAD
    // is a parent only when no head contains it, or with --no-ff.
    let contains = |tip: Oid, c: Oid| tip == c || repo.graph_descendant_of(tip, c).unwrap_or(false);
    let mut kept: Vec<(&git2::Commit, &str)> = Vec::new();
    for (i, (c, name)) in given.iter().enumerate() {
        let redundant = contains(head.id(), c.id())
            || given[..i].iter().any(|(o, _)| o.id() == c.id())
            || given
                .iter()
                .any(|(o, _)| o.id() != c.id() && contains(o.id(), c.id()));
        if !redundant {
            kept.push((c, name));
        }
    }
    let subsumed = kept.iter().any(|(c, _)| contains(c.id(), head.id()));
    match kept.as_slice() {
        [] => {
            report(OpProgress::Line("Already up to date.".to_owned()));
            return Ok(());
        }
        [(one, name)] => {
            return merge_commit(repo, one, name, opts, report);
        }
        _ => {}
    }
    if opts.ff_only {
        return Err(GitError::Other(
            "Not possible to fast-forward, aborting.".into(),
        ));
    }
    for (c, _) in &kept {
        if !opts.allow_unrelated && repo.merge_base(head.id(), c.id()).is_err() {
            return Err(GitError::Other(
                "refusing to merge unrelated histories".into(),
            ));
        }
    }
    let head_tree = head.tree()?;
    let staged = repo.diff_tree_to_index(Some(&head_tree), None, None)?;
    if staged.deltas().len() > 0 {
        let files: Vec<String> = staged
            .deltas()
            .filter_map(|d| Some(format!("\t{}", d.new_file().path()?.display())))
            .collect();
        return Err(GitError::Conflict(format!(
            "Your local changes to the following files would be overwritten by merge:\n{}",
            files.join("\n")
        )));
    }
    // git's git-merge-octopus.sh, step for step: read-tree with every merge
    // base, then merge-one-file on what is left unmerged.
    let git_dir = repo.path();
    let lines = |text: &str| {
        for l in text.lines() {
            report(OpProgress::Line(l.to_owned()));
        }
    };
    let mut mrc: Vec<Oid> = vec![head.id()];
    let mut mrt = crate::write_tree(git_dir, false, None)?;
    let mut non_ff = false;
    let mut failure = false;
    for (c, name) in kept.iter().filter(|_| strategy == "octopus") {
        if failure {
            report(OpProgress::Line("Automated merge did not work.".to_owned()));
            report(OpProgress::Line(
                "Should not be doing an octopus.".to_owned(),
            ));
            // git's merge restores the state from before a strategy that gave up.
            let reset = crate::ReadTreeOpts {
                reset: true,
                update: true,
                ..Default::default()
            };
            crate::read_tree(git_dir, &[head.id().to_string()], &reset)?;
            sync_index(repo)?;
            return Err(GitError::Conflict(OCTOPUS_FAILED.into()));
        }
        let common: Vec<Oid> = match repo.merge_bases_many(&[&[c.id()], &mrc[..]].concat()) {
            Ok(b) => b.iter().copied().collect(),
            Err(e) if e.code() == ErrorCode::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        if common.contains(&c.id()) {
            report(OpProgress::Line(format!("Already up to date with {name}")));
            continue;
        }
        let update = crate::ReadTreeOpts {
            merge: true,
            update: true,
            ..Default::default()
        };
        if !non_ff && common == mrc {
            report(OpProgress::Line(format!("Fast-forwarding to: {name}")));
            crate::read_tree(
                git_dir,
                &[head.id().to_string(), c.id().to_string()],
                &update,
            )?;
            mrc = vec![c.id()];
            mrt = crate::write_tree(git_dir, false, None)?;
            continue;
        }
        non_ff = true;
        report(OpProgress::Line(format!("Trying simple merge with {name}")));
        let mut trees: Vec<String> = common.iter().map(Oid::to_string).collect();
        trees.push(mrt.clone());
        trees.push(c.id().to_string());
        let aggressive = crate::ReadTreeOpts {
            aggressive: true,
            ..update
        };
        crate::read_tree(git_dir, &trees, &aggressive)?;
        let next = match crate::write_tree(git_dir, false, None) {
            Ok(tree) => tree,
            Err(_) => {
                report(OpProgress::Line(
                    "Simple merge did not work, trying automatic merge.".to_owned(),
                ));
                for args in crate::unmerged_stages(git_dir)? {
                    let r = crate::merge_one_file(git_dir, &args)?;
                    lines(&r.out);
                    lines(&r.err);
                    failure |= r.failed;
                }
                if failure {
                    report(OpProgress::Line("fatal: merge program failed".to_owned()));
                }
                crate::write_tree(git_dir, false, None).unwrap_or_default()
            }
        };
        mrc.push(c.id());
        mrt = next;
    }
    sync_index(repo)?;
    let heads: String = kept.iter().map(|(c, _)| format!("{}\n", c.id())).collect();
    std::fs::write(repo.path().join("MERGE_HEAD"), heads)?;
    let mode = if opts.no_ff { "no-ff" } else { "" };
    std::fs::write(repo.path().join("MERGE_MODE"), mode)?;
    conclude_merge(
        repo,
        &head,
        &kept,
        !subsumed || opts.no_ff,
        opts,
        strategy,
        report,
    )
}

/// Whether `commit` touched `path`; when it renamed the file, `path` moves to
/// the old name for older commits, as `git log --follow` does.
pub(crate) fn follow_path(repo: &Repository, commit: &git2::Commit<'_>, path: &mut String) -> bool {
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

/// git's pickaxe: whether `commit`'s own diff (merges have none) changes how
/// often `opts.occurrences` appears in a file (-S), or adds or removes a line
/// matching `changes` (-G).
pub(crate) fn pickaxe(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    opts: &crate::LogOptions,
    changes: Option<&regex::Regex>,
) -> Result<bool, GitError> {
    if commit.parent_count() > 1 {
        return Ok(false);
    }
    let old = commit.parent(0).ok().map(|p| p.tree()).transpose()?;
    let mut dopts = DiffOptions::new();
    crate::pathspec::limit_diff(&mut dopts, &opts.paths)?;
    let mut diff = repo.diff_tree_to_tree(old.as_ref(), Some(&commit.tree()?), Some(&mut dopts))?;
    diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
    for (i, delta) in diff.deltas().enumerate() {
        if let Some(needle) = &opts.occurrences {
            let count = |f: git2::DiffFile| {
                repo.find_blob(f.id()).map_or(0, |b| {
                    String::from_utf8_lossy(b.content())
                        .matches(needle.as_str())
                        .count()
                })
            };
            if count(delta.old_file()) != count(delta.new_file()) {
                return Ok(true);
            }
        }
        if let Some(re) = changes
            && let Some(patch) = Patch::from_diff(&diff, i)?
        {
            for h in 0..patch.num_hunks() {
                for l in 0..patch.num_lines_in_hunk(h)? {
                    let line = patch.line_in_hunk(h, l)?;
                    if matches!(line.origin(), '+' | '-')
                        && re.is_match(&String::from_utf8_lossy(line.content()))
                    {
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(false)
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
/// Apply stash `index`, dropping it after (`drop`) only when it applied
/// cleanly; conflicts go through rerere and keep the entry, as git does.
fn apply_stash(
    repo: &mut Repository,
    index: usize,
    opts: Option<&mut git2::StashApplyOptions>,
    drop: bool,
) -> Result<(), GitError> {
    repo.stash_apply(index, opts)?;
    if repo.index()?.has_conflicts() {
        let said = crate::rerere::report(repo, None);
        return Err(GitError::Conflict(format!(
            "{said}{}",
            if drop {
                "The stash entry is kept in case you need it again."
            } else {
                "the stash applied with conflicts; resolve them and `rgit add` the files"
            }
        )));
    }
    if drop {
        repo.stash_drop(index)?;
    }
    Ok(())
}

fn stash_flags(include_untracked: bool) -> Option<git2::StashFlags> {
    if include_untracked {
        Some(git2::StashFlags::INCLUDE_UNTRACKED)
    } else {
        Some(git2::StashFlags::DEFAULT)
    }
}

/// A signature time in `git log`'s default form: `Thu Sep 24 23:24:06 2026 -0500`.
fn git_date(t: git2::Time) -> String {
    let d = DateParts::of(t);
    format!(
        "{} {} {} {:02}:{:02}:{:02} {} {}",
        d.weekday, d.month, d.day, d.hour, d.minute, d.second, d.year, d.zone
    )
}

/// A signature time in one of git's `--date` styles: default, iso,
/// iso-strict or short.
pub(crate) fn format_git_date(t: git2::Time, style: &str) -> String {
    let d = DateParts::of(t);
    let m = MONTHS.iter().position(|m| *m == d.month).unwrap_or(0) + 1;
    match style {
        "iso" => format!(
            "{}-{m:02}-{:02} {:02}:{:02}:{:02} {}",
            d.year, d.day, d.hour, d.minute, d.second, d.zone
        ),
        "iso-strict" => format!(
            "{}-{m:02}-{:02}T{:02}:{:02}:{:02}{}:{}",
            d.year,
            d.day,
            d.hour,
            d.minute,
            d.second,
            &d.zone[..3],
            &d.zone[3..]
        ),
        "short" => format!("{}-{m:02}-{:02}", d.year, d.day),
        _ => git_date(t),
    }
}

/// A signature time as an email `Date:` (RFC 2822): `Sat, 3 Jan 2026 03:04:05 -0500`.
pub(crate) fn rfc2822_date(t: git2::Time) -> String {
    let d = DateParts::of(t);
    format!(
        "{}, {} {} {} {:02}:{:02}:{:02} {}",
        d.weekday, d.day, d.month, d.year, d.hour, d.minute, d.second, d.zone
    )
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

struct DateParts {
    weekday: &'static str,
    month: &'static str,
    day: i64,
    year: i64,
    hour: i64,
    minute: i64,
    second: i64,
    zone: String,
}

impl DateParts {
    fn of(t: git2::Time) -> Self {
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
        const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
        DateParts {
            weekday: WD[days.rem_euclid(7) as usize],
            month: MONTHS[(month - 1) as usize],
            day,
            year: yoe + era * 400 + i64::from(month <= 2),
            hour: secs / 3600,
            minute: secs % 3600 / 60,
            second: secs % 60,
            zone: format!(
                "{}{:02}{:02}",
                if off < 0 { '-' } else { '+' },
                off.abs() / 60,
                off.abs() % 60
            ),
        }
    }
}

/// A signature time's local (year, month, day).
fn civil_date(t: git2::Time) -> (i64, i64, i64) {
    let d = DateParts::of(t);
    let month = MONTHS.iter().position(|m| *m == d.month).unwrap_or(0) as i64 + 1;
    (d.year, month, d.day)
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
/// commit id for a repository/worktree. The id is `None` on an unborn branch.
fn worktree_head(repo: &Repository) -> (Option<String>, Option<String>) {
    let Ok(head) = repo.head() else {
        let unborn = repo.find_reference("HEAD").ok().and_then(|h| {
            h.symbolic_target()
                .ok()
                .flatten()
                .map(|t| short_ref(t).to_owned())
        });
        return (unborn, None);
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

/// The full HEAD commit id, `None` on an unborn branch.
fn head_oid(repo: &Repository) -> Option<String> {
    Some(repo.head().ok()?.peel_to_commit().ok()?.id().to_string())
}

/// Whether a repository/worktree has any uncommitted change: tracked edits,
/// staged files, or new (untracked) files. Ignored files do not count.
/// Create branch `name` at `start` for a new worktree, tracking `start` when
/// it is a remote-tracking branch.
fn new_worktree_branch(
    repo: &Repository,
    name: &str,
    start: &str,
    reset: bool,
    track: Option<bool>,
) -> Result<(), GitError> {
    let commit = repo.rev_single(start)?.peel_to_commit()?;
    let mut branch = repo.branch(name, &commit, reset)?;
    if track.unwrap_or_else(|| repo.find_branch(start, BranchType::Remote).is_ok()) {
        branch.set_upstream(Some(start))?;
    }
    Ok(())
}

/// The path of the worktree, main or linked, that has `refname` checked out.
pub(crate) fn checked_out_at(repo: &Repository, refname: &str) -> Result<Option<String>, GitError> {
    let on = |r: &Repository| {
        r.find_reference("HEAD")
            .ok()
            .and_then(|h| h.symbolic_target().ok().flatten().map(str::to_owned))
            .as_deref()
            == Some(refname)
    };
    let main = Repository::open(repo.commondir())?;
    if let Some(dir) = main.workdir().filter(|_| on(&main)) {
        return Ok(Some(dir.to_string_lossy().trim_end_matches('/').to_owned()));
    }
    for name in main.worktrees()?.iter().filter_map(|n| n.ok().flatten()) {
        let wt = main.find_worktree(name)?;
        if Repository::open_from_worktree(&wt).is_ok_and(|r| on(&r)) {
            return Ok(Some(wt.path().to_string_lossy().into_owned()));
        }
    }
    Ok(None)
}

/// The last folder of a worktree path, which git names the worktree after.
fn worktree_base_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "worktree".to_owned())
}

/// A linked worktree by name, or by its path as git takes it.
fn find_worktree(repo: &Repository, worktree: &str) -> Result<git2::Worktree, GitError> {
    if let Ok(wt) = repo.find_worktree(worktree) {
        return Ok(wt);
    }
    let want = std::fs::canonicalize(worktree).ok();
    for name in repo.worktrees()?.iter().filter_map(|n| n.ok().flatten()) {
        let wt = repo.find_worktree(name)?;
        if want.is_some() && std::fs::canonicalize(wt.path()).ok() == want {
            return Ok(wt);
        }
    }
    Err(GitError::Other(format!("'{worktree}' is not a worktree")))
}

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
fn extract_with_renames(repo: &Repository, mut diff: Diff) -> Result<Vec<FileDiff>, GitError> {
    let mut fopts = DiffFindOptions::new();
    fopts.renames(true);
    diff.find_similar(Some(&mut fopts))?;
    extract(repo, &diff)
}

fn extract(repo: &Repository, diff: &Diff) -> Result<Vec<FileDiff>, GitError> {
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
        let mut file = match Patch::from_diff(diff, idx)? {
            Some(mut patch) => patch_file(&mut patch)?,
            None => FileDiff {
                path: String::new(),
                old_path: None,
                status: StatusCode::Modified,
                hunks: Vec::new(),
                binary: delta.flags().is_binary(),
                header: String::new(),
                similarity: 0,
                sizes: (delta.old_file().size(), delta.new_file().size()),
                modes: (
                    u32::from(delta.old_file().mode()),
                    u32::from(delta.new_file().mode()),
                ),
                ids: (
                    delta.old_file().id().to_string(),
                    delta.new_file().id().to_string(),
                ),
                loaded: true,
            },
        };
        if old_path.is_some() {
            file.similarity = similarity(repo, &delta);
        }
        // libgit2 scores renames its own way; print git's score.
        if let Some(start) = file.header.find("similarity index ") {
            let end = start + file.header[start..].find('\n').unwrap_or(0);
            let line = format!("similarity index {}%", file.similarity);
            file.header.replace_range(start..end, &line);
        }
        file.path = path;
        file.old_path = old_path;
        file.status = delta_status(delta.status());
        files.push(file);
    }

    Ok(files)
}

/// The libgit2 side of a command's [`crate::xdiff::DiffTweaks`].
fn tweak_options(opts: &mut DiffOptions, t: &crate::xdiff::DiffTweaks) {
    use crate::xdiff::Algorithm;
    opts.patience(t.algorithm == Algorithm::Patience)
        .minimal(t.algorithm == Algorithm::Minimal)
        .show_binary(t.binary);
    if let Some(n) = t.inter_hunk {
        opts.interhunk_lines(n);
    }
    if t.full_index {
        opts.id_abbrev(40);
    }
    if let Some(p) = &t.src_prefix {
        opts.old_prefix(p.as_str());
    }
    if let Some(p) = &t.dst_prefix {
        opts.new_prefix(p.as_str());
    }
}

/// A patch as a [`FileDiff`], its path and status left for the caller.
fn patch_file(patch: &mut Patch) -> Result<FileDiff, GitError> {
    let text = patch.to_buf()?;
    let text = String::from_utf8_lossy(&text);
    let end = text.find("\n@@ ").map_or(text.len(), |i| i + 1);
    let mut hunks = Vec::new();
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
    let delta = patch.delta();
    Ok(FileDiff {
        path: String::new(),
        old_path: None,
        status: StatusCode::Modified,
        hunks,
        binary: delta.flags().is_binary(),
        header: text[..end].to_owned(),
        similarity: 0,
        sizes: (delta.old_file().size(), delta.new_file().size()),
        modes: (
            u32::from(delta.old_file().mode()),
            u32::from(delta.new_file().mode()),
        ),
        ids: (
            delta.old_file().id().to_string(),
            delta.new_file().id().to_string(),
        ),
        loaded: true,
    })
}

/// `git diff --no-index`: two files on disk, named as given.
pub fn diff_no_index(
    old: &Path,
    new: &Path,
    spec: &crate::DiffSpec,
) -> Result<Vec<FileDiff>, GitError> {
    let (a, b) = (std::fs::read(old)?, std::fs::read(new)?);
    let mut opts = DiffOptions::new();
    if let Some(n) = spec.context {
        opts.context_lines(n);
    }
    opts.ignore_whitespace(spec.ignore_all_space)
        .ignore_whitespace_change(spec.ignore_space_change)
        .indent_heuristic(true)
        .reverse(spec.reverse);
    let mut patch = Patch::from_buffers(&a, Some(old), &b, Some(new), Some(&mut opts))?;
    if a == b {
        return Ok(Vec::new());
    }
    let mut file = patch_file(&mut patch)?;
    let (old, new) = (old.to_string_lossy(), new.to_string_lossy());
    file.path = new.clone().into_owned();
    file.old_path = (old != new).then(|| old.into_owned());
    Ok(vec![file])
}

/// git's rename score for a delta in percent (diffcore-delta's span hashing):
/// the share of the larger file's bytes that both sides have in common.
pub(crate) fn similarity(repo: &Repository, delta: &git2::DiffDelta) -> u16 {
    let (old, new) = (delta.old_file(), delta.new_file());
    if old.id() == new.id() && !old.id().is_zero() {
        return 100;
    }
    let read = |f: &git2::DiffFile| -> Vec<u8> {
        if !f.id().is_zero()
            && let Ok(blob) = repo.find_blob(f.id())
        {
            return blob.content().to_vec();
        }
        f.path()
            .zip(repo.workdir())
            .and_then(|(p, w)| std::fs::read(w.join(p)).ok())
            .unwrap_or_default()
    };
    let (a, b) = (read(&old), read(&new));
    let max = a.len().max(b.len()) as u64;
    if max == 0 {
        return 100;
    }
    let (sa, sb) = (span_hashes(&a), span_hashes(&b));
    let copied: u64 = sa
        .iter()
        .map(|(h, n)| sb.get(h).map_or(0, |m| (*n).min(*m)))
        .sum();
    (copied * 60000 / max * 100 / 60000) as u16
}

pub(crate) fn span_hashes(data: &[u8]) -> std::collections::HashMap<u32, u64> {
    let mut out = std::collections::HashMap::new();
    let text = !data.contains(&0);
    let (mut a1, mut a2, mut n) = (0u32, 0u32, 0u64);
    let mut add = |a1: u32, a2: u32, n: u64| {
        let h = a1.wrapping_add(a2.wrapping_mul(0x61)) % 107_927;
        *out.entry(h).or_insert(0) += n;
    };
    for (i, &c) in data.iter().enumerate() {
        if text && c == b'\r' && data.get(i + 1) == Some(&b'\n') {
            continue;
        }
        let old = a1;
        a1 = (a1 << 7) ^ (a2 >> 25);
        a2 = (a2 << 7) ^ (old >> 25);
        a1 = a1.wrapping_add(u32::from(c));
        n += 1;
        if n < 64 && c != b'\n' {
            continue;
        }
        add(a1, a2, n);
        (a1, a2, n) = (0, 0, 0);
    }
    if n > 0 {
        add(a1, a2, n);
    }
    out
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
