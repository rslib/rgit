//! Git data acquisition for rgit.
//!
//! Reads are served through the [`GitBackend`] trait. [`Git2Backend`] implements
//! it entirely in-process over libgit2 - no subprocess on the refresh path.
//! Mutations that libgit2 cannot express are layered on the same trait later.

mod backend;
mod change_id;
mod diff;
mod error;
mod git_repo;
mod model;
mod oplog;
pub mod workflow;
pub mod workspace;

pub use backend::GitBackend;
pub use diff::{DiffLine, FileDiff, Hunk, LineOrigin};
pub use error::GitError;
pub use git_repo::{Git2Backend, clone, init};
pub use model::{
    BlameLine, Commit, CommitDetails, Head, LogEntry, LogOptions, OpLogEntry, OpProgress, RefEntry,
    RefKind, Remote, RepoState, RepoStatus, ResetMode, RestackOutcome, SmartlogEntry, Stash,
    StatusCode, StatusEntry, Worktree,
};
