use std::path::{Path, PathBuf};
use std::sync::Mutex;

use git2::build::CheckoutBuilder;
use git2::{
    ApplyLocation, ApplyOptions, BranchType, Cred, CredentialType, Diff, DiffOptions, ErrorCode,
    FetchOptions, ObjectType, Oid, Patch, PushOptions, RemoteCallbacks, Repository, ResetType,
    Status, StatusOptions,
};

use crate::model::{RepoState, ResetMode};

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

    /// Snapshot the current state into the operation log before a destructive
    /// operation, so it can be undone. Best-effort: a snapshot failure is logged
    /// but never blocks the operation. Takes and releases the repo lock, so call
    /// it before the operation acquires its own lock (the mutex is not
    /// reentrant).
    fn snap(&self, label: &str) {
        let repo = self.repo.lock().expect("repo mutex");
        if let Err(e) = crate::oplog::snapshot(&repo, label) {
            tracing::warn!(target: "rgit_git", "oplog snapshot failed: {e}");
        }
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
        let mut opts = DiffOptions::new();
        let unstaged = extract(&repo.diff_index_to_workdir(None, Some(&mut opts))?)?;
        let staged =
            extract(&repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?)?;

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
        self.snap("stash");
        let mut repo = self.repo.lock().expect("repo mutex");
        let sig = repo.signature()?;
        repo.stash_save2(&sig, None, stash_flags(include_untracked))?;
        Ok(stash_saved_line(&repo))
    }

    fn stash_push_message(
        &self,
        message: &str,
        include_untracked: bool,
    ) -> Result<String, GitError> {
        self.snap("stash");
        let mut repo = self.repo.lock().expect("repo mutex");
        let sig = repo.signature()?;
        repo.stash_save2(&sig, Some(message), stash_flags(include_untracked))?;
        Ok(stash_saved_line(&repo))
    }

    fn stash_pop(&self, index: usize) -> Result<(), GitError> {
        self.snap("stash pop");
        let mut repo = self.repo.lock().expect("repo mutex");
        repo.stash_pop(index, None)?;
        Ok(())
    }

    fn stash_apply(&self, index: usize) -> Result<(), GitError> {
        self.snap("stash apply");
        let mut repo = self.repo.lock().expect("repo mutex");
        repo.stash_apply(index, None)?;
        Ok(())
    }

    fn stash_drop(&self, index: usize) -> Result<(), GitError> {
        self.snap("stash drop");
        let mut repo = self.repo.lock().expect("repo mutex");
        repo.stash_drop(index)?;
        Ok(())
    }

    fn log(&self, opts: &crate::LogOptions) -> Result<Vec<crate::LogEntry>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let mut walk = repo.revwalk()?;
        let seeded = if opts.all {
            walk.push_glob("refs/*").is_ok()
        } else if let Some(rev) = &opts.rev {
            repo.revparse_single(rev)
                .ok()
                .and_then(|o| o.peel_to_commit().ok())
                .map(|c| walk.push(c.id()).is_ok())
                .unwrap_or(false)
        } else {
            walk.push_head().is_ok()
        };
        if !seeded {
            return Ok(Vec::new());
        }
        walk.set_sorting(git2::Sort::TIME)?;

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
            if let Some(path) = &opts.path {
                if !commit_touched_path(&repo, &commit, path) {
                    continue;
                }
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

        Ok(crate::CommitDetails {
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
            files: extract(&diff)?,
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
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;

        let mut files = Vec::with_capacity(diff.deltas().len());
        for idx in 0..diff.deltas().len() {
            let delta = diff.get_delta(idx).expect("delta in range");
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
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

    fn commit_file_diff(
        &self,
        rev: &str,
        path: &str,
    ) -> Result<Option<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let commit = repo.revparse_single(rev)?.peel_to_commit()?;
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut opts = DiffOptions::new();
        opts.pathspec(path);
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
        Ok(extract(&diff)?.into_iter().find(|f| f.path == path))
    }

    fn diff_refs(&self, from: &str, to: &str) -> Result<Vec<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let from_tree = repo.revparse_single(from)?.peel_to_tree()?;
        let to_tree = repo.revparse_single(to)?.peel_to_tree()?;
        let mut opts = DiffOptions::new();
        let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), Some(&mut opts))?;
        extract(&diff)
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
            // Include untracked files so a brand-new file shows as an all-added diff.
            opts.include_untracked(true).recurse_untracked_dirs(true);
            repo.diff_index_to_workdir(None, Some(&mut opts))?
        };
        Ok(extract(&diff)?.into_iter().find(|f| f.path == path))
    }

    fn blame(&self, path: &str) -> Result<Vec<crate::BlameLine>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let blame = repo.blame_file(Path::new(path), None)?;
        let content = std::fs::read_to_string(self.workdir.join(path))?;

        let lines = content
            .lines()
            .enumerate()
            .map(|(i, text)| match blame.get_line(i + 1) {
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
            .collect();
        Ok(lines)
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
        let mut index = repo.index()?;
        let rel = Path::new(path);
        if self.workdir.join(path).exists() {
            index.add_path(rel)?;
        } else {
            index.remove_path(rel)?;
        }
        index.write()?;
        Ok(())
    }

    fn unstage_file(&self, path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
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
        let commit = repo.revparse_single(rev)?.peel_to_commit()?;
        let blob = commit
            .tree()?
            .get_path(std::path::Path::new(path))?
            .to_object(&repo)?
            .peel_to_blob()?;
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
            let oid_at = |t: &git2::Tree, p: &str| {
                t.get_path(std::path::Path::new(p)).ok().map(|e| e.id())
            };
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
                    summary: commit.summary().ok().flatten().unwrap_or_default().to_owned(),
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
            if entry.kind() == Some(git2::ObjectType::Blob) {
                if let Ok(name) = entry.name() {
                    // `root` is the containing dir with a trailing slash, or empty.
                    out.push(format!("{root}{name}"));
                }
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
                if let Some(needle) = &path_filter {
                    if !rel.to_lowercase().contains(needle.as_str()) {
                        return None;
                    }
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
        for name in repo.tag_names(None)?.iter().filter_map(|t| t.ok().flatten()) {
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
        let repo = self.repo.lock().expect("repo mutex");
        let tree = repo.revparse_single(rev)?.peel_to_commit()?.tree()?;
        let mut buf = Vec::new();
        let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        // A write failure inside the walk closure is captured here, since the
        // callback can only signal continue/abort, not return an error.
        let mut failure: Option<GitError> = None;
        tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() != Some(git2::ObjectType::Blob) {
                return git2::TreeWalkResult::Ok;
            }
            let Ok(name) = entry.name() else {
                return git2::TreeWalkResult::Ok;
            };
            let path = format!("{root}{name}");
            let blob = match entry.to_object(&repo).and_then(|o| o.peel_to_blob()) {
                Ok(b) => b,
                Err(e) => {
                    failure = Some(e.into());
                    return git2::TreeWalkResult::Abort;
                }
            };
            let content = blob.content();
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(if entry.filemode() == 0o120000 { 0o777 } else { 0o644 });
            header.set_cksum();
            if let Err(e) = builder.append_data(&mut header, &path, content) {
                failure = Some(e.into());
                return git2::TreeWalkResult::Abort;
            }
            git2::TreeWalkResult::Ok
        })?;
        if let Some(e) = failure {
            return Err(e);
        }
        builder.into_inner()?.finish()?;
        Ok(buf)
    }

    fn checkout_detached(&self, rev: &str) -> Result<(), GitError> {
        self.snap("checkout");
        let repo = self.repo.lock().expect("repo mutex");
        let commit = repo.revparse_single(rev)?.peel_to_commit()?;
        repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe()))?;
        repo.set_head_detached(commit.id())?;
        Ok(())
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
        self.snap("rebase");
        let repo = self.repo.lock().expect("repo mutex");
        let target = repo.revparse_single(rev)?.peel_to_commit()?;
        let upstream = repo.find_annotated_commit(target.id())?;
        run_rebase(&repo, &upstream, None, report)
    }

    fn rebase_range(
        &self,
        upstream: &str,
        onto: &str,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        self.snap("restack");
        let repo = self.repo.lock().expect("repo mutex");
        let upstream_oid = repo.revparse_single(upstream)?.peel_to_commit()?.id();
        let onto_oid = repo.revparse_single(onto)?.peel_to_commit()?.id();
        let upstream = repo.find_annotated_commit(upstream_oid)?;
        let onto = repo.find_annotated_commit(onto_oid)?;
        run_rebase(&repo, &upstream, Some(&onto), report)
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

    fn rebase_interactive(&self, onto: Option<&str>) -> Result<(), GitError> {
        // Inherit stdio so git can open the todo editor on the terminal.
        let mut cmd = std::process::Command::new("git");
        cmd.arg("rebase").arg("-i").current_dir(&self.workdir);
        if let Some(onto) = onto {
            cmd.arg(onto);
        }
        let status = cmd
            .status()
            .map_err(|e| GitError::Cli(format!("could not run git: {e}")))?;
        if status.success() {
            Ok(())
        } else {
            Err(GitError::Cli(
                "interactive rebase failed or was aborted".into(),
            ))
        }
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

        self.snap("absorb");
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
                        if let Ok(dl) = patch.line_in_hunk(h, l) {
                            if dl.origin() == '-' {
                                if let Some(no) = dl.old_lineno() {
                                    line = no as usize;
                                    break;
                                }
                            }
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
    }

    fn bisect(&self, args: &[String]) -> Result<String, GitError> {
        let mut argv = vec!["bisect"];
        argv.extend(args.iter().map(String::as_str));
        self.run_git(&argv, &[])
    }

    fn clean(&self, dry_run: bool) -> Result<String, GitError> {
        if !dry_run {
            self.snap("clean");
        }
        // libgit2 has no clean; -nd lists, -fd removes (files and directories).
        self.run_git(&["clean", if dry_run { "-nd" } else { "-fd" }], &[])
    }

    fn remove_path(&self, path: &str, cached: bool) -> Result<(), GitError> {
        self.snap("rm");
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let mut index = repo.index()?;
        index.remove_path(Path::new(path))?;
        index.write()?;
        if !cached {
            let full = self.workdir.join(path);
            if full.exists() {
                std::fs::remove_file(full)?;
            }
        }
        Ok(())
    }

    fn move_path(&self, from: &str, to: &str, force: bool) -> Result<(), GitError> {
        self.snap("mv");
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        if !force && self.workdir.join(to).exists() {
            return Err(GitError::Other(format!(
                "destination {to} already exists; use --force to overwrite"
            )));
        }
        std::fs::rename(self.workdir.join(from), self.workdir.join(to))?;
        let mut index = repo.index()?;
        index.remove_path(Path::new(from))?;
        index.add_path(Path::new(to))?;
        index.write()?;
        Ok(())
    }

    fn describe(&self, rev: &str) -> Result<String, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let obj = repo.revparse_single(rev)?;
        let mut opts = git2::DescribeOptions::new();
        opts.describe_tags().show_commit_oid_as_fallback(true);
        let describe = obj.describe(&opts)?;
        Ok(describe.format(Some(
            git2::DescribeFormatOptions::new().dirty_suffix("-dirty"),
        ))?)
    }

    fn git(&self, args: &[String]) -> Result<String, GitError> {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_git(&argv, &[])
    }

    fn reset(&self, rev: &str, mode: ResetMode) -> Result<(), GitError> {
        self.snap("reset");
        let repo = self.repo.lock().expect("repo mutex");
        let target = repo.revparse_single(rev)?;
        let kind = match mode {
            ResetMode::Soft => ResetType::Soft,
            ResetMode::Mixed => ResetType::Mixed,
            ResetMode::Hard => ResetType::Hard,
        };
        repo.reset(&target, kind, None)?;
        Ok(())
    }

    fn cherry_pick(&self, rev: &str, no_commit: bool) -> Result<(), GitError> {
        self.snap("cherry-pick");
        let repo = self.repo.lock().expect("repo mutex");
        let source = repo.revparse_single(rev)?.peel_to_commit()?;
        repo.cherrypick(&source, None)?;
        if no_commit {
            // Leave the change staged; the caller commits when ready.
            return Ok(());
        }
        finalize_sequenced(&repo, &source.author(), source.message().unwrap_or(""))
    }

    fn revert(&self, rev: &str, no_commit: bool) -> Result<(), GitError> {
        self.snap("revert");
        let repo = self.repo.lock().expect("repo mutex");
        let source = repo.revparse_single(rev)?.peel_to_commit()?;
        repo.revert(&source, None)?;
        if no_commit {
            return Ok(());
        }
        let summary = source.summary().ok().flatten().unwrap_or("commit");
        let message = format!(
            "Revert \"{summary}\"\n\nThis reverts commit {}.",
            source.id()
        );
        finalize_sequenced(&repo, &repo.signature()?, &message)
    }

    fn merge(
        &self,
        rev: &str,
        no_ff: bool,
        ff_only: bool,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        self.snap("merge");
        let repo = self.repo.lock().expect("repo mutex");
        let source = repo.revparse_single(rev)?.peel_to_commit()?;
        let annotated = repo.find_annotated_commit(source.id())?;
        let (analysis, _) = repo.merge_analysis(&[&annotated])?;

        if analysis.is_up_to_date() {
            report(OpProgress::Line("Already up to date.".to_owned()));
            return Ok(());
        }
        if ff_only && !analysis.is_fast_forward() {
            return Err(GitError::Other(
                "not possible to fast-forward; use a merge commit instead".to_owned(),
            ));
        }
        if analysis.is_fast_forward() && !no_ff {
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

        repo.merge(&[&annotated], None, None)?;
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
            return Err(GitError::Conflict(
                "merge conflicts; resolve and commit, or reset --hard to abort".into(),
            ));
        }
        let sig = repo.signature()?;
        let tree = repo.find_tree(repo.index()?.write_tree()?)?;
        let head = repo.head()?.peel_to_commit()?;
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("Merge {rev}"),
            &tree,
            &[&head, &source],
        )?;
        repo.cleanup_state()?;
        report(OpProgress::Line(
            "Merge made by the 'ort' strategy.".to_owned(),
        ));
        Ok(())
    }

    fn merge_abort(&self) -> Result<(), GitError> {
        self.snap("merge abort");
        let repo = self.repo.lock().expect("repo mutex");
        // A conflicted merge has not moved HEAD, so hard-resetting to it drops the
        // half-merged index and worktree; cleanup_state clears MERGE_HEAD et al.
        let head = repo.head()?.peel_to_commit()?;
        repo.reset(head.as_object(), git2::ResetType::Hard, None)?;
        repo.cleanup_state()?;
        Ok(())
    }

    fn resolve_conflict(&self, path: &str, ours: bool) -> Result<(), GitError> {
        self.snap("resolve");
        let repo = self.repo.lock().expect("repo mutex");
        let mut index = repo.index()?;
        // Find the chosen side's blob before mutating the index.
        let chosen = {
            let conflicts = index.conflicts()?;
            let mut found = None;
            for entry in conflicts {
                let entry = entry?;
                let side = if ours { entry.our } else { entry.their };
                if let Some(e) = side {
                    if e.path == path.as_bytes() {
                        found = Some(e.id);
                        break;
                    }
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
        self.snap("delete branch");
        let repo = self.repo.lock().expect("repo mutex");
        let mut branch = repo.find_branch(name, BranchType::Local)?;
        if !force {
            let tip = branch.get().peel_to_commit()?.id();
            let merged = repo
                .head()
                .ok()
                .and_then(|h| h.peel_to_commit().ok())
                .map(|head| head.id() == tip || repo.graph_descendant_of(head.id(), tip).unwrap_or(false))
                .unwrap_or(false);
            if !merged {
                return Err(GitError::Other(format!(
                    "branch {name} is not fully merged into HEAD; use --force to delete"
                )));
            }
        }
        branch.delete()?;
        Ok(())
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
        let names = repo.worktrees()?;
        let mut out = Vec::new();
        for name in names.iter().filter_map(|n| n.ok().flatten()) {
            if let Ok(wt) = repo.find_worktree(name) {
                out.push(crate::Worktree {
                    name: name.to_owned(),
                    path: wt.path().to_string_lossy().into_owned(),
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn add_worktree(&self, name: &str, path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.worktree(name, Path::new(path), None)?;
        Ok(())
    }

    fn remove_worktree(&self, name: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let wt = repo.find_worktree(name)?;
        let mut opts = git2::WorktreePruneOptions::new();
        opts.valid(true).working_tree(true);
        wt.prune(Some(&mut opts))?;
        Ok(())
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
        self.snap("lane restack");
        let repo = self.repo.lock().expect("repo mutex");
        crate::lanes::restack(&repo)
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
        self.snap("lane commit");
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        crate::lanes::commit(&repo, lane, message)
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
        self.snap("rename branch");
        let repo = self.repo.lock().expect("repo mutex");
        repo.find_branch(old, BranchType::Local)?
            .rename(new, false)?;
        Ok(())
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
        self.snap("checkout");
        let repo = self.repo.lock().expect("repo mutex");
        checkout(&repo, name)
    }

    fn create_branch(&self, name: &str) -> Result<(), GitError> {
        self.snap("create branch");
        let repo = self.repo.lock().expect("repo mutex");
        let head = repo.head()?.peel_to_commit()?;
        repo.branch(name, &head, false)?;
        checkout(&repo, name)
    }

    fn discard_file(&self, path: &str) -> Result<(), GitError> {
        self.snap("discard");
        let repo = self.repo.lock().expect("repo mutex");
        sync_index(&repo)?;
        let status = repo.status_file(Path::new(path))?;
        if status.contains(Status::WT_NEW) {
            std::fs::remove_file(self.workdir.join(path))?;
        } else {
            // Restore the worktree file to its index (staged) content.
            let mut checkout = CheckoutBuilder::new();
            checkout.path(path).force();
            repo.checkout_index(None, Some(&mut checkout))?;
        }
        Ok(())
    }

    fn discard_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError> {
        self.snap("discard");
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
    }

    fn discard_lines(&self, path: &str, new_start: u32, lines: &[usize]) -> Result<(), GitError> {
        self.snap("discard");
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
    }

    fn commit_msg_path(&self) -> PathBuf {
        self.repo
            .lock()
            .expect("repo mutex")
            .path()
            .join("COMMIT_EDITMSG")
    }

    fn commit(&self, message: &str) -> Result<(), GitError> {
        self.snap("commit");
        let repo = self.repo.lock().expect("repo mutex");
        let parents = match repo.head() {
            Ok(head_ref) => vec![head_ref.peel_to_commit()?],
            Err(_) => Vec::new(),
        };
        make_commit(&repo, message, &parents, true, true)
    }

    fn amend(&self, message: &str) -> Result<(), GitError> {
        self.snap("amend");
        let repo = self.repo.lock().expect("repo mutex");
        make_amend(&repo, message, true)
    }

    fn reword(&self, rev: &str, message: &str) -> Result<(), GitError> {
        self.snap("reword");
        {
            let repo = self.repo.lock().expect("repo mutex");
            let target = repo.revparse_single(rev)?.peel_to_commit()?.id();
            reword_commit(&repo, target, message)?;
        }
        // Descendant stacked branches point at the old oids; move them forward.
        let _ = self.restack();
        Ok(())
    }

    fn uncommit(&self, n: usize) -> Result<(), GitError> {
        self.snap("uncommit");
        let repo = self.repo.lock().expect("repo mutex");
        let target = repo.revparse_single(&format!("HEAD~{}", n.max(1)))?;
        repo.reset(&target, ResetType::Soft, None)?;
        Ok(())
    }

    fn split(&self, rev: &str, paths: &[String]) -> Result<(), GitError> {
        self.snap("split");
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
            let selected = |p: &str| paths.iter().any(|s| p == s || p.starts_with(&format!("{s}/")));
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
            let c1 = repo.commit(None, &target.author(), &sig, &subject, &part1_tree, &[&parent])?;
            let c1_commit = repo.find_commit(c1)?;
            let c2 = repo.commit(None, &target.author(), &sig, full_msg, &c_tree, &[&c1_commit])?;

            let chain = first_parent_chain(&repo, target.id())?;
            let descendants: Vec<&git2::Commit> = chain.iter().rev().skip(1).collect();
            let new_tip = replay_onto(&repo, &descendants, c2)?;
            repo.reference(&branch_ref, new_tip, true, "rgit split")?;
        }
        let _ = self.restack();
        Ok(())
    }

    fn sync(&self, report: &dyn Fn(OpProgress)) -> Result<crate::RestackOutcome, GitError> {
        self.snap("sync");
        self.fetch(None, false, false, report)?;
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
                    if up != tip && repo.graph_descendant_of(up, tip).unwrap_or(false) {
                        if let Some(name) = &full {
                            repo.reference(name, up, true, "rgit sync fast-forward")?;
                        }
                    }
                }
            }
        }
        self.restack()
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
        self.snap("prune");
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
    }

    fn reorder(&self, rev: &str, target: &str, before: bool) -> Result<(), GitError> {
        self.snap("reorder");
        {
            let repo = self.repo.lock().expect("repo mutex");
            let branch_ref = head_branch_ref(&repo)?;
            let rev_oid = repo.revparse_single(rev)?.peel_to_commit()?.id();
            let target_oid = repo.revparse_single(target)?.peel_to_commit()?.id();
            if rev_oid == target_oid {
                return Err(GitError::Other("cannot move a commit onto itself".to_owned()));
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
            let base = chain
                .last()
                .unwrap()
                .parent(0)
                .map_err(|_| GitError::Other("cannot reorder across the root commit".to_owned()))?;

            // Affected commits, oldest first; move rev relative to target.
            let mut order: Vec<Oid> = chain.iter().rev().map(git2::Commit::id).collect();
            let rev_pos = order.iter().position(|&o| o == rev_oid).expect("rev in order");
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
    }

    fn squash_range(&self, from: &str) -> Result<(), GitError> {
        self.snap("squash");
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
            let msg = crate::change_id::preserve(head.message().unwrap_or(""), msg.trim_end());
            let sig = repo.signature()?;
            let new = repo.commit(None, &head.author(), &sig, &msg, &head.tree()?, &[&base])?;
            repo.reference(&branch_ref, new, true, "rgit squash range")?;
        }
        let _ = self.restack();
        Ok(())
    }

    fn squash(&self, rev: &str) -> Result<(), GitError> {
        self.snap("squash");
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
            let combined = crate::change_id::preserve(parent.message().unwrap_or(""), &combined);
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
    }

    fn hooks_dir(&self) -> PathBuf {
        let repo = self.repo.lock().expect("repo mutex");
        // core.hooksPath wins (relative to the working directory); otherwise the
        // repository's own hooks directory.
        if let Ok(cfg) = repo.config() {
            if let Ok(p) = cfg.get_path("core.hooksPath") {
                return if p.is_absolute() {
                    p
                } else {
                    self.workdir.join(p)
                };
            }
        }
        repo.path().join("hooks")
    }

    fn commit_no_verify(&self, message: &str) -> Result<(), GitError> {
        self.snap("commit");
        let repo = self.repo.lock().expect("repo mutex");
        let parents = match repo.head() {
            Ok(head_ref) => vec![head_ref.peel_to_commit()?],
            Err(_) => Vec::new(),
        };
        make_commit(&repo, message, &parents, true, false)
    }

    fn amend_no_verify(&self, message: &str) -> Result<(), GitError> {
        self.snap("amend");
        let repo = self.repo.lock().expect("repo mutex");
        make_amend(&repo, message, false)
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
        all: bool,
        prune: bool,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        let cred = cred_guard.as_deref();
        if all {
            let names: Vec<String> = repo
                .remotes()?
                .iter()
                .filter_map(|t| t.ok().flatten())
                .map(|s| s.to_owned())
                .collect();
            for name in names {
                do_fetch(&repo, &name, prune, report, cred)?;
            }
            Ok(())
        } else {
            let name = match remote {
                Some(r) => r.to_owned(),
                None => upstream_remote(&repo)?.0,
            };
            do_fetch(&repo, &name, prune, report, cred)
        }
    }

    fn pull(&self, rebase: bool, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
        // Snapshot before locking (snap takes the repo lock itself) so a rebase
        // pull is undoable.
        if rebase {
            self.snap("pull --rebase");
        }
        let repo = self.repo.lock().expect("repo mutex");
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        let cred = cred_guard.as_deref();
        let (remote, branch) = upstream_remote(&repo)?;
        do_fetch(&repo, &remote, false, report, cred)?;
        if rebase {
            let target = repo.refname_to_id(&format!("refs/remotes/{remote}/{branch}"))?;
            let upstream = repo.find_annotated_commit(target)?;
            run_rebase(&repo, &upstream, None, report)
        } else {
            fast_forward(&repo, &remote, &branch, report)
        }
    }

    fn push(
        &self,
        remote: Option<&str>,
        force: bool,
        force_with_lease: bool,
        set_upstream: bool,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let cred_guard = self.cred_prompt.lock().expect("cred mutex");
        // A named remote pushes the current branch to <remote>/<branch>; without
        // one, fall back to the branch's configured upstream.
        let (remote_name, branch) = match remote {
            Some(r) => {
                let head = repo.head()?;
                let branch = head
                    .shorthand()
                    .ok()
                    .map(str::to_owned)
                    .ok_or_else(|| GitError::Other("HEAD is detached; not on a branch".to_owned()))?;
                (r.to_owned(), branch)
            }
            None => upstream_remote(&repo)?,
        };
        let branch_ref = format!("refs/heads/{branch}");
        let lease = repo
            .refname_to_id(&format!("refs/remotes/{remote_name}/{branch}"))
            .ok();

        let mut remote = repo.find_remote(&remote_name)?;
        if let Ok(url) = remote.url() {
            report(OpProgress::Line(format!("To {url}")));
        }
        let lead = if force || force_with_lease { "+" } else { "" };
        let refspec = format!("{lead}{branch_ref}:{branch_ref}");

        let rejected = std::sync::atomic::AtomicBool::new(false);
        let mut callbacks = remote_callbacks(report, &rejected, cred_guard.as_deref());
        if force_with_lease {
            // Abort if the remote no longer matches the ref we last fetched -
            // this is force-with-lease, done through the negotiation callback.
            let target = branch_ref.clone();
            callbacks.push_negotiation(move |updates| {
                for update in updates {
                    if update.dst_refname().ok() == Some(target.as_str()) {
                        if let Some(lease) = lease {
                            if update.src() != lease {
                                return Err(git2::Error::from_str(
                                    "stale info: the remote branch moved; force-with-lease aborted",
                                ));
                            }
                        }
                    }
                }
                Ok(())
            });
        }

        let mut opts = PushOptions::new();
        opts.remote_callbacks(callbacks);
        remote.push(&[refspec.as_str()], Some(&mut opts))?;
        // libgit2 reports a refused ref through the callback but still returns
        // success, so surface the rejection as an error.
        if rejected.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(GitError::PushRejected);
        }

        if set_upstream {
            repo.find_branch(&branch, BranchType::Local)?
                .set_upstream(Some(&format!("{remote_name}/{branch}")))?;
        }
        Ok(())
    }

    fn push_tags(
        &self,
        remote: Option<&str>,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
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
        self.snap("push delete");
        // An empty source ref deletes the destination on the remote.
        self.push_refspecs(remote, &[format!(":refs/heads/{branch}")], report)
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
    prune: bool,
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
    if prune {
        opts.prune(git2::FetchPrune::On);
    }
    remote.fetch::<&str>(&[], Some(&mut opts), None)?;
    Ok(())
}

fn fast_forward(
    repo: &Repository,
    remote: &str,
    branch: &str,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let tracking = format!("refs/remotes/{remote}/{branch}");
    let target = repo.refname_to_id(&tracking)?;
    let annotated = repo.find_annotated_commit(target)?;
    let (analysis, _) = repo.merge_analysis(&[&annotated])?;

    if analysis.is_up_to_date() {
        report(OpProgress::Line("Already up to date.".to_owned()));
        return Ok(());
    }
    if !analysis.is_fast_forward() {
        return Err(GitError::NotFastForward);
    }

    let local = format!("refs/heads/{branch}");
    if let Ok(old) = repo.refname_to_id(&local) {
        report(OpProgress::Line(format!(
            "Updating {}..{}",
            short7(old),
            short7(target)
        )));
    }
    repo.find_reference(&local)?
        .set_target(target, "pull: fast-forward")?;
    repo.set_head(&local)?;
    repo.checkout_head(Some(CheckoutBuilder::new().force()))?;
    report(OpProgress::Line("Fast-forward".to_owned()));
    Ok(())
}

/// Create a new repository at `path` (`git init`), via libgit2.
/// Create a repository at `path`. `initial_branch` names the first branch
/// (git's `-b`); `bare` makes a bare repository (git's `--bare`).
pub fn init(path: &Path, initial_branch: Option<&str>, bare: bool) -> Result<(), GitError> {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.bare(bare);
    if let Some(b) = initial_branch {
        opts.initial_head(b);
    }
    Repository::init_opts(path, &opts)?;
    Ok(())
}

/// Clone `url` into `path` (`git clone`), via libgit2, using the same
/// credentials as fetch/push and reporting git-style progress.
/// Clone `url` into `path`. `branch` checks out that branch instead of the
/// remote's default HEAD; `depth > 0` makes a shallow clone of that many commits.
pub fn clone(
    url: &str,
    path: &Path,
    branch: Option<&str>,
    depth: i32,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(remote_callbacks(report, &ignored, None));
    if depth > 0 {
        opts.depth(depth);
    }
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(opts);
    if let Some(b) = branch {
        builder.branch(b);
    }
    builder.clone(url, path)?;
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
    Cred::ssh_key(user, public.exists().then_some(&public), &private, passphrase)
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
                            let key = if ssh_attempts == 4 { "id_ed25519" } else { "id_rsa" };
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
                if let Ok(config) = git2::Config::open_default() {
                    if let Ok(c) = Cred::credential_helper(&config, url, username) {
                        return Ok(c);
                    }
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
    // After the commit-msg hook (which a Gerrit setup may use to add its own
    // Change-Id), stamp one if none is present so the change has a stable id.
    let msg = crate::change_id::ensure(&msg);

    let mut index = repo.index()?;
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    if check_empty && parents.first().is_some_and(|p| p.tree_id() == tree_oid) {
        return Err(GitError::NothingToCommit);
    }

    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
    let sig = repo.signature()?;
    repo.commit(Some("HEAD"), &sig, &sig, &msg, &tree, &parent_refs)?;

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
    if let Ok(tree) = commit.tree() {
        if let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) {
            if let Ok(stats) = diff.stats() {
                let plural =
                    |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "s" });
                let mut line = format!(" {} changed", plural(stats.files_changed(), "file"));
                if stats.insertions() > 0 {
                    line += &format!(", {}(+)", plural(stats.insertions(), "insertion"));
                }
                if stats.deletions() > 0 {
                    line += &format!(", {}(-)", plural(stats.deletions(), "deletion"));
                }
                out.push(line);
            }
        }
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
            (short, commit.summary().ok().flatten().unwrap_or("").to_owned())
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
    let msg = crate::change_id::preserve(&old_msg, &msg);

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
            crate::change_id::preserve(commit.message().unwrap_or(""), new_message)
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
        tip = repo.commit(None, &c.author(), &sig, c.message().unwrap_or(""), &tree, &[&our])?;
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
    if let Ok(head) = repo.head() {
        if let Ok(name) = head.shorthand() {
            if let Ok(local) = repo.find_branch(name, BranchType::Local) {
                if let Ok(up) = local.upstream() {
                    if let Some(oid) = up.get().target() {
                        return Some(oid);
                    }
                }
            }
        }
    }
    for name in ["main", "master", "develop", "trunk"] {
        if let Ok(branch) = repo.find_branch(name, BranchType::Local) {
            if let Some(oid) = branch.get().target() {
                return Some(oid);
            }
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
            out.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                hunk.new_start(),
                hunk.new_lines(),
                hunk.old_start(),
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
        if let (Some(local_oid), Some(up_oid)) = (local.get().target(), upstream.get().target()) {
            if let Ok((ahead, behind)) = repo.graph_ahead_behind(local_oid, up_oid) {
                head.ahead = ahead;
                head.behind = behind;
            }
        }
    }
    fill_remotes(repo, &local, branch, head);
    // No tracking configured, but the branch exists on a remote by name: treat
    // that as the effective upstream so "published" state and the REMOTE overview
    // agree. Prefer origin (the usual push target), else the first such remote.
    if !configured {
        if let Some((name, ahead, behind)) = head
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
        if let Ok(rb) = repo.find_branch(&tracking, BranchType::Remote) {
            if let Some(roid) = rb.get().target() {
                if let Ok((ahead, behind)) = repo.graph_ahead_behind(local_oid, roid) {
                    head.remotes.push((tracking, ahead, behind));
                }
            }
        }
    }
}

fn collect_entries(repo: &Repository) -> Result<Vec<StatusEntry>, GitError> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);

    let statuses = repo.statuses(Some(&mut opts))?;
    let mut entries = Vec::with_capacity(statuses.len());

    for entry in statuses.iter() {
        let status = entry.status();
        if status.contains(Status::IGNORED) {
            continue;
        }
        let Ok(path) = entry.path() else {
            continue;
        };
        let orig_path = entry
            .head_to_index()
            .or_else(|| entry.index_to_workdir())
            .and_then(|d| d.old_file().path())
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|old| old != path);

        entries.push(StatusEntry {
            path: path.to_owned(),
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

/// Commit the result of a cherry-pick or revert with the given author and
/// message, then clear the sequencer state. Errors if there are conflicts.
fn finalize_sequenced(
    repo: &Repository,
    author: &git2::Signature<'_>,
    message: &str,
) -> Result<(), GitError> {
    if repo.index()?.has_conflicts() {
        return Err(GitError::Conflict(
            "conflicts; resolve and commit, or reset --hard to abort".into(),
        ));
    }
    let committer = repo.signature()?;
    let tree = repo.find_tree(repo.index()?.write_tree()?)?;
    let head = repo.head()?.peel_to_commit()?;
    repo.commit(Some("HEAD"), author, &committer, message, &tree, &[&head])?;
    repo.cleanup_state()?;
    Ok(())
}

/// A short relative age (`5s`, `12m`, `3h`, `9d`) from a commit time to now.
/// Whether `commit`'s diff against its first parent touched `path`, matched as an
/// exact path or a directory prefix. A root commit is compared to the empty tree.
fn commit_touched_path(repo: &Repository, commit: &git2::Commit<'_>, path: &str) -> bool {
    let Ok(tree) = commit.tree() else {
        return false;
    };
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
        return false;
    };
    let prefix = format!("{path}/");
    (0..diff.deltas().len()).any(|i| {
        diff.get_delta(i)
            .and_then(|d| d.new_file().path().or_else(|| d.old_file().path()))
            .map(|p| {
                let s = p.to_string_lossy();
                s == path || s.starts_with(&prefix)
            })
            .unwrap_or(false)
    })
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
            if r.is_remote() {
                if let Some(oid) = r.target() {
                    remote_tips.push(oid);
                }
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
            hunks,
            binary,
        });
    }

    Ok(files)
}

fn line_origin(origin: char) -> LineOrigin {
    match origin {
        '+' => LineOrigin::Added,
        '-' => LineOrigin::Removed,
        ' ' => LineOrigin::Context,
        _ => LineOrigin::Meta,
    }
}
