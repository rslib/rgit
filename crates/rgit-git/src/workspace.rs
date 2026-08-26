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

    let reflinked = clone_tree(&root, &dest)?;

    let clone = Git2Backend::discover(&dest)?;
    // Clean up the clone if branching fails, so we do not leave an orphan dir.
    if let Err(e) = clone.create_branch(name) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }

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

/// List the repo's workspaces with their current branch.
pub fn list(backend: &dyn GitBackend) -> Result<String, GitError> {
    let base = workspaces_dir(backend.workdir());
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Ok("no workspaces".to_owned());
    };
    let mut lines = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let branch = Git2Backend::discover(&path)
            .ok()
            .and_then(|b| b.status().ok())
            .and_then(|s| s.head.branch)
            .unwrap_or_else(|| "?".to_owned());
        lines.push(format!("{name}  [{branch}]  {}", path.display()));
    }
    if lines.is_empty() {
        Ok("no workspaces".to_owned())
    } else {
        lines.sort();
        Ok(lines.join("\n"))
    }
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
