use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use git2::{Delta, DiffFindOptions, DiffOptions, Repository, Status, StatusOptions, StatusShow};
use rayon::prelude::*;

use crate::diff::FileDiff;
use crate::error::GitError;
use crate::model::{StatusCode, StatusEntry};

/// A full snapshot hashes untracked files' contents (recursing into new
/// directories) so a brand-new file shows its all-added diff in the preview,
/// like `git diff` with --no-index would; past this many untracked files (an
/// unignored build dir, say) they are left out and `file_diff` loads one on
/// demand. A lazy snapshot reads deltas only.
const EAGER_UNTRACKED: usize = 200;

/// How backend status reports untracked files: one `<folder>/` entry per
/// folder without tracked files, every file, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UntrackedMode {
    /// As `git status` shows it: collapsed folders.
    #[default]
    Normal,
    /// Every untracked file, as before.
    All,
    /// No untracked entries.
    No,
}

impl UntrackedMode {
    /// Parse `status.showUntrackedFiles` (or `-u`) as wt_status does:
    /// unset or `normal` is Normal, `all` is All, `no` or false is No.
    pub fn from_config(value: Option<&str>) -> Result<Self, GitError> {
        match value {
            None | Some("normal") => Ok(UntrackedMode::Normal),
            Some("all") => Ok(UntrackedMode::All),
            Some("no") => Ok(UntrackedMode::No),
            Some(v) => match bool_value(v) {
                Some(true) => Ok(UntrackedMode::Normal),
                Some(false) => Ok(UntrackedMode::No),
                None => Err(GitError::Other(format!(
                    "Invalid untracked files mode '{v}'"
                ))),
            },
        }
    }

    pub(crate) fn include_untracked(self) -> bool {
        self != UntrackedMode::No
    }

    pub(crate) fn recurse_untracked_dirs(self) -> bool {
        self == UntrackedMode::All
    }
}

