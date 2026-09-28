//! Git data acquisition for rgit.
//!
//! Reads are served through the [`GitBackend`] trait. [`Git2Backend`] implements
//! it entirely in-process over libgit2 - no subprocess on the refresh path.
//! Mutations that libgit2 cannot express are layered on the same trait later.

mod am;
mod apply;
mod archive;
mod attr;
mod backend;
mod bisect;
mod blame;
mod bundle;
mod change_id;
mod combine;
mod config;
mod creds;
mod describe;
mod diff;
mod error;
mod fetch_display;
mod fmt_merge_msg;
mod format_patch;
mod git_repo;
mod index_ops;
mod lanes;
mod line_log;
mod lowlevel;
mod mail;
mod model;
mod name_rev;
mod notes;
mod oplog;
mod plumbing;
mod range_diff;
mod rebase;
mod shallow;
mod show_branch;
#[cfg(feature = "ssh")]
mod ssh;
mod stash;
mod submodule;
mod text;
mod trailers;
mod update_ref;
mod walk;
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
pub use archive::{
    ArchiveOpts, add_file as archive_file, format_from_filename as archive_format_from_filename,
    format_names as archive_formats, is_local_url, remote_archive,
};
pub use attr::check_attr;
pub use backend::GitBackend;
pub use blame::{Blame, BlameOptions};
pub use bundle::{BundleHeader, bundle_header};
pub use combine::CombinedFile;
pub use config::{
    ConfigEntry, ConfigScope, SetMode, ansi_color, command_line_config, config_file,
    config_fixed_value, config_get, config_key, config_list, config_name_matcher, config_section,
    config_set, config_typed, config_unset, expiry_date, value_matcher,
};
pub use creds::CredentialPrompt;
pub use describe::DescribeOptions;
pub use diff::{DiffLine, FileDiff, Hunk, LineOrigin};
pub use error::GitError;
pub use fmt_merge_msg::{FmtMergeMsgOpts, fmt_merge_msg};
pub use format_patch::{CherryCommit, FormatPatchOpts, PatchMail, Thread, mbox};
pub use git_repo::{
    Git2Backend, clone, diff_no_index, init, ls_remote, pathspec_matches, run_hook,
};
pub use index_ops::{
    CheckoutIndexOpts, ReadTreeOpts, Report, checkout_index, commit_tree, mktag, mktree, read_tree,
    update_index, write_tree,
};
pub use lowlevel::{
    DiffFmt, DiffMode, DiffTreeOpts, MergeFileOpts, MergeTreeOpts, diff_files, diff_index,
    diff_tree, merge_file, merge_tree,
};
pub use mail::{MailinfoOpts, MailsplitOpts, mailinfo, mailsplit};
pub use model::{
    BlameLine, Blob, CheckoutMode, CloneArgs, Commit, CommitDetails, CommitFile, CommitOptions,
    CommitOverview, CommitRef, Deco, DiffSpec, EmptyCommit, FetchArgs, FileActivity, GrepMatch,
    GrepQuery, Head, HunkRef, InitArgs, Lane, LanesState, LastCommit, LogEntry, LogOptions,
    LogOrder, MergeOptions, OpLogEntry, OpProgress, PickOptions, PullArgs, PushArgs, RebaseOptions,
    RebaseProgress, RefEntry, RefKind, RefUpdate, Remote, RepoState, RepoStatus, ResetMode,
    RestackOutcome, RmOptions, SmartlogEntry, Stash, StatusCode, StatusEntry, SubmoduleInfo,
    SubmoduleOp, TagInfo, TreeEntry, Worktree, WorktreeAddArgs, activity_weights,
    group_decorations,
};
pub use name_rev::{NameRevOpts, name_rev};
pub use plumbing::{
    GitGrep, GrepExpr, GrepHit, GrepSyntax, Ident, IgnoreRule, IndexItem, ObjectCounts, PathState,
    RawObject, RefDetail, ReflogItem, TreeItem, TreeWalk, WalkCommit, grep_dir, hash_object,
};
pub use range_diff::RangeDiffOpts;
pub use show_branch::{ShowBranchOpts, show_branch, show_branch_defaults};
pub use text::{
    ColumnOpts, check_ref_format, collapse_slashes, column_finalize, column_mode, columns,
    comment_lines, patch_ids, quote_path, stripspace,
};
pub use trailers::{
    IfExists, IfMissing, NewTrailer, TrailerOpts, Where, interpret_trailers, parse_if_exists,
    parse_if_missing, parse_where,
};

/// Pathspec matching as git's GIT_LITERAL_PATHSPECS, GIT_NOGLOB_PATHSPECS and
/// GIT_ICASE_PATHSPECS (`--literal-pathspecs` and friends) ask.
pub(crate) fn pathspec_flags() -> git2::PathspecFlags {
    let on = |k: &str| std::env::var(k).is_ok_and(|v| git2::Config::parse_bool(v).unwrap_or(false));
    let mut flags = git2::PathspecFlags::DEFAULT;
    if on("GIT_LITERAL_PATHSPECS") || on("GIT_NOGLOB_PATHSPECS") {
        flags |= git2::PathspecFlags::NO_GLOB;
    }
    if on("GIT_ICASE_PATHSPECS") {
        flags |= git2::PathspecFlags::IGNORE_CASE;
    }
    flags
}
