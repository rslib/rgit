use std::path::{Path, PathBuf};
use std::sync::Mutex;

use git2::build::CheckoutBuilder;
use git2::{
    ApplyLocation, ApplyOptions, BranchType, Cred, CredentialType, Diff, DiffOptions, ErrorCode,
    FetchOptions, ObjectType, Patch, PushOptions, RemoteCallbacks, Repository, ResetType, Status,
    StatusOptions,
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
        })
    }

    /// Run a `git` CLI command in the working directory, returning its stdout on
    /// success or its stderr as a `Cli` error. Used only for the few operations
    /// libgit2 cannot do (undo, rebase continue/skip, bisect).
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

impl GitBackend for Git2Backend {
    fn workdir(&self) -> &Path {
        &self.workdir
    }

    fn status(&self) -> Result<RepoStatus, GitError> {
        let mut repo = self.repo.lock().expect("repo mutex");
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

    fn stash_push(&self) -> Result<(), GitError> {
        let mut repo = self.repo.lock().expect("repo mutex");
        let sig = repo.signature()?;
        repo.stash_save2(&sig, None, Some(git2::StashFlags::INCLUDE_UNTRACKED))?;
        Ok(())
    }

    fn stash_push_message(&self, message: &str) -> Result<(), GitError> {
        let mut repo = self.repo.lock().expect("repo mutex");
        let sig = repo.signature()?;
        repo.stash_save2(
            &sig,
            Some(message),
            Some(git2::StashFlags::INCLUDE_UNTRACKED),
        )?;
        Ok(())
    }

    fn stash_pop(&self, index: usize) -> Result<(), GitError> {
        let mut repo = self.repo.lock().expect("repo mutex");
        repo.stash_pop(index, None)?;
        Ok(())
    }

    fn stash_apply(&self, index: usize) -> Result<(), GitError> {
        let mut repo = self.repo.lock().expect("repo mutex");
        repo.stash_apply(index, None)?;
        Ok(())
    }

    fn stash_drop(&self, index: usize) -> Result<(), GitError> {
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
        } else {
            walk.push_head().is_ok()
        };
        if !seeded {
            return Ok(Vec::new());
        }
        walk.set_sorting(git2::Sort::TIME)?;

        let author_needle = opts.author.as_ref().map(|a| a.to_lowercase());
        let mut entries = Vec::new();
        for oid in walk {
            if entries.len() >= opts.limit {
                break;
            }
            let commit = repo.find_commit(oid?)?;
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
            });
        }
        Ok(entries)
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
            author: commit.author().name().unwrap_or("?").to_owned(),
            email: commit.author().email().unwrap_or("").to_owned(),
            when: relative_age(commit.time().seconds(), now),
            message: commit.message().unwrap_or("").trim_end().to_owned(),
            files: extract(&diff)?,
        })
    }

    fn diff_refs(&self, from: &str, to: &str) -> Result<Vec<crate::FileDiff>, GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let from_tree = repo.revparse_single(from)?.peel_to_tree()?;
        let to_tree = repo.revparse_single(to)?.peel_to_tree()?;
        let mut opts = DiffOptions::new();
        let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), Some(&mut opts))?;
        extract(&diff)
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
        let mut index = repo.index()?;
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        Ok(())
    }

    fn unstage_all(&self) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
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
        let mut dopts = DiffOptions::new();
        dopts.pathspec(path);
        let diff = repo.diff_index_to_workdir(None, Some(&mut dopts))?;
        apply_one_hunk(&repo, &diff, path, new_start)
    }

    fn unstage_hunk(&self, path: &str, new_start: u32) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
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

    fn checkout_detached(&self, rev: &str) -> Result<(), GitError> {
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

    fn rebase_onto(&self, rev: &str, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let onto = repo.revparse_single(rev)?.peel_to_commit()?;
        let upstream = repo.find_annotated_commit(onto.id())?;
        let mut rebase = repo.rebase(None, Some(&upstream), None, None)?;
        let sig = repo.signature()?;
        while let Some(op) = rebase.next() {
            let op = op?;
            // git prints "Applying: <subject>" for each replayed commit.
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

    fn rebase_abort(&self) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.open_rebase(None)?.abort()?;
        Ok(())
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

    fn undo(&self) -> Result<(), GitError> {
        self.run_git(&["reset", "--keep", "HEAD@{1}"], &[])
            .map(drop)
    }

    fn bisect(&self, args: &[String]) -> Result<String, GitError> {
        let mut argv = vec!["bisect"];
        argv.extend(args.iter().map(String::as_str));
        self.run_git(&argv, &[])
    }

    fn clean(&self) -> Result<(), GitError> {
        self.run_git(&["clean", "-fd"], &[]).map(drop)
    }

    fn remove_path(&self, path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let mut index = repo.index()?;
        index.remove_path(Path::new(path))?;
        index.write()?;
        let full = self.workdir.join(path);
        if full.exists() {
            std::fs::remove_file(full)?;
        }
        Ok(())
    }

    fn move_path(&self, from: &str, to: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
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

    fn cherry_pick(&self, rev: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let source = repo.revparse_single(rev)?.peel_to_commit()?;
        repo.cherrypick(&source, None)?;
        finalize_sequenced(&repo, &source.author(), source.message().unwrap_or(""))
    }

    fn revert(&self, rev: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let source = repo.revparse_single(rev)?.peel_to_commit()?;
        repo.revert(&source, None)?;
        let summary = source.summary().ok().flatten().unwrap_or("commit");
        let message = format!(
            "Revert \"{summary}\"\n\nThis reverts commit {}.",
            source.id()
        );
        finalize_sequenced(&repo, &repo.signature()?, &message)
    }

    fn merge(&self, rev: &str, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let source = repo.revparse_single(rev)?.peel_to_commit()?;
        let annotated = repo.find_annotated_commit(source.id())?;
        let (analysis, _) = repo.merge_analysis(&[&annotated])?;

        if analysis.is_up_to_date() {
            report(OpProgress::Line("Already up to date.".to_owned()));
            return Ok(());
        }
        if analysis.is_fast_forward() {
            if let Some(old) = repo.head().ok().and_then(|h| h.target()) {
                report(OpProgress::Line(format!(
                    "Updating {}..{}",
                    short7(old),
                    short7(source.id())
                )));
            }
            let name = repo.head()?.name().unwrap_or("HEAD").to_owned();
            repo.reference(&name, source.id(), true, "merge: fast-forward")?;
            repo.set_head(&name)?;
            repo.checkout_head(Some(CheckoutBuilder::new().force()))?;
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

    fn resolve_conflict(&self, path: &str, ours: bool) -> Result<(), GitError> {
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

    fn delete_branch(&self, name: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        repo.find_branch(name, BranchType::Local)?.delete()?;
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

    fn rename_branch(&self, old: &str, new: &str) -> Result<(), GitError> {
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

    fn checkout_branch(&self, name: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        checkout(&repo, name)
    }

    fn create_branch(&self, name: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let head = repo.head()?.peel_to_commit()?;
        repo.branch(name, &head, false)?;
        checkout(&repo, name)
    }

    fn discard_file(&self, path: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
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
        let repo = self.repo.lock().expect("repo mutex");
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
        let repo = self.repo.lock().expect("repo mutex");
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
        let repo = self.repo.lock().expect("repo mutex");
        let parents = match repo.head() {
            Ok(head_ref) => vec![head_ref.peel_to_commit()?],
            Err(_) => Vec::new(),
        };
        make_commit(&repo, message, &parents, true, true)
    }

    fn amend(&self, message: &str) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        make_amend(&repo, message, true)
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
        let repo = self.repo.lock().expect("repo mutex");
        let parents = match repo.head() {
            Ok(head_ref) => vec![head_ref.peel_to_commit()?],
            Err(_) => Vec::new(),
        };
        make_commit(&repo, message, &parents, true, false)
    }

    fn amend_no_verify(&self, message: &str) -> Result<(), GitError> {
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

    fn fetch(&self, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let (remote, _) = upstream_remote(&repo)?;
        do_fetch(&repo, &remote, report)
    }

    fn pull(&self, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let (remote, branch) = upstream_remote(&repo)?;
        do_fetch(&repo, &remote, report)?;
        fast_forward(&repo, &remote, &branch, report)
    }

    fn push(
        &self,
        force: bool,
        force_with_lease: bool,
        set_upstream: bool,
        report: &dyn Fn(OpProgress),
    ) -> Result<(), GitError> {
        let repo = self.repo.lock().expect("repo mutex");
        let (remote_name, branch) = upstream_remote(&repo)?;
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
        let mut callbacks = remote_callbacks(report, &rejected);
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

fn do_fetch(repo: &Repository, remote: &str, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
    let mut remote = repo.find_remote(remote)?;
    if let Ok(url) = remote.url() {
        report(OpProgress::Line(format!("From {url}")));
    }
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(remote_callbacks(report, &ignored));
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
pub fn init(path: &Path) -> Result<(), GitError> {
    Repository::init(path)?;
    Ok(())
}

/// Clone `url` into `path` (`git clone`), via libgit2, using the same
/// credentials as fetch/push and reporting git-style progress.
pub fn clone(url: &str, path: &Path, report: &dyn Fn(OpProgress)) -> Result<(), GitError> {
    let ignored = std::sync::atomic::AtomicBool::new(false);
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(remote_callbacks(report, &ignored));
    git2::build::RepoBuilder::new()
        .fetch_options(opts)
        .clone(url, path)?;
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
fn remote_callbacks<'a>(
    report: &'a dyn Fn(OpProgress),
    rejected: &'a std::sync::atomic::AtomicBool,
) -> RemoteCallbacks<'a> {
    let mut cb = RemoteCallbacks::new();
    cb.credentials(|url, username, allowed| {
        if allowed.contains(CredentialType::SSH_KEY) {
            if let Some(user) = username {
                return Cred::ssh_key_from_agent(user);
            }
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            if let Ok(config) = git2::Config::open_default() {
                return Cred::credential_helper(&config, url, username);
            }
        }
        if allowed.contains(CredentialType::USERNAME) {
            if let Some(user) = username {
                return Cred::username(user);
            }
        }
        Cred::default()
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
    let mut msg = message.to_owned();
    if verify {
        run_commit_hooks(repo, &mut msg)?;
    }

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

/// Rewrite HEAD from the index, keeping its parents. Runs the commit hooks when
/// `verify` is set.
fn make_amend(repo: &Repository, message: &str, verify: bool) -> Result<(), GitError> {
    let head = repo.head()?.peel_to_commit()?;
    let mut msg = message.to_owned();
    if verify {
        run_commit_hooks(repo, &mut msg)?;
    }

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
    let Ok(upstream) = local.upstream() else {
        return;
    };
    if let Ok(Some(name)) = upstream.name() {
        head.upstream = Some(name.to_owned());
    }
    if let (Some(local_oid), Some(up_oid)) = (local.get().target(), upstream.get().target()) {
        if let Ok((ahead, behind)) = repo.graph_ahead_behind(local_oid, up_oid) {
            head.ahead = ahead;
            head.behind = behind;
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

fn collect_recent(repo: &Repository) -> Vec<Commit> {
    let Ok(mut walk) = repo.revwalk() else {
        return Vec::new();
    };
    // Fails on an unborn branch; an empty log is the right answer there.
    if walk.push_head().is_err() {
        return Vec::new();
    }
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