fn bool_value(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// The status pieces `refresh` computes: changed paths plus the worktree-side
/// and staged diffs, in the same order as the single libgit2 calls.
#[derive(Debug, Clone)]
pub struct StatusTriple {
    pub entries: Vec<StatusEntry>,
    pub unstaged: Vec<FileDiff>,
    pub staged: Vec<FileDiff>,
}

/// The single-call reference: the exact calls `status` ran before sharding.
pub(crate) fn refresh_single(
    repo: &Repository,
    full: bool,
    untracked: UntrackedMode,
) -> Result<StatusTriple, GitError> {
    let entries = crate::git_repo::collect_entries(repo, untracked)?;
    let eager_untracked =
        full && entries.iter().filter(|e| e.is_untracked()).count() <= EAGER_UNTRACKED;
    let mut unstaged = if full {
        crate::git_repo::extract_with_renames_par(
            repo,
            crate::git_repo::StatusDiff::Workdir {
                eager_untracked,
                untracked,
            },
        )?
    } else {
        crate::git_repo::extract_meta(
            repo,
            crate::git_repo::StatusDiff::Workdir {
                eager_untracked,
                untracked,
            },
        )?
    };
    drop_collapsed(&mut unstaged);
    crate::git_repo::drop_skipped(repo, &mut unstaged)?;
    let staged = if full {
        crate::git_repo::extract_with_renames_par(repo, crate::git_repo::StatusDiff::Staged)?
    } else {
        crate::git_repo::extract_meta(repo, crate::git_repo::StatusDiff::Staged)?
    };
    Ok(StatusTriple {
        entries,
        unstaged,
        staged,
    })
}

/// How to open a repository handle that sees what the caller sees: the same
/// git dir, workdir, index file and `-c` config layer. A plain open of the
/// git dir loses all four (GIT_WORK_TREE, a detached GIT_DIR, GIT_INDEX_FILE
/// and command-line config), so every handle here opens through this.
#[doc(hidden)]
pub struct Opener {
    pub gitdir: PathBuf,
    pub workdir: PathBuf,
    pub index_file: Option<PathBuf>,
}

impl Opener {
    /// Capture how `repo` was opened; `workdir` is the backend's workdir,
    /// which may differ from what the git dir derives on its own.
    #[doc(hidden)]
    pub fn from_repo(repo: &Repository, workdir: &Path) -> Result<Self, GitError> {
        Ok(Opener {
            gitdir: repo.path().to_path_buf(),
            workdir: workdir.to_path_buf(),
            index_file: repo.index()?.path().map(|p| p.to_path_buf()),
        })
    }

    fn open(&self) -> Result<Repository, GitError> {
        let repo = Repository::open(&self.gitdir)?;
        repo.set_workdir(&self.workdir, false)?;
        if let Some(path) = &self.index_file {
            let mut index = git2::Index::open(path)?;
            repo.set_index(&mut index)?;
        }
        crate::config::add_command_line(&repo.config()?)?;
        Ok(repo)
    }
}

/// The single-call reference above, on a handle opened like the caller's.
/// Test-only: the integration suite asserts this equals `debug_sharded`.
#[doc(hidden)]
pub fn debug_single(
    gitdir: &Path,
    workdir: Option<&Path>,
    index_file: Option<&Path>,
    untracked: UntrackedMode,
    full: bool,
) -> Result<StatusTriple, GitError> {
    refresh_single(
        &debug_opener(gitdir, workdir, index_file)?.open()?,
        full,
        untracked,
    )
}

/// The sharded worktree walk, on handles opened like the caller's.
/// Test-only: the integration suite asserts this equals `debug_single`.
#[doc(hidden)]
pub fn debug_sharded(
    gitdir: &Path,
    workdir: Option<&Path>,
    index_file: Option<&Path>,
    untracked: UntrackedMode,
    full: bool,
) -> Result<StatusTriple, GitError> {
    refresh(&debug_opener(gitdir, workdir, index_file)?, full, untracked)
}

fn debug_opener(
    gitdir: &Path,
    workdir: Option<&Path>,
    index_file: Option<&Path>,
) -> Result<Opener, GitError> {
    let probe = Repository::open(gitdir)?;
    let workdir = match workdir {
        Some(path) => path.to_path_buf(),
        None => probe
            .workdir()
            .ok_or_else(|| GitError::Bare(probe.path().to_path_buf()))?
            .to_path_buf(),
    };
    Ok(Opener {
        gitdir: gitdir.to_path_buf(),
        workdir,
        index_file: index_file.map(|p| p.to_path_buf()),
    })
}

/// Status with the worktree side sharded across rayon threads. Falls back to
/// the single call when there is nothing to shard (fewer than 2 top-level
/// entries) or no second thread to run it on. Shard threads open their own
/// handles through the opener and never touch the caller's repository handle.
pub(crate) fn refresh(
    opener: &Opener,
    full: bool,
    untracked: UntrackedMode,
) -> Result<StatusTriple, GitError> {
    let probe = opener.open()?;
    let shards = plan_shards(&probe, &opener.workdir)?;
    let tops: usize = shards.iter().map(Vec::len).sum();
    if tops < 2 || rayon::current_num_threads() < 2 {
        return refresh_single(&probe, full, untracked);
    }
    refresh_sharded(&probe, opener, &shards, full, untracked)
}

/// One shard's literal pathspec: top-level file names, or top-level directory
/// names (which match everything below them).
fn plan_shards(repo: &Repository, workdir: &Path) -> Result<Vec<Vec<String>>, GitError> {
    let mut files = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    for entry in std::fs::read_dir(workdir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            dirs.insert(name);
        } else {
            files.insert(name);
        }
    }
    // Deleted paths are gone from disk, so union the index's top-level names
    // in; without them a removed directory's deletions would lose their shard.
    let index = repo.index()?;
    let mut exact = BTreeSet::new();
    let mut comps = BTreeSet::new();
    for entry in index.iter() {
        let path = String::from_utf8_lossy(&entry.path).into_owned();
        exact.insert(path.clone());
        if let Some(top) = path.split('/').next() {
            comps.insert(top.to_owned());
        }
    }
    for top in comps {
        if files.contains(&top) || dirs.contains(&top) {
            continue;
        }
        if exact.contains(&top) {
            files.insert(top);
        } else {
            dirs.insert(top);
        }
    }
    let mut shards = Vec::new();
    if !files.is_empty() {
        shards.push(files.into_iter().collect());
    }
    // About 2 shards per thread; each shard's paths share one statuses call.
    let target = 2 * rayon::current_num_threads().max(1);
    let count = dirs.len().min(target).max(1);
    let dirs: Vec<String> = dirs.into_iter().collect();
    if !dirs.is_empty() {
        for chunk in dirs.chunks(dirs.len().div_ceil(count)) {
            shards.push(chunk.to_vec());
        }
    }
    Ok(shards)
}

/// The HEAD-to-index side on one handle: staged flags plus renames, which can
/// cross shards so they stay unsharded. Reads no worktree files.
fn index_side(repo: &Repository) -> Result<BTreeMap<String, (Status, Option<String>)>, GitError> {
    let mut opts = StatusOptions::new();
    opts.show(StatusShow::Index).renames_head_to_index(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let mut out = BTreeMap::new();
    for entry in statuses.iter() {
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
        let orig = rename
            .and_then(|d| d.old_file().path())
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|old| *old != path);
        out.insert(path, (entry.status(), orig));
    }
    Ok(out)
}

