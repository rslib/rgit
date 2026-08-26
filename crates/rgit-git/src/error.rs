use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("no git repository found at or above {0}")]
    NotARepository(PathBuf),

    #[error("repository at {0} has no working tree")]
    Bare(PathBuf),

    #[error("hunk at line {new_start} of {path} not found")]
    HunkNotFound { path: String, new_start: u32 },

    #[error("nothing staged to commit")]
    NothingToCommit,

    #[error("{0}")]
    Hook(String),

    #[error("HEAD is detached; no branch to sync")]
    DetachedHead,

    #[error("cannot fast-forward; a merge or rebase is required")]
    NotFastForward,

    #[error("push rejected; the remote has changes you do not - fetch and integrate, or force")]
    PushRejected,

    /// A `git` CLI invocation failed (used only for the operations libgit2
    /// cannot do: undo, rebase continue/skip, bisect).
    #[error("{0}")]
    Cli(String),

    #[error("{0}")]
    Conflict(String),

    /// A workflow/workspace orchestration error (no workflow set, bad name, ...).
    #[error("{0}")]
    Other(String),

    #[error(transparent)]
    Git(#[from] git2::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
