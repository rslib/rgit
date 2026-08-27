//! Git data acquisition for rgit.
//!
//! Reads are served through the [`GitBackend`] trait. [`Git2Backend`] implements
//! it entirely in-process over libgit2 - no subprocess on the refresh path.
//! Mutations that libgit2 cannot express are layered on the same trait later.

mod backend;
mod change_id;
mod creds;
mod diff;
mod lanes;
mod error;
mod git_repo;
mod model;
mod oplog;
pub mod workflow;
pub mod workspace;

pub use backend::GitBackend;
pub use creds::CredentialPrompt;
pub use diff::{DiffLine, FileDiff, Hunk, LineOrigin};
pub use error::GitError;
pub use git_repo::{Git2Backend, clone, init};
pub use model::{
    activity_weights, BlameLine, Commit, CommitDetails, CommitFile, CommitOverview, CommitRef, Deco, FileActivity, Head, HunkRef, Lane, LanesState, LogEntry,
    LogOptions, group_decorations,
    Blob, GrepMatch, GrepQuery, LastCommit, OpLogEntry, OpProgress, RefEntry, RefKind, Remote, RepoState, RepoStatus,
    ResetMode, RestackOutcome, SmartlogEntry, Stash, StatusCode, StatusEntry, TagInfo, TreeEntry,
    Worktree,
};