/// The worktree side, one statuses call per shard on that shard's own handle.
fn workdir_side(
    opener: &Opener,
    shards: &[Vec<String>],
    untracked: UntrackedMode,
) -> Result<Vec<BTreeMap<String, Status>>, GitError> {
    shards
        .par_iter()
        .map(|paths| {
            let repo = opener.open()?;
            let mut opts = StatusOptions::new();
            opts.show(StatusShow::Workdir)
                .include_untracked(untracked.include_untracked())
                .recurse_untracked_dirs(untracked.recurse_untracked_dirs())
                .disable_pathspec_match(true);
            for path in paths {
                opts.pathspec(path.as_str());
            }
            let statuses = repo.statuses(Some(&mut opts))?;
            let mut out = BTreeMap::new();
            for entry in statuses.iter() {
                if let Ok(path) = entry.path() {
                    out.insert(path.to_owned(), entry.status());
                }
            }
            Ok(out)
        })
        .collect()
}

/// Combine the single index map with the sharded worktree maps, applying the
/// same ignored/sparse rules as the single call, sorted by path.
fn merge_entries(
    repo: &Repository,
    index: &BTreeMap<String, (Status, Option<String>)>,
    workdir: &[BTreeMap<String, Status>],
    paths: BTreeSet<&str>,
) -> Result<Vec<StatusEntry>, GitError> {
    let skips = crate::sparse::skipped_paths(&repo.index()?);
    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        let mut status = Status::empty();
        let mut orig = None;
        if let Some((index_status, index_orig)) = index.get(path) {
            status |= *index_status;
            orig = index_orig.clone();
        }
        for shard in workdir {
            if let Some(worktree_status) = shard.get(path) {
                status |= *worktree_status;
            }
        }
        if status.contains(Status::IGNORED) {
            continue;
        }
        if status.contains(Status::WT_DELETED) && skips.contains(path) {
            status.remove(Status::WT_DELETED);
            if status.is_empty() {
                continue;
            }
        }
        entries.push(StatusEntry {
            path: path.to_owned(),
            orig_path: orig,
            index: crate::git_repo::index_code(status),
            worktree: crate::git_repo::worktree_code(status),
        });
    }
    Ok(entries)
}

fn refresh_sharded(
    probe: &Repository,
    opener: &Opener,
    shards: &[Vec<String>],
    full: bool,
    untracked: UntrackedMode,
) -> Result<StatusTriple, GitError> {
    let index_map = index_side(probe)?;
    let maps = workdir_side(opener, shards, untracked)?;
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    for map in &maps {
        paths.extend(map.keys().map(String::as_str));
    }
    paths.extend(index_map.keys().map(String::as_str));
    let entries = merge_entries(probe, &index_map, &maps, paths)?;
    let eager_untracked =
        full && entries.iter().filter(|e| e.is_untracked()).count() <= EAGER_UNTRACKED;
    let mut unstaged = unstaged_sharded(opener, shards, full, eager_untracked, untracked)?;
    drop_collapsed(&mut unstaged);
    unstaged = pair_workdir_renames(opener, unstaged, full, eager_untracked, untracked)?;
    crate::git_repo::drop_skipped(probe, &mut unstaged)?;
    let staged = if full {
        crate::git_repo::extract_with_renames_par(probe, crate::git_repo::StatusDiff::Staged)?
    } else {
        crate::git_repo::extract_meta(probe, crate::git_repo::StatusDiff::Staged)?
    };
    Ok(StatusTriple {
        entries,
        unstaged,
        staged,
    })
}

/// The worktree diff, one literal-pathspec diff per shard on that shard's own
/// handle, with no rename detection; pairing happens once after the merge.
fn unstaged_sharded(
    opener: &Opener,
    shards: &[Vec<String>],
    full: bool,
    eager_untracked: bool,
    untracked: UntrackedMode,
) -> Result<Vec<FileDiff>, GitError> {
    let lists: Vec<Vec<FileDiff>> = shards
        .par_iter()
        .map(|paths| {
            let repo = opener.open()?;
            let mut opts = DiffOptions::new();
            if eager_untracked && untracked.include_untracked() {
                opts.include_untracked(true)
                    .recurse_untracked_dirs(untracked.recurse_untracked_dirs())
                    .show_untracked_content(true);
            }
            opts.disable_pathspec_match(true);
            for path in paths {
                opts.pathspec(path.as_str());
            }
            let diff = repo.diff_index_to_workdir(None, Some(&mut opts))?;
            if full {
                crate::git_repo::extract(&repo, &diff)
            } else {
                let mut files = Vec::with_capacity(diff.deltas().len());
                for delta in diff.deltas() {
                    files.push(crate::git_repo::delta_meta(&repo, &delta));
                }
                Ok(files)
            }
        })
        .collect::<Result<Vec<_>, GitError>>()?;
    let mut merged: Vec<FileDiff> = lists.into_iter().flatten().collect();
    merged.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(merged)
}

