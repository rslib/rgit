//! Copy-on-write workspaces: an instant, block-sharing clone of the entire repo
//! (working tree, build artifacts, .venvs, and .git) placed on a new branch, so
//! parallel agents get fully isolated trees without re-downloading dependencies
//! or rebuilding. Uses reflink where the filesystem supports it (APFS clonefile,
//! btrfs/XFS FICLONE, ReFS) and falls back to a full copy otherwise. Lives in
//! the backend so the CLI and TUI share it.

use std::path::{Path, PathBuf};

use crate::backend::GitBackend;
use crate::error::GitError;
use crate::git_repo::Git2Backend;

fn err(msg: impl Into<String>) -> GitError {
    GitError::Other(msg.into())
}

/// Where a repo's workspaces live. An explicit `$RGIT_WORKSPACE_DIR` is used
/// verbatim - the user chose the exact location. The default is a
/// `.rgit-workspaces` directory beside the repo root (same filesystem, so
/// reflink works), namespaced by the repo's own directory name so sibling repos
/// under the same parent each get their own pool instead of colliding in one
/// flat namespace.
fn workspaces_dir(repo_root: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("RGIT_WORKSPACE_DIR") {
        return PathBuf::from(dir);
    }
    let repo_name = repo_root
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("repo"));
    repo_root
        .parent()
        .unwrap_or(repo_root)
        .join(".rgit-workspaces")
        .join(repo_name)
}

/// Create a CoW clone of the repo on a new branch `name`.
pub fn create(backend: &dyn GitBackend, name: &str) -> Result<String, GitError> {
    // The name is both a directory and a branch, so validate it as both up front
    // (before the potentially large clone).
    if name.contains('/')
        || name.contains(std::path::MAIN_SEPARATOR)
        || !git2::Reference::is_valid_name(&format!("refs/heads/{name}"))
    {
        return Err(err(format!("invalid workspace/branch name: {name:?}")));
    }
    let root = backend.workdir().to_path_buf();
    let base = workspaces_dir(&root);
    let dest = base.join(name);
    if dest.exists() {
        return Err(err(format!("workspace already exists: {}", dest.display())));
    }
    std::fs::create_dir_all(&base)?;

    // Remove a half-made clone on any failure, so no orphan dir is left.
    let reflinked = match clone_tree(&root, &dest).and_then(|reflinked| {
        Git2Backend::discover(&dest)?.create_branch(name)?;
        Ok(reflinked)
    }) {
        Ok(reflinked) => reflinked,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e);
        }
    };

    let how = if reflinked {
        "reflink (copy-on-write, shares disk blocks)"
    } else {
        "full copy (filesystem has no reflink; consider APFS/btrfs/XFS)"
    };
    Ok(format!(
        "created workspace {name} at {dest}\n  clone: {how}\n  branch: {name}\n\
         cd {dest}\n\n\
         Share build caches across workspaces (add to your shell) so a fresh clone does not rebuild:\n  \
         export CCACHE_DIR=$HOME/.cache/ccache\n  \
         export UV_CACHE_DIR=$HOME/.cache/uv\n  \
         export CPM_SOURCE_CACHE=$HOME/.cache/cpm   # warm once single-threaded first",
        dest = dest.display()
    ))
}

/// One workspace: its name, checked-out branch (`?` if unreadable), and path.
/// `ahead` and `behind` count its commits the main repository's branch of the
/// same name lacks, and the reverse; `None` when that branch does not exist or
/// the history cannot be compared.
pub struct WorkspaceInfo {
    pub name: String,
    pub branch: String,
    pub path: PathBuf,
    pub ahead: Option<usize>,
    pub behind: Option<usize>,
}

