use std::path::{Path, PathBuf};

use crate::settings::RepoDirs;

impl RepoDirs {
    /// Find the repo around `start` by walking up to a `.git` folder or file.
    ///
    /// Honors `GIT_DIR` and `GIT_WORK_TREE`. Returns `None` outside a repo.
    pub fn discover(start: &Path) -> Option<RepoDirs> {
        Self::discover_with(start, &|name| std::env::var(name).ok())
    }

    pub fn discover_with(start: &Path, env: &dyn Fn(&str) -> Option<String>) -> Option<RepoDirs> {
        if let Some(git_dir) = env("GIT_DIR") {
            let git_dir = absolute(start, Path::new(&git_dir));
            return Some(RepoDirs {
                worktree_root: env("GIT_WORK_TREE").map(|tree| absolute(start, Path::new(&tree))),
                common_dir: common_dir(&git_dir),
            });
        }
        for folder in start.ancestors() {
            let dot_git = folder.join(".git");
            let git_dir = if dot_git.is_dir() {
                dot_git
            } else if dot_git.is_file() {
                let text = std::fs::read_to_string(&dot_git).ok()?;
                let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?;
                absolute(folder, Path::new(target.trim()))
            } else {
                continue;
            };
            return Some(RepoDirs {
                worktree_root: Some(folder.to_owned()),
                common_dir: common_dir(&git_dir),
            });
        }
        None
    }
}

fn absolute(base: &Path, path: &Path) -> PathBuf {
    base.join(path)
}

fn common_dir(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => absolute(git_dir, Path::new(text.trim())),
        Err(_) => git_dir.to_owned(),
    }
}
