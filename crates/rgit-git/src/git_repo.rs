use std::path::{Path, PathBuf};
use std::sync::Mutex;

use git2::build::CheckoutBuilder;
use git2::{
    ApplyLocation, ApplyOptions, BranchType, Cred, CredentialType, Delta, Diff, DiffFindOptions,
    DiffOptions, ErrorCode, FetchOptions, ObjectType, Oid, Patch, Pathspec, PathspecFlags,
    PushOptions, RemoteCallbacks, Repository, ResetType, Status, StatusOptions,
};

use crate::model::{CommitOptions, ConfigScope, RepoState, ResetMode};

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
        if multi_url(&repo, &remote_name, true) {
            let mut cmd = vec!["push", remote_name.as_str()];
            cmd.extend(refspecs.iter().map(String::as_str));
            self.run_git(&cmd, &[])?;
            return Ok(());
        }
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

    /// Run a `git rebase` step. On the terminal (`tty`) git gets stdio for its
    /// editors; otherwise the todo and messages are taken as they are. A stop
    /// is reported with git's own lines, without its progress and hints.
    fn rebase_git(&self, args: &[&str], tty: bool) -> Result<String, GitError> {
        let result = if tty {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.workdir)
                .status()
                .map_err(|e| GitError::Cli(format!("could not run git: {e}")))?;
            if status.success() {
                Ok(String::new())
            } else {
                Err(GitError::Cli("the rebase stopped or was aborted".into()))
            }
        } else {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.workdir)
                .env("GIT_SEQUENCE_EDITOR", "true")
                .env("GIT_EDITOR", "true")
                .output()
                .map_err(|e| GitError::Cli(format!("could not run git: {e}")))?;
            let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
            if out.status.success() {
                Ok(stdout)
            } else {
                // git reports CONFLICT lines on stdout and the stop on stderr.
                Err(GitError::Cli(format!(
                    "{stdout}\n{}",
                    String::from_utf8_lossy(&out.stderr)
                )))
            }
        };
        let stopped = self.repo.lock().expect("repo mutex").state() != git2::RepositoryState::Clean;
        match result {
            Err(GitError::Cli(msg)) if stopped => {
                let why: Vec<&str> = msg
                    .split(['\r', '\n'])
                    .map(str::trim_end)
                    .filter(|l| {
                        !l.is_empty()
                            && !l.starts_with("hint:")
                            && !l.starts_with("Rebasing (")
                            && !l.starts_with("Could not apply ")
                    })
                    .collect();
                Err(GitError::Conflict(format!(
                    "{}\nresolve, then run `rgit rebase --continue` (or --skip / --abort)",
                    why.join("\n")
                )))
            }
            Err(GitError::Cli(msg)) => Err(GitError::Cli(
                msg.split(['\r', '\n'])
                    .filter(|l| !l.is_empty() && !l.starts_with("Rebasing ("))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            other => other,
        }
    }

    /// Fetch one remote natively, or through git for what libgit2 cannot do:
    /// deepening by a count or a date, and shallow fetches over its local
    /// transport.
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
        let local = repo.find_remote(name)?.url().is_ok_and(is_local_url);
        if !(args.deepen > 0
            || args.shallow_since.is_some()
            || (local && (args.depth > 0 || args.unshallow))
            || multi_url(repo, name, false))
        {
            return do_fetch(repo, name, refspecs, args, report, cred);
        }
        let mut cmd = vec!["fetch".to_owned()];
        if args.depth > 0 {
            cmd.push(format!("--depth={}", args.depth));
        }
        if args.deepen > 0 {
            cmd.push(format!("--deepen={}", args.deepen));
        }
        if let Some(date) = &args.shallow_since {
            cmd.push(format!("--shallow-since={date}"));
        }
        for (on, flag) in [
            (args.unshallow, "--unshallow"),
            (args.prune, "--prune"),
            (args.prune_tags, "--prune-tags"),
            (args.tags, "--tags"),
            (args.no_tags, "--no-tags"),
            (args.force, "--force"),
        ] {
            if on {
                cmd.push(flag.to_owned());
            }
        }
        match &args.refmap {
            Some(map) if map.is_empty() => cmd.push("--refmap=".to_owned()),
            Some(map) => cmd.extend(map.iter().map(|m| format!("--refmap={m}"))),
            None => {}
        }
        cmd.push(name.to_owned());
        cmd.extend(refspecs.iter().cloned());
        let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
        self.run_git(&argv, &[])?;
        Ok(())
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
            rebase: rebase_progress(&repo),
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
                let sig = repo.signature()?;
                let mut flags = stash_flags(include_untracked || all).unwrap_or_default();
                flags.set(git2::StashFlags::INCLUDE_IGNORED, all);
                flags.set(git2::StashFlags::KEEP_INDEX, keep_index);
                repo.stash_save2(&sig, message, Some(flags))?;
                return Ok(stash_saved_line(&repo));
            }
            // libgit2 can stash a pathspec, but git2 cannot name such a stash.
            let mut args = vec!["stash", "push"];
            args.extend(include_untracked.then_some("-u"));
            args.extend(all.then_some("-a"));
            args.extend(keep_index.then_some("-k"));
            if let Some(m) = message {
                args.extend(["-m", m]);
            }
            args.push("--");
            args.extend(paths.iter().map(String::as_str));
            let out = self.run_git(&args, &[])?;
            sync_index(&self.repo.lock().expect("repo mutex"))?;
            Ok(out)
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
            if drop {
                repo.stash_pop(index, Some(&mut opts))?;
            } else {
                repo.stash_apply(index, Some(&mut opts))?;
            }
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

    fn stash_push_part(
        &self,
        message: Option<&str>,
        hunks: Option<&[(String, u32)]>,
        keep_index: bool,
        paths: &[String],
    ) -> Result<String, GitError> {
        self.logged("stash", || {
            let repo = self.repo.lock().expect("repo mutex");
            sync_index(&repo)?;
            let head = repo.head()?.peel_to_commit()?;
            let head_tree = head.tree()?;
            let index_tree = repo.find_tree(repo.index()?.write_tree()?)?;
            let tree = match hunks {
                None => index_tree,
                Some(hunks) => {
                    let mut opts = DiffOptions::new();
                    for p in paths {
                        opts.pathspec(p);
                    }
                    let diff =
                        repo.diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut opts))?;
                    let path = std::cell::RefCell::new(String::new());
                    let mut apply = ApplyOptions::new();
                    apply.delta_callback(|d| {
                        let p = d.and_then(|d| d.new_file().path().or(d.old_file().path()));
                        *path.borrow_mut() = p
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        hunks.iter().any(|(h, _)| *h == *path.borrow())
                    });
                    apply.hunk_callback(|h| {
                        h.is_some_and(|h| {
                            hunks
                                .iter()
                                .any(|(p, start)| *p == *path.borrow() && *start == h.new_start())
                        })
                    });
                    let mut picked = repo.apply_to_tree(&head_tree, &diff, Some(&mut apply))?;
                    repo.find_tree(picked.write_tree_to(&repo)?)?
                }
            };
            if tree.id() == head_tree.id() {
                return Err(GitError::Other(
                    match hunks {
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
            if hunks.is_none() {
                repo.apply(&undo, ApplyLocation::Both, None)?;
            } else {
                repo.apply(&undo, ApplyLocation::WorkDir, None)?;
                if !keep_index {
                    let all = [".".to_owned()];
                    let paths = if paths.is_empty() { &all[..] } else { paths };
                    repo.reset_default(Some(head.as_object()), paths)?;
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
            let commit = repo.revparse_single(rev)?.peel_to_commit()?;
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
        let path = root_dot(path);
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
        let path = root_dot(path);
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

    fn add(&self, paths: &[String], update: bool, force: bool) -> Result<(), GitError> {
        let paths = &root_dots(paths);
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        if !force {
            for p in paths {
                no_match(&repo, p)?;
            }
        }
        let mut index = repo.index()?;
        if !update {
            let flags = if force {
                git2::IndexAddOption::FORCE
            } else {
                git2::IndexAddOption::CHECK_PATHSPEC
            };
            index.add_all(paths, flags, None)?;
        }
        index.update_all(paths, None)?;
        index.write()?;
        Ok(())
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
                .map(|s| repo.revparse_single(s))
                .transpose()?;
            let tree = target.as_ref().map(|t| t.peel_to_tree()).transpose()?;
            // checkout matches a revision's paths in it alone, as git does.
            let index = repo.index()?;
            let index = (!overlay || tree.is_none()).then_some(&index);
            for p in paths {
                known(index, tree.as_ref(), p)?;
            }
            // Overlay: only the source's own files, so the rest stay put.
            let paths: Vec<String> = match &tree {
                Some(tree) if overlay => Pathspec::new(paths)?
                    .match_tree(tree, PathspecFlags::DEFAULT)?
                    .entries()
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .collect(),
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
                        .filter(|p| spec.matches_path(Path::new(p), PathspecFlags::DEFAULT))
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
            checkout
                .force()
                .disable_pathspec_match(overlay && tree.is_some());
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
                .revparse_single(if branch { &refname } else { rev })?
                .peel_to_commit()?;
            match mode {
                crate::CheckoutMode::Safe => {
                    repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe()))?
                }
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
                Some(rev) => repo.revparse_single(rev)?.peel_to_tree()?,
                None => repo.find_tree(repo.treebuilder(None)?.write()?)?,
            };
            repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe()))?;
            repo.set_head(&refname)?;
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
        self.rebase_git(&["rebase", "--continue"], false).map(drop)
    }

    fn rebase_skip(&self) -> Result<(), GitError> {
        self.rebase_git(&["rebase", "--skip"], false).map(drop)
    }

    fn rebase_quit(&self) -> Result<(), GitError> {
        self.run_git(&["rebase", "--quit"], &[]).map(drop)
    }

    fn rebase_edit_todo(&self) -> Result<(), GitError> {
        self.rebase_git(&["rebase", "--edit-todo"], true).map(drop)
    }

    fn rebase_with(
        &self,
        upstream: Option<&str>,
        opts: &crate::RebaseOptions,
    ) -> Result<String, GitError> {
        // libgit2's rebase has no todo list, so these run on git's sequencer.
        let mut args = vec!["rebase"];
        args.extend(opts.flags.iter().map(String::as_str));
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
        // `<upstream> <branch>` needs an upstream; --root takes the branch alone.
        if opts.branch.is_some() && upstream.is_none() && !opts.root {
            args.push("@{upstream}");
        }
        args.extend(upstream);
        args.extend(opts.branch.as_deref());
        self.logged("rebase", || self.rebase_git(&args, opts.interactive))
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
            for path in paths {
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
                return Err(GitError::Other(format!(
                    "not under version control: {from}"
                )));
            }
            let dest = self.workdir.join(&to);
            if dest.exists() && (!force || dest.is_dir()) {
                return Err(GitError::Other(format!(
                    "destination {to} already exists; use --force to overwrite"
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
            .filter(|p| spec.matches_path(Path::new(p), PathspecFlags::DEFAULT))
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
                if !spec.matches_path(Path::new(&path), PathspecFlags::DEFAULT) {
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
        let target = repo.revparse_single(rev)?.peel(ObjectType::Commit)?;
        repo.reset_default(Some(&target), root_dots(paths))?;
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
                let committer = repo.signature()?;
                let author = if state.opts.revert {
                    committer.clone()
                } else {
                    source.author().to_owned()
                };
                repo.commit(Some("HEAD"), &author, &committer, &message, &tree, &[&head])?;
            }
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
        if revs.len() > 1 {
            return self.logged("merge", || octopus(self, revs, opts, report));
        }
        let rev = revs.first().map(String::as_str).unwrap_or("HEAD");
        self.logged("merge", || {
            let repo = self.repo.lock().expect("repo mutex");
            let source = repo.revparse_single(rev)?.peel_to_commit()?;
            let before = repo.head()?.peel_to_tree()?;
            merge_commit(&repo, &source, rev, opts, report)?;
            let after = repo.head()?.peel_to_tree()?;
            if opts.stat && after.id() != before.id() {
                let stats = repo
                    .diff_tree_to_tree(Some(&before), Some(&after), None)?
                    .stats()?;
                let format = git2::DiffStatsFormat::FULL | git2::DiffStatsFormat::INCLUDE_SUMMARY;
                let buf = stats.to_buf(format, 80)?;
                for line in String::from_utf8_lossy(&buf).lines() {
                    report(OpProgress::Line(line.to_owned()));
                }
            }
            Ok(())
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
            Ok(())
        })
    }

    fn merge_quit(&self) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        for file in ["MERGE_HEAD", "MERGE_MODE", "MERGE_MSG", "AUTO_MERGE"] {
            let _ = std::fs::remove_file(repo.path().join(file));
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
        let target = repo.revparse_single(rev)?;
        if message.trim().is_empty() {
            repo.tag_lightweight(name, &target, force)?;
        } else {
            let sig = repo.signature()?;
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
        if let Some(key) = sign {
            // libgit2 cannot sign; git runs gpg (or ssh, per gpg.format).
            let message = message.unwrap_or_default();
            let mut args = vec!["tag", "--cleanup=verbatim", "-m", &message];
            if key.is_empty() {
                args.push("-s");
            } else {
                args.extend(["-u", key]);
            }
            if force {
                args.push("-f");
            }
            args.extend([name, rev]);
            self.run_git(&args, &[])?;
            return Ok(());
        }
        let repo = self.repo.lock().expect("repo mutex");
        let target = repo.revparse_single(rev)?;
        match message {
            None => repo.tag_lightweight(name, &target, force)?,
            Some(m) => repo.tag(name, &target, &repo.signature()?, &m, force)?,
        };
        Ok(())
    }

    fn verify_tags(&self, names: &[String]) -> Result<String, GitError> {
        // libgit2 cannot check signatures; git runs gpg, reporting on stderr.
        let out = std::process::Command::new("git")
            .args(["tag", "-v"])
            .args(names)
            .current_dir(&self.workdir)
            .output()
            .map_err(|e| GitError::Cli(format!("could not run git: {e}")))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.success() {
            Ok(text.trim_end().to_owned())
        } else {
            Err(GitError::Cli(text.trim_end().to_owned()))
        }
    }

    fn is_ancestor(&self, ancestor: &str, rev: &str) -> Result<bool, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let a = repo.revparse_single(ancestor)?.peel_to_commit()?.id();
        let r = repo.revparse_single(rev)?.peel_to_commit()?.id();
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
            let cred = cred_guard.as_deref();
            let tracking = |repo: &Repository| -> Result<Vec<String>, GitError> {
                let mut out = Vec::new();
                for b in repo.branches(Some(BranchType::Remote))? {
                    if let Some(n) = b?.0.name()? {
                        out.push(n.to_owned());
                    }
                }
                Ok(out)
            };
            let before = tracking(&repo)?;
            if multi_url(&repo, name, false) {
                self.run_git(&["remote", "prune", name], &[])?;
            } else {
                let rejected = std::sync::atomic::AtomicBool::new(false);
                let mut remote = repo.find_remote(name)?;
                let mut conn = remote.connect_auth(
                    git2::Direction::Fetch,
                    Some(remote_callbacks(&|_| {}, &rejected, cred)),
                    None,
                )?;
                conn.remote()
                    .prune(Some(remote_callbacks(&|_| {}, &rejected, cred)))?;
            }
            let after = tracking(&repo)?;
            Ok(before.into_iter().filter(|b| !after.contains(b)).collect())
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
        let heads = conn
            .list()?
            .iter()
            .map(|h| (h.name().to_owned(), h.oid().to_string()))
            .collect();
        let head = conn
            .default_branch()
            .ok()
            .and_then(|b| b.as_str().ok().map(|s| short_ref(s).to_owned()));
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
                    .revparse_single(commitish.unwrap_or("HEAD"))?
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
            log.append(commit.id(), &repo.signature()?, Some(&msg))?;
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
            let commit = repo.revparse_single(start)?.peel_to_commit()?;
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
            let commit = repo.revparse_single(start)?.peel_to_commit()?;
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
            self.fetch_remote(&repo, name, refspecs, args, report, cred)?;
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
        if rebase && args.strategy_option.is_some() {
            return Err(GitError::Other(
                "-X applies to a merge only; add --no-rebase".to_owned(),
            ));
        }
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
            let dirty = autostash
                && repo
                    .statuses(Some(&mut status))?
                    .iter()
                    .any(|e| e.status() != Status::CURRENT);
            if dirty {
                let sig = repo.signature()?;
                let id = repo.stash_save2(&sig, Some("autostash"), None)?;
                report(OpProgress::Line(format!(
                    "Created autostash: {}",
                    short7(id)
                )));
            }
            let result = if rebase {
                let upstream = repo.find_annotated_commit(target)?;
                run_rebase(&repo, &upstream, None, report)
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

    fn push_to(
        &self,
        remote: Option<&str>,
        refspecs: &[String],
        args: &crate::PushArgs,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        let repo = self.repo.lock().expect("repo mutex");
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
                    Some(repo.revparse_single(src).ok()?.peel_to_commit().ok()?.id())
                })
                .collect();
            for t in repo.tag_names(None)?.iter().flatten().flatten() {
                let name = format!("refs/tags/{t}");
                let Ok(obj) = repo.revparse_single(&name) else {
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
        if multi_url(&repo, &remote_name, true) {
            let mut cmd = vec!["push".to_owned()];
            for (on, flag) in [
                (args.force_with_lease, "--force-with-lease"),
                (args.dry_run, "--dry-run"),
                (args.set_upstream, "--set-upstream"),
            ] {
                if on {
                    cmd.push(flag.to_owned());
                }
            }
            cmd.push(remote_name);
            cmd.extend(specs.iter().map(|(f, src, dst)| {
                let lead = if args.force || *f { "+" } else { "" };
                format!("{lead}{src}:{dst}")
            }));
            let cmd: Vec<&str> = cmd.iter().map(String::as_str).collect();
            self.run_git(&cmd, &[])?;
            return Ok(());
        }

        let mut remote = repo.find_remote(&remote_name)?;
        let url = remote.url().unwrap_or(&remote_name).to_owned();
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
        let repo_ref = &*repo;
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
                            repo_ref,
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
                    let refs: Vec<git2_hooks::PrePushRef> = out
                        .iter()
                        .filter(|r| r.flag != '=')
                        .map(|r| {
                            let (local, new) = match r.flag {
                                '-' => ("(delete)", None),
                                _ => (r.src.as_str(), Some(r.new)),
                            };
                            let old = (!r.old.is_zero()).then_some(r.old);
                            git2_hooks::PrePushRef::new(local, new, r.dst.as_str(), old)
                        })
                        .collect();
                    let hook =
                        git2_hooks::hooks_pre_push(repo_ref, None, Some(&remote_name), &url, &refs);
                    if let Err(e) = run_hook("pre-push", hook) {
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
            return Err(GitError::PushFailed(format!(
                "{msg}\nerror: failed to push some refs to '{url}'"
            )));
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
            text.extend(push_hints(&repo, &all));
            return Err(GitError::PushFailed(text.join("\n")));
        }
        for line in text {
            report(OpProgress::Line(line));
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
        // libgit2 cannot name a submodule apart from its path, move a
        // submodule's repository into .git/modules, or summarize one.
        let mut argv = vec!["submodule".to_owned()];
        match op {
            Op::Add {
                url,
                path,
                branch,
                name: Some(name),
            } => {
                argv.push("add".to_owned());
                if let Some(b) = branch {
                    argv.extend(["-b".to_owned(), b.clone()]);
                }
                argv.extend([
                    "--name".to_owned(),
                    name.clone(),
                    "--".to_owned(),
                    url.clone(),
                ]);
                argv.extend(path.clone());
            }
            Op::AbsorbGitDirs { paths } => {
                argv.push("absorbgitdirs".to_owned());
                argv.push("--".to_owned());
                argv.extend(paths.iter().cloned());
            }
            Op::Summary { args } => {
                argv.push("summary".to_owned());
                argv.extend(args.iter().cloned());
            }
            _ => {
                return self.logged("submodule", || {
                    let repo = self.repo.lock().expect("repo mutex");
                    sync_index(&repo)?;
                    submodule_op(&repo, op, report)
                });
            }
        }
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        let out = self.run_git(&args, &[])?;
        for line in out.lines() {
            report(OpProgress::Line(line.to_owned()));
        }
        sync_index(&self.repo.lock().expect("repo mutex"))
    }

    fn resolve_object(&self, rev: &str) -> Result<String, GitError> {
        crate::plumbing::resolve(&self.repo.lock().expect("repo mutex"), rev)
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

    fn rev_walk(&self, walk: &crate::RevWalk) -> Result<Vec<crate::WalkCommit>, GitError> {
        crate::plumbing::rev_walk(&self.repo.lock().expect("repo mutex"), walk)
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
    let url = remote.url().unwrap_or_default().to_owned();
    let announced = std::sync::atomic::AtomicBool::new(false);
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
        remote = repo.remote_anonymous(&url)?;
    }
    if args.prune_tags {
        if specs.is_empty() {
            specs = remote
                .fetch_refspecs()?
                .iter()
                .flatten()
                .flatten()
                .map(str::to_owned)
                .collect();
        }
        specs.push("refs/tags/*:refs/tags/*".to_owned());
    }
    let tracking = tracking_specs(&remote, &specs)?;
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let mut callbacks = remote_callbacks(report, &ignored, cred);
    callbacks.update_tips(|dst, old, new| {
        let src = tracking
            .iter()
            .find_map(|(s, d)| map_glob(d, s, dst))
            .unwrap_or_else(|| dst.to_owned());
        if let Some(line) = fetch_line(repo, &src, dst, Some(old), new, true) {
            // Like git, "From" only heads a fetch that changed something.
            if !announced.swap(true, std::sync::atomic::Ordering::Relaxed) {
                report(OpProgress::Line(format!("From {url}")));
            }
            report(OpProgress::Line(line));
        }
        true
    });
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(callbacks);
    if args.prune {
        opts.prune(git2::FetchPrune::On);
    }
    if args.no_tags {
        opts.download_tags(git2::AutotagOption::None);
    } else if args.tags {
        opts.download_tags(git2::AutotagOption::All);
    } else if !refspecs.is_empty() {
        // git follows no tags when the refs to fetch are named.
        opts.download_tags(git2::AutotagOption::None);
    }
    if args.unshallow {
        // libgit2's GIT_FETCH_DEPTH_UNSHALLOW.
        opts.depth(i32::MAX);
    } else if args.depth > 0 {
        opts.depth(args.depth);
    }
    remote.fetch(&specs, Some(&mut opts), None)?;
    Ok(())
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

/// A git-style fetch line for `dst` moving from `old` to `new`; `None` when
/// nothing changes. `done` says the update happened (else a dry run).
fn fetch_line(
    repo: &Repository,
    src: &str,
    dst: &str,
    old: Option<Oid>,
    new: Oid,
    done: bool,
) -> Option<String> {
    let (src, short) = (short_ref(src), short_ref(dst));
    let tag = dst.starts_with("refs/tags/");
    if new.is_zero() {
        return Some(format!(" - [deleted]         (none)     -> {short}"));
    }
    Some(match old.filter(|o| !o.is_zero()) {
        Some(old) if old == new => return None,
        None if tag => format!(" * [new tag]         {src} -> {short}"),
        None if dst.starts_with("refs/heads/") || dst.starts_with("refs/remotes/") => {
            format!(" * [new branch]      {src} -> {short}")
        }
        None => format!(" * [new ref]         {src} -> {short}"),
        Some(_) if tag && done => format!(" t [tag update]      {src} -> {short}"),
        Some(_) if tag => {
            format!(" ! [rejected]        {src} -> {short}  (would clobber existing tag)")
        }
        Some(old) if repo.graph_descendant_of(new, old).unwrap_or(false) => {
            format!("   {}..{}  {src} -> {short}", short7(old), short7(new))
        }
        Some(old) => format!(
            " + {}...{} {src} -> {short}  (forced update)",
            short7(old),
            short7(new)
        ),
    })
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
    let mut remote = repo.find_remote(name)?;
    if let Ok(url) = remote.url() {
        report(OpProgress::Line(format!("From {url}")));
    }
    let tracking = tracking_specs(&remote, refspecs)?;
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
            match tracking.iter().find_map(|(s, d)| map_glob(s, d, head)) {
                Some(dst) => dst,
                None => continue,
            }
        };
        let old = repo.refname_to_id(&dst).ok();
        let Some(line) = fetch_line(repo, head, &dst, old, *new, args.force) else {
            continue;
        };
        report(OpProgress::Line(line));
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
                    report(OpProgress::Line(format!(
                        " - [deleted]         (none)     -> {}",
                        short_ref(local)
                    )));
                }
            }
        }
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
    } else if dst.is_some_and(|d| !d.is_empty()) && repo.revparse_single(src).is_ok() {
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

    // `-s ours` records the merge but keeps HEAD's tree as it is.
    if ours {
        std::fs::write(repo.path().join("MERGE_HEAD"), format!("{}\n", source.id()))?;
    } else {
        let mut merge_opts = git2::MergeOptions::new();
        if let Some(side) = &opts.strategy_option {
            merge_opts.file_favor(file_favor(side)?);
        }
        repo.merge(&[&annotated], Some(&mut merge_opts), None)?;
    }
    let mut message = match &opts.message {
        Some(m) => m.clone(),
        None => merge_title(repo, name)?,
    };
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
    walk.push(source.id())?;
    walk.hide(head.id())?;
    let merged: Vec<git2::Commit> = walk
        .filter_map(|oid| repo.find_commit(oid.ok()?).ok())
        .collect();
    if let Some(limit) = opts.log.filter(|n| *n > 0) {
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
    if !opts.no_verify {
        run_hook_file(repo, "pre-merge-commit")?;
    }
    if opts.signoff {
        message = signoff(&message, &sig);
    }
    if opts.edit {
        message = edit_message(repo, "MERGE_MSG", &message)?;
    }
    if !opts.no_verify {
        run_hook(
            "commit-msg",
            git2_hooks::hooks_commit_msg(repo, None, &mut message),
        )?;
    }
    let message = git2::message_prettify(&message, None)?;
    let tree = repo.find_tree(repo.index()?.write_tree()?)?;
    repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &[&head, source])?;
    end_operation(repo)?;
    report(OpProgress::Line(format!(
        "Merge made by the '{}' strategy.",
        if ours { "ours" } else { "ort" }
    )));
    Ok(())
}

/// git's default merge title for `name`: `Merge branch 'x'`, `Merge
/// remote-tracking branch 'origin/x'`, `Merge tag 'v1'` or `Merge commit 'x'`,
/// with `into <branch>` unless merging into main or master.
fn merge_title(repo: &Repository, name: &str) -> Result<String, GitError> {
    let kind = match repo.resolve_reference_from_short_name(name) {
        Ok(r) if r.is_branch() => "branch ",
        Ok(r) if r.is_remote() => "remote-tracking branch ",
        Ok(r) if r.is_tag() => "tag ",
        _ => "commit ",
    };
    let mut title = format!("Merge {kind}'{name}'");
    if let Ok(branch) = current_branch(repo)
        && branch != "main"
        && branch != "master"
    {
        title.push_str(&format!(" into {branch}"));
    }
    Ok(title)
}

/// Run a hook git2_hooks has no helper for (`pre-merge-commit`), if the hooks
/// directory has it as an executable file.
fn run_hook_file(repo: &Repository, name: &str) -> Result<(), GitError> {
    let workdir = repo.workdir().unwrap_or(repo.path());
    let dir = match repo.config()?.get_path("core.hooksPath") {
        Ok(p) => workdir.join(p),
        Err(_) => repo.path().join("hooks"),
    };
    let hook = dir.join(name);
    #[cfg(unix)]
    let runnable = {
        use std::os::unix::fs::PermissionsExt;
        hook.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    #[cfg(not(unix))]
    let runnable = hook.is_file();
    if !runnable {
        return Ok(());
    }
    let out = std::process::Command::new(&hook)
        .current_dir(workdir)
        .output()?;
    if out.status.success() {
        return Ok(());
    }
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_owned();
    let detail = match text(&out.stderr) {
        e if e.is_empty() => text(&out.stdout),
        e => e,
    };
    Err(GitError::Hook(format!("{name} hook failed: {detail}")))
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

fn default_initial_branch() -> Option<String> {
    git2::Config::open_default()
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
/// credentials as fetch/push and reporting git-style progress.
/// `args.depth` is ignored for a plain local path, as git does.
pub fn clone(
    url: &str,
    path: &Path,
    args: &crate::CloneArgs,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let mut depth = args.depth;
    if !args.git_flags.is_empty() || (depth > 0 && url.starts_with("file://")) {
        return clone_with_git(url, path, args);
    }
    if depth > 0 && is_local_url(url) {
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
    if args.no_tags {
        opts.download_tags(git2::AutotagOption::None);
    }
    let single = match (&args.branch, args.single_branch && !args.mirror) {
        (Some(b), true) => Some(b.clone()),
        (None, true) => {
            let mut remote = git2::Remote::create_detached(url)?;
            let callbacks = remote_callbacks(report, &ignored, None);
            let conn = remote.connect_auth(git2::Direction::Fetch, Some(callbacks), None)?;
            let head = conn.default_branch()?;
            Some(short_ref(head.as_str().unwrap_or("main")).to_owned())
        }
        _ => None,
    };
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(opts).bare(args.bare || args.mirror);
    if let Some(b) = &args.branch {
        builder.branch(b);
    }
    if args.no_checkout {
        let mut checkout = CheckoutBuilder::new();
        checkout.dry_run();
        builder.with_checkout(checkout);
    }
    let origin = args.origin.clone().unwrap_or_else(|| "origin".to_owned());
    let (mirror, no_tags) = (args.mirror, args.no_tags);
    let (name, only) = (origin.clone(), single.clone());
    builder.remote_create(move |repo, _, url| {
        let fetch = match &only {
            _ if mirror => "+refs/*:refs/*".to_owned(),
            Some(b) => format!("+refs/heads/{b}:refs/remotes/{name}/{b}"),
            None => format!("+refs/heads/*:refs/remotes/{name}/*"),
        };
        let remote = repo.remote_with_fetch(&name, url, &fetch)?;
        let mut config = repo.config()?;
        if mirror {
            config.set_bool(&format!("remote.{name}.mirror"), true)?;
        }
        if no_tags {
            config.set_str(&format!("remote.{name}.tagOpt"), "--no-tags")?;
        }
        Ok(remote)
    });
    let repo = builder.clone(url, path)?;
    if mirror {
        // A mirror has no remote-tracking refs and no upstreams.
        if let Ok(mut head) = repo.find_reference(&format!("refs/remotes/{origin}/HEAD")) {
            head.delete()?;
        }
        if let Ok(b) = current_branch(&repo) {
            let mut config = repo.config()?;
            let _ = config.remove(&format!("branch.{b}.remote"));
            let _ = config.remove(&format!("branch.{b}.merge"));
        }
    }
    if let Some(b) = single
        .as_ref()
        .filter(|_| !mirror)
        .or(no_tags.then_some(&origin))
    {
        // libgit2's clone takes every tag; git follows only the branch's, or none.
        let tip = repo
            .refname_to_id(&format!("refs/remotes/{origin}/{b}"))
            .ok();
        for tag in repo.tag_names(None)?.iter().flatten().flatten() {
            let mut r = repo.find_reference(&format!("refs/tags/{tag}"))?;
            let on_branch = !no_tags
                && r.peel_to_commit().is_ok_and(|c| {
                    tip.is_some_and(|t| {
                        c.id() == t || repo.graph_descendant_of(t, c.id()).unwrap_or(false)
                    })
                });
            if !on_branch {
                r.delete()?;
            }
        }
    }
    let mut config = repo.config()?;
    for kv in &args.config {
        let (key, value) = kv.split_once('=').unwrap_or((kv, "true"));
        config.set_str(key, value)?;
    }
    if args.recurse_submodules && !args.bare && !args.mirror {
        update_submodules(&repo, report)?;
    }
    Ok(())
}

/// A clone through git, for what libgit2 cannot do: shallow over `file://`,
/// and alternates, partial, sparse and template clones.
fn clone_with_git(url: &str, path: &Path, args: &crate::CloneArgs) -> Result<(), GitError> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--quiet"]);
    if args.depth > 0 {
        cmd.arg(format!("--depth={}", args.depth));
    }
    if let Some(b) = &args.branch {
        cmd.args(["--branch", b]);
    }
    if let Some(o) = &args.origin {
        cmd.args(["--origin", o]);
    }
    for (on, flag) in [
        (args.bare, "--bare"),
        (args.mirror, "--mirror"),
        (args.recurse_submodules, "--recurse-submodules"),
        (args.single_branch, "--single-branch"),
        (args.no_checkout, "--no-checkout"),
        (args.no_tags, "--no-tags"),
    ] {
        if on {
            cmd.arg(flag);
        }
    }
    for kv in &args.config {
        cmd.args(["--config", kv]);
    }
    let out = cmd
        .args(&args.git_flags)
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
            url, path, branch, ..
        } => {
            let path = path.clone().unwrap_or_else(|| {
                let base = url.trim_end_matches('/').rsplit(['/', ':']).next();
                base.unwrap_or(url).trim_end_matches(".git").to_owned()
            });
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
        } => update_each(repo, "", paths, *init, *recursive, *remote, report)?,
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
        Op::AbsorbGitDirs { .. } | Op::Summary { .. } => {
            unreachable!("run through git")
        }
    }
    Ok(())
}

/// `submodule update` over `repo`'s chosen submodules, reporting paths under `prefix`.
fn update_each(
    repo: &Repository,
    prefix: &str,
    paths: &[String],
    init: bool,
    recursive: bool,
    remote: bool,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let top = repo.workdir().unwrap_or(repo.path()).to_path_buf();
    for mut sm in chosen_submodules(repo, paths)? {
        let name = sm.name().unwrap_or_default().to_owned();
        let path = format!("{prefix}{}", sm.path().display());
        let registered = repo
            .config()?
            .get_string(&format!("submodule.{name}.url"))
            .is_ok();
        if !registered && !init {
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
        let before = sm.open().ok().and_then(|r| r.head().ok()?.target());
        if before.is_none() {
            report(OpProgress::Line(format!(
                "Cloning into '{}'...",
                top.join(sm.path()).display()
            )));
        }
        sm.update(true, None)?;
        let sub = sm.open()?;
        let after = if remote {
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
        if recursive {
            update_each(&sub, &format!("{path}/"), &[], init, true, remote, report)?;
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
    let sig = repo.signature()?;
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
            run_hook(
                "commit-msg",
                git2_hooks::hooks_commit_msg(repo, None, &mut msg),
            )?;
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
        None => repo.index()?.write_tree()?,
    };
    let tree = repo.find_tree(tree_oid)?;
    let mut author = match (&o.author, &o.author_from) {
        (Some(a), _) => Some(find_author(repo, a)?),
        _ if o.reset_author => Some(sig.clone()),
        (None, Some(rev)) => Some(
            repo.revparse_single(rev)?
                .peel_to_commit()?
                .author()
                .to_owned(),
        ),
        (None, None) => None,
    };
    if let Some((secs, offset)) = o.date {
        let base = match (&author, &head) {
            (Some(a), _) => a.clone(),
            (None, Some(h)) if o.amend => h.author().to_owned(),
            _ => sig.clone(),
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
    match head {
        // Commit::amend rewrites HEAD keeping its parents, which the ref-updating
        // commit refuses ("current tip is not the first parent").
        Some(head) if o.amend => {
            head.amend(
                Some("HEAD"),
                author.as_ref(),
                Some(&sig),
                None,
                Some(&msg),
                Some(&tree),
            )?;
        }
        head => {
            if !o.allow_empty && !merging && head.as_ref().is_some_and(|p| p.tree_id() == tree_oid)
            {
                return Err(GitError::NothingToCommit);
            }
            let parents: Vec<&git2::Commit> = head.iter().chain(&merge_heads).collect();
            let author = author.as_ref().unwrap_or(&sig);
            repo.commit(Some("HEAD"), author, &sig, &msg, &tree, &parents)?;
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

    if !o.no_verify {
        let _ = git2_hooks::hooks_post_commit(repo, None);
    }
    Ok(())
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
    index.update_all(paths, None)?;
    index.write()?;
    let spec = Pathspec::new(paths)?;
    let path_of = |e: &git2::IndexEntry| PathBuf::from(String::from_utf8_lossy(&e.path).as_ref());
    let hit = |e: &git2::IndexEntry| spec.matches_path(&path_of(e), PathspecFlags::DEFAULT);
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
fn signoff(msg: &str, sig: &git2::Signature) -> String {
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
    let workdir = repo.workdir().unwrap_or(repo.path());
    let dir = match repo.config()?.get_path("core.hooksPath") {
        Ok(p) => workdir.join(p),
        Err(_) => repo.path().join("hooks"),
    };
    let hook = dir.join("pre-commit");
    let runnable = hook
        .metadata()
        .is_ok_and(|m| m.is_file() && crate::lanes::executable_mode(&m) == 0o100755);
    if !runnable {
        return Ok(());
    }
    let out = std::process::Command::new(&hook)
        .current_dir(workdir)
        .env("GIT_INDEX_FILE", index)
        .output()?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let detail = if err.trim().is_empty() {
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    } else {
        err.trim().to_owned()
    };
    Err(GitError::Hook(format!("pre-commit hook failed: {detail}")))
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

/// Whether remote `name` has several URLs for the direction: libgit2 reads
/// only the last, while git fetches from the first and pushes to each, so
/// such remotes go through git.
fn multi_url(repo: &Repository, name: &str, push: bool) -> bool {
    remote_urls(repo, name, push).is_ok_and(|u| u.len() > 1)
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
    let sig = repo.signature()?;
    let index_tree = repo.find_tree(repo.index()?.write_tree()?)?;
    let index = repo.commit(
        None,
        &sig,
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
        &sig,
        &sig,
        &message,
        tree,
        &[&commit, &repo.find_commit(index)?],
    )?;
    Ok(repo.find_commit(id)?)
}

/// Point `refs/stash` at `id`, logging `message` in its reflog.
fn store_stash(repo: &Repository, id: Oid, message: &str) -> Result<(), GitError> {
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
            // libgit2 ignores GIT_CONFIG_GLOBAL, so stack the files as git
            // does: it replaces both ~/.gitconfig and the XDG file.
            let mut config = Config::new()?;
            let system = std::env::var_os("GIT_CONFIG_SYSTEM")
                .map(PathBuf::from)
                .or_else(|| Config::find_system().ok())
                .filter(|_| std::env::var_os("GIT_CONFIG_NOSYSTEM").is_none());
            for (path, level) in [
                (system, ConfigLevel::System),
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

/// git's `.` (the whole repo, at the root) spelled as libgit2 pathspecs take it.
fn root_dot(path: &str) -> &str {
    if path == "." { "*" } else { path }
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
    let flags = PathspecFlags::DEFAULT;
    let hit = index.is_some_and(|i| {
        spec.match_index(i, flags)
            .is_ok_and(|m| m.entries().len() > 0)
    }) || tree.is_some_and(|t| {
        spec.match_tree(t, flags)
            .is_ok_and(|m| m.entries().len() > 0)
    });
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
    index.update_all(["*"], None)?;
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
            message = signoff(&message, &repo.signature()?);
        }
        let subject = commit.summary().ok().flatten().unwrap_or("").to_owned();
        // With -n the index on disk still holds the starting tree.
        let on_disk = if opts.no_commit { &start } else { &ours };
        let stop = if merged.has_conflicts() {
            checkout_merged(repo, &mut merged, on_disk, verb)?;
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
        let empty = tree
            .as_ref()
            .is_some_and(|t| !opts.no_commit && t.id() == ours.id());
        let was_empty =
            commit.parent_count() == 1 && commit.parent(0)?.tree_id() == commit.tree_id();
        if empty && !was_empty && opts.empty == crate::EmptyCommit::Drop {
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
        if opts.edit {
            message = edit_message(repo, "COMMIT_EDITMSG", &message)?;
        }
        repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe()))?;
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
        checkout_merged(repo, &mut index, &start, verb)?;
    }
    Ok(())
}

/// Write the merge result `index` over the index and working tree, which hold
/// `from`: only the paths that differ are touched, so other staged changes
/// stay, and a local change in one of them stops it, as git does.
fn checkout_merged(
    repo: &Repository,
    index: &mut git2::Index,
    from: &git2::Tree<'_>,
    verb: &str,
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
    let mut checkout = CheckoutBuilder::new();
    checkout
        .force()
        .allow_conflicts(true)
        .conflict_style_merge(true)
        .disable_pathspec_match(true);
    for p in &paths {
        checkout.path(p);
    }
    repo.checkout_index(Some(index), Some(&mut checkout))?;
    Ok(())
}

/// Let the user edit `msg` in their editor (GIT_EDITOR, core.editor, VISUAL,
/// EDITOR), as git's `-e` does, through `.git/<file>`. Comment lines are
/// dropped and an empty message aborts.
fn edit_message(repo: &Repository, file: &str, msg: &str) -> Result<String, GitError> {
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

/// Merge several heads at once. libgit2 merges only a single head, so this
/// runs git's octopus strategy.
fn octopus(
    backend: &Git2Backend,
    revs: &[String],
    opts: &crate::MergeOptions,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let stat = if opts.stat { "--stat" } else { "--no-stat" };
    let mut args = vec!["merge", "--no-edit", stat];
    for (on, flag) in [
        (opts.no_ff, "--no-ff"),
        (opts.ff_only, "--ff-only"),
        (opts.squash, "--squash"),
        (opts.no_commit, "--no-commit"),
        (opts.allow_unrelated, "--allow-unrelated-histories"),
        (opts.no_verify, "--no-verify"),
        (opts.signoff, "--signoff"),
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
    if let Some(strategy) = &opts.strategy {
        args.extend(["-s", strategy]);
    }
    let log = opts.log.map(|n| format!("--log={n}"));
    args.extend(log.as_deref());
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
    let (year, month, day) = civil_date(t);
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

/// A signature time's local (year, month, day).
fn civil_date(t: git2::Time) -> (i64, i64, i64) {
    let days = (t.seconds() + i64::from(t.offset_minutes()) * 60).div_euclid(86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
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
    let commit = repo.revparse_single(start)?.peel_to_commit()?;
    let mut branch = repo.branch(name, &commit, reset)?;
    if track.unwrap_or_else(|| repo.find_branch(start, BranchType::Remote).is_ok()) {
        branch.set_upstream(Some(start))?;
    }
    Ok(())
}

/// The path of the worktree, main or linked, that has `refname` checked out.
fn checked_out_at(repo: &Repository, refname: &str) -> Result<Option<String>, GitError> {
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
