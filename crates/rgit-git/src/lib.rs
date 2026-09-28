//! Git data acquisition for rgit.
//!
//! Reads are served through the [`GitBackend`] trait. [`Git2Backend`] implements
//! it entirely in-process over libgit2 - no subprocess on the refresh path.
//! Mutations that libgit2 cannot express are layered on the same trait later.

mod apply;
mod archive;
mod backend;
mod bundle;
mod change_id;
mod config;
mod creds;
mod diff;
mod error;
mod fetch_display;
mod format_patch;
mod git_repo;
mod lanes;
mod model;
mod oplog;
mod plumbing;
mod range_diff;
mod rebase;
mod shallow;
#[cfg(feature = "ssh")]
mod ssh;
mod stash;
mod submodule;
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

pub use apply::{
    ApplyOpts, FilePatch, apply_outside, check_whitespace, parse_patch, patch_numstat, patch_stat,
    patch_summary, rename_label, stat_summary,
};
pub use archive::{ArchiveOpts, add_file as archive_file};
pub use backend::GitBackend;
pub use bundle::{BundleHeader, bundle_header};
pub use config::{
    ConfigEntry, ConfigScope, SetMode, ansi_color, config_file, config_fixed_value, config_key,
    config_list, config_name_matcher, config_section, config_set, config_typed, config_unset,
    expiry_date, value_matcher,
};
pub use creds::CredentialPrompt;
pub use diff::{DiffLine, FileDiff, Hunk, LineOrigin};
pub use error::GitError;
pub use format_patch::{CherryCommit, FormatPatchOpts, PatchMail, Thread, mbox};
pub use git_repo::{
    Git2Backend, clone, diff_no_index, init, ls_remote, pathspec_matches, run_hook,
};
pub use model::{
    BlameLine, Blob, CheckoutMode, CloneArgs, Commit, CommitDetails, CommitFile, CommitOptions,
    CommitOverview, CommitRef, Deco, DiffSpec, EmptyCommit, FetchArgs, FileActivity, GrepMatch,
    GrepQuery, Head, HunkRef, InitArgs, Lane, LanesState, LastCommit, LogEntry, LogOptions,
    MergeOptions, OpLogEntry, OpProgress, PickOptions, PullArgs, PushArgs, RebaseOptions,
    RebaseProgress, RefEntry, RefKind, RefUpdate, Remote, RepoState, RepoStatus, ResetMode,
    RestackOutcome, RmOptions, SmartlogEntry, Stash, StatusCode, StatusEntry, SubmoduleInfo,
    SubmoduleOp, TagInfo, TreeEntry, Worktree, WorktreeAddArgs, activity_weights,
    group_decorations,
};
pub use plumbing::{
    GitGrep, GrepHit, GrepSyntax, Ident, IgnoreRule, IndexItem, ObjectCounts, PathState, RawObject,
    RefDetail, ReflogItem, RevWalk, TreeItem, TreeWalk, WalkCommit, hash_object,
};
pub use range_diff::RangeDiffOpts;