/// This repo's workspaces, sorted by name.
pub fn entries(backend: &dyn GitBackend) -> Vec<WorkspaceInfo> {
    let Ok(dir) = std::fs::read_dir(workspaces_dir(backend.workdir())) else {
        return Vec::new();
    };
    let mut out: Vec<WorkspaceInfo> = dir
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|path| WorkspaceInfo {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            branch: Git2Backend::discover(&path)
                .ok()
                .and_then(|b| b.status().ok())
                .and_then(|s| s.head.branch)
                .unwrap_or_else(|| "?".to_owned()),
            ahead: None,
            behind: None,
            path,
        })
        .collect();
    for w in &mut out {
        (w.ahead, w.behind) = match divergence(backend, &w.path, &w.branch) {
            Some((ahead, behind)) => (Some(ahead), Some(behind)),
            None => (None, None),
        };
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The workspace's commits that the main repository's `branch` lacks, and the
/// reverse. The workspace's object store sees the main one through an
/// in-memory alternate, so commits made on either side since the clone count.
fn divergence(backend: &dyn GitBackend, ws: &Path, branch: &str) -> Option<(usize, usize)> {
    let main_tip = git2::Oid::from_str(&backend.branch_tip(branch).ok()??).ok()?;
    let repo = git2::Repository::open(ws).ok()?;
    let main = git2::Repository::open(backend.workdir()).ok()?;
    repo.odb()
        .ok()?
        .add_disk_alternate(main.path().join("objects").to_str()?)
        .ok()?;
    let tip = repo
        .find_reference(&format!("refs/heads/{branch}"))
        .ok()?
        .target()?;
    repo.graph_ahead_behind(tip, main_tip).ok()
}

pub fn list(backend: &dyn GitBackend) -> Result<String, GitError> {
    let list = entries(backend);
    if list.is_empty() {
        return Ok("no workspaces".to_owned());
    }
    Ok(list
        .iter()
        .map(|w| match (w.ahead, w.behind) {
            (Some(ahead), Some(behind)) => format!(
                "{}  [{}]  +{ahead} -{behind}  {}",
                w.name,
                w.branch,
                w.path.display()
            ),
            _ => format!("{}  [{}]  {}", w.name, w.branch, w.path.display()),
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Remove a workspace directory.
pub fn remove(backend: &dyn GitBackend, name: &str) -> Result<String, GitError> {
    let dest = workspaces_dir(backend.workdir()).join(name);
    if !dest.exists() {
        return Err(err(format!("no such workspace: {name}")));
    }
    std::fs::remove_dir_all(&dest)?;
    Ok(format!("removed workspace {name}"))
}

/// Clone `src` to `dst` copy-on-write. Returns whether reflink was used (vs a
/// full copy). On macOS `clonefile` clones the tree in one call; elsewhere the
/// reflink primitive is single-file, so walk and clone each file.
fn clone_tree(src: &Path, dst: &Path) -> Result<bool, GitError> {
    #[cfg(target_os = "macos")]
    {
        Ok(reflink_copy::reflink_or_copy(src, dst)?.is_none())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut all_reflinked = true;
        for entry in walkdir::WalkDir::new(src).follow_links(false) {
            let entry = entry.map_err(|e| err(e.to_string()))?;
            let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
            let target = dst.join(rel);
            let ft = entry.file_type();
            if ft.is_dir() {
                std::fs::create_dir_all(&target)?;
            } else if ft.is_symlink() {
                let link = std::fs::read_link(entry.path())?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(link, &target)?;
                #[cfg(not(unix))]
                let _ = link;
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if reflink_copy::reflink_or_copy(entry.path(), &target)?.is_some() {
                    all_reflinked = false;
                }
            }
        }
        Ok(all_reflinked)
    }
}

/// A workspace sync that moved one branch.
pub struct Transfer {
    pub workspace: String,
    pub branch: String,
    pub from: String,
    pub to: String,
    pub commits: usize,
}

/// Why a sync did not run: a state the user fixes, or a git failure.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("{message}")]
    Refused { message: String, help: String },
    #[error(transparent)]
    Git(#[from] GitError),
}

fn open(backend: &dyn GitBackend, name: &str) -> Result<(PathBuf, Git2Backend), SyncError> {
    let path = workspaces_dir(backend.workdir()).join(name);
    if !path.is_dir() {
        return Err(err(format!("no such workspace: {name}")).into());
    }
    let ws = Git2Backend::discover(&path)?;
    Ok((path, ws))
}

fn current_branch(backend: &dyn GitBackend) -> Result<String, SyncError> {
    Ok(backend
        .status()?
        .head
        .branch
        .ok_or(GitError::DetachedHead)?)
}

/// Fast-forward `from`'s `src_branch` into `to`'s `dst_branch`, creating the
/// branch when `to` lacks it. `label` names `to`'s side in a refusal and
/// `retry` is the command that brings `to` up to date.
struct Sync<'a> {
    from: &'a dyn GitBackend,
    from_path: &'a Path,
    to: &'a dyn GitBackend,
    src_branch: &'a str,
    dst_branch: &'a str,
    workspace: &'a str,
    to_label: &'a str,
    from_label: &'a str,
    retry: String,
}

impl Sync<'_> {
    fn run(&self) -> Result<Transfer, SyncError> {
        let refuse = |message: String, help: String| SyncError::Refused { message, help };
        let tip = self.from.branch_tip(self.src_branch)?.ok_or_else(|| {
            err(format!(
                "no branch {} in the {}",
                self.src_branch, self.from_label
            ))
        })?;

        let checked_out = current_branch(self.to).is_ok_and(|b| b == self.dst_branch);
        if checked_out && self.to.status()?.entries.iter().any(|e| !e.is_untracked()) {
            return Err(refuse(
                format!(
                    "the {} has uncommitted changes on {}",
                    self.to_label, self.dst_branch
                ),
                "Run `rgit commit` or `rgit stash push` there first".to_owned(),
            ));
        }

        let from_path = self
            .from_path
            .to_str()
            .ok_or_else(|| err("workspace path is not UTF-8"))?;
        self.to.fetch(
            Some(from_path),
            &[format!("refs/heads/{}", self.src_branch)],
            &crate::FetchArgs::default(),
            &|_| {},
        )?;

        let current = self.to.branch_tip(self.dst_branch)?;
        let (commits, old) = match &current {
            Some(old) => {
                let (ahead, behind) = self.to.ahead_behind(&tip, old)?;
                if behind > 0 {
                    return Err(refuse(
                        format!(
                            "{} has {behind} commits the {} does not",
                            self.dst_branch, self.from_label
                        ),
                        format!("{} or rgit merge", self.retry),
                    ));
                }
                (ahead, old.clone())
            }
            None => (self.to.ahead_behind(&tip, "HEAD")?.0, "0".repeat(tip.len())),
        };

        if commits > 0 {
            let reference = format!("refs/heads/{}", self.dst_branch);
            match checked_out {
                true => self.to.merge(&tip, false, true, &|_| {})?,
                false => self.to.update_ref(
                    &reference,
                    Some(&tip),
                    Some(&old),
                    false,
                    Some(&format!("workspace {}: fast-forward", self.workspace)),
                )?,
            }
        }
        Ok(Transfer {
            workspace: self.workspace.to_owned(),
            branch: self.dst_branch.to_owned(),
            from: old,
            to: tip,
            commits,
        })
    }
}

/// Fast-forward the main repository's branch (the workspace's current branch,
/// or `into`) to the workspace's.
pub fn pull(
    backend: &dyn GitBackend,
    name: &str,
    into: Option<&str>,
) -> Result<Transfer, SyncError> {
    let (path, ws) = open(backend, name)?;
    let branch = current_branch(&ws)?;
    Sync {
        from: &ws,
        from_path: &path,
        to: backend,
        src_branch: &branch,
        dst_branch: into.unwrap_or(&branch),
        workspace: name,
        to_label: "main repository",
        from_label: "workspace",
        retry: format!("rgit workspace push {name}"),
    }
    .run()
}

/// Fast-forward the workspace's current branch to the main repository's branch
/// of the same name.
pub fn push(backend: &dyn GitBackend, name: &str) -> Result<Transfer, SyncError> {
    let (_, ws) = open(backend, name)?;
    let branch = current_branch(&ws)?;
    Sync {
        from: backend,
        from_path: backend.workdir(),
        to: &ws,
        src_branch: &branch,
        dst_branch: &branch,
        workspace: name,
        to_label: "workspace",
        from_label: "main repository",
        retry: format!("rgit workspace pull {name}"),
    }
    .run()
}
