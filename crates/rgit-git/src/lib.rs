//! Git data acquisition for rgit.
//!
//! Reads are served through the [`GitBackend`] trait. [`Git2Backend`] implements
//! it entirely in-process over libgit2 - no subprocess on the refresh path.
//! Mutations that libgit2 cannot express are layered on the same trait later.

mod backend;
mod change_id;
mod creds;
mod diff;
mod error;
mod git_repo;
mod lanes;
mod model;
mod oplog;
mod plumbing;
#[cfg(feature = "ssh")]
mod ssh;
pub mod workflow;

/// Install a callback that supplies an SSH password when key-based auth over an
/// `ssh://` remote fails and no ControlMaster socket is available to reuse. The
/// argument is a human-readable prompt; return `None` to decline. No-op when the
/// `ssh` feature is disabled.
#[cfg(feature = "ssh")]
pub use ssh::set_password_provider;

#[cfg(not(feature = "ssh"))]
pub fn set_password_provider(_ask: impl Fn(&str) -> Option<String> + Send + Sync + 'static) {}

pub mod workspace;

pub use backend::GitBackend;
pub use creds::CredentialPrompt;
pub use diff::{DiffLine, FileDiff, Hunk, LineOrigin};
pub use error::GitError;
pub use git_repo::{Git2Backend, clone, config_value, init, pathspec_matches};
pub use model::{
    BlameLine, Blob, CloneArgs, Commit, CommitDetails, CommitFile, CommitOptions, CommitOverview,
    CommitRef, ConfigScope, Deco, DiffSpec, FetchArgs, FileActivity, GrepMatch, GrepQuery, Head,
    HunkRef, Lane, LanesState, LastCommit, LogEntry, LogOptions, MergeOptions, OpLogEntry,
    OpProgress, PickOptions, PushArgs, RebaseOptions, RefEntry, RefKind, Remote, RepoState,
    RepoStatus, ResetMode, RestackOutcome, SmartlogEntry, Stash, StatusCode, StatusEntry, TagInfo,
    TreeEntry, Worktree, activity_weights, group_decorations,
};
pub use plumbing::{
    GitGrep, GrepHit, GrepSyntax, Ident, IgnoreRule, IndexItem, ObjectCounts, PathState, RawObject,
    RefDetail, ReflogItem, RevWalk, TreeItem, TreeWalk, WalkCommit,
};