/// Pair worktree renames once, on one diff limited to the merged deleted and
/// added paths; per-shard pairing could miss a match in another shard.
fn pair_workdir_renames(
    opener: &Opener,
    mut merged: Vec<FileDiff>,
    full: bool,
    eager_untracked: bool,
    untracked: UntrackedMode,
) -> Result<Vec<FileDiff>, GitError> {
    let has_deleted = merged.iter().any(|f| f.status == StatusCode::Deleted);
    let has_added = merged.iter().any(|f| f.status == StatusCode::Added);
    if !has_deleted || !has_added {
        return Ok(merged);
    }
    let repo = opener.open()?;
    let mut opts = DiffOptions::new();
    if eager_untracked && untracked.include_untracked() {
        opts.include_untracked(true)
            .recurse_untracked_dirs(untracked.recurse_untracked_dirs())
            .show_untracked_content(true);
    }
    opts.disable_pathspec_match(true);
    for file in &merged {
        if matches!(file.status, StatusCode::Deleted | StatusCode::Added) {
            opts.pathspec(file.path.as_str());
        }
    }
    let mut diff = repo.diff_index_to_workdir(None, Some(&mut opts))?;
    let mut fopts = DiffFindOptions::new();
    fopts.renames(true);
    diff.find_similar(Some(&mut fopts))?;
    let paired: Vec<FileDiff> = if full {
        crate::git_repo::extract(&repo, &diff)?
    } else {
        diff.deltas()
            .filter(|d| matches!(d.status(), Delta::Renamed | Delta::Copied))
            .map(|d| crate::git_repo::delta_meta(&repo, &d))
            .collect()
    };
    let renames: Vec<FileDiff> = paired
        .into_iter()
        .filter(|f| f.old_path.is_some())
        .collect();
    if renames.is_empty() {
        return Ok(merged);
    }
    splice_renames(&mut merged, &renames);
    Ok(merged)
}

/// Drop the per-folder deltas a collapsed walk reports: the entry carries
/// the folder, so the snapshot holds no diff text for it.
fn drop_collapsed(files: &mut Vec<FileDiff>) {
    files.retain(|f| !f.path.ends_with('/'));
}

/// Splice paired renames into the merged list, dropping the delete/add pair
/// each replaces. Removes the larger index first so the smaller stays valid.
fn splice_renames(merged: &mut Vec<FileDiff>, renames: &[FileDiff]) {
    for rename in renames {
        let old = rename.old_path.as_deref().unwrap_or_default();
        let (Some(delete), Some(add)) = (
            merged
                .iter()
                .position(|f| f.path == old && f.status == StatusCode::Deleted),
            merged
                .iter()
                .position(|f| f.path == rename.path && f.status == StatusCode::Added),
        ) else {
            continue;
        };
        let (first, second) = if delete > add {
            (delete, add)
        } else {
            (add, delete)
        };
        merged.remove(first);
        merged.remove(second);
        merged.push(rename.clone());
    }
    merged.sort_by(|a, b| a.path.cmp(&b.path));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StatusCode;

    fn diff(path: &str, status: StatusCode) -> FileDiff {
        FileDiff {
            path: path.to_owned(),
            old_path: None,
            status,
            hunks: Vec::new(),
            binary: false,
            header: String::new(),
            similarity: 0,
            sizes: (0, 0),
            modes: (0, 0),
            ids: (String::new(), String::new()),
            loaded: false,
        }
    }

    #[test]
    fn splice_drops_pair_when_add_sorts_first() {
        let mut merged = vec![
            diff("new.txt", StatusCode::Added),
            diff("old.txt", StatusCode::Deleted),
        ];
        let mut rename = diff("new.txt", StatusCode::Renamed);
        rename.old_path = Some("old.txt".to_owned());
        splice_renames(&mut merged, &[rename]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].path, "new.txt");
        assert_eq!(merged[0].status, StatusCode::Renamed);
        assert_eq!(merged[0].old_path.as_deref(), Some("old.txt"));
    }

    #[test]
    fn splice_drops_pair_when_delete_sorts_first() {
        let mut merged = vec![
            diff("gone.txt", StatusCode::Deleted),
            diff("kept.txt", StatusCode::Modified),
            diff("zeta.txt", StatusCode::Added),
        ];
        let mut rename = diff("zeta.txt", StatusCode::Renamed);
        rename.old_path = Some("gone.txt".to_owned());
        splice_renames(&mut merged, &[rename]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].path, "kept.txt");
        assert_eq!(merged[1].path, "zeta.txt");
        assert_eq!(merged[1].status, StatusCode::Renamed);
    }
}
