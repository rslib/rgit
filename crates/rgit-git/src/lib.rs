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
mod clean;
mod combine;
mod commit_graph;
mod config;
mod creds;
mod describe;
mod diagnose;
mod diff;
mod error;
mod fast_export;
mod fast_import;
mod fetch_display;
mod fmt_merge_msg;
mod format_patch;
mod fsck;
mod git_repo;
mod hooks;
mod index_ops;
mod lanes;
mod line_log;
mod lowlevel;
mod mail;
mod mailmap;
mod maintenance;
mod merge_one;
mod midx;
mod model;
mod name_rev;
mod notes;
mod oplog;
mod ort;
mod pack;
mod pack_tools;
mod pathspec;
mod plumbing;
mod promisor;
mod range_diff;
mod rebase;
mod replace;
mod replay;
mod rerere;
mod rev;
mod shallow;
mod show_branch;
mod sign;
mod smart;
mod sparse;
#[cfg(feature = "ssh")]
mod ssh;
mod stash;
mod submodule;
mod text;
mod trailers;
mod update_ref;
pub mod userdiff;
mod walk;
pub mod workflow;
mod ws;
mod wt_status;
pub mod xdiff;

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
pub use bisect::{BisectPick, bisect_steps};
pub use blame::{Blame, BlameOptions};
pub use bundle::{BundleHeader, bundle_header};
pub use clean::{CleanOptions, ignore_match, relative as clean_relative};
pub use combine::CombinedFile;
pub use commit_graph::{CommitGraphOp, CommitGraphWrite, Split as CommitGraphSplit};
pub use config::{
    ConfigEntry, ConfigScope, SetMode, ansi_color, command_line_config, config_all, config_file,
    config_fixed_value, config_get, config_key, config_list, config_name_matcher, config_section,
    config_set, config_typed, config_unset, expiry_date, value_matcher,
};
pub use creds::CredentialPrompt;
pub use describe::DescribeOptions;
pub use diagnose::{bugreport, diagnose, is_partial_clone, strftime_now};
pub use diff::{DiffLine, FileDiff, Hunk, LineOrigin};
pub use error::GitError;
pub use fast_export::fast_export;
pub use fast_import::{FastImportReport, fast_import};
pub use fmt_merge_msg::{FmtMergeMsgOpts, fmt_merge_msg};
pub use format_patch::{CherryCommit, FormatPatchOpts, PatchMail, Thread, mbox};
pub use fsck::{FsckOptions, FsckReport};
pub use git_repo::{
    Git2Backend, OCTOPUS_FAILED, clone, diff_no_index, init, ls_remote, pathspec_matches, run_hook,
};
pub use hooks::{HookWatch, hook_run, hook_running, stream_hooks, watch as watch_hooks};
pub use index_ops::{
    ALL_STAGES, CheckoutIndexOpts, ReadTreeOpts, Report, checkout_index, commit_tree, mktag,
    mktree, read_tree, update_index, write_tree,
};
pub use lowlevel::{
    DiffFmt, DiffMode, DiffTreeOpts, MergeFileOpts, MergeTreeOpts, diff_files, diff_index,
    diff_tree, merge_file, merge_tree, merge_tree_trivial,
};
pub use mail::{MailinfoOpts, MailsplitOpts, mailinfo, mailsplit};
pub use mailmap::{Mailmap, check_mailmap};
pub use maintenance::{
    GcOptions, MaintenanceRun, ReflogExpire, RepackOptions, TASKS as MAINTENANCE_TASKS,
};
pub use merge_one::{index_has_path, merge_one_file, unmerged_stages};
pub use midx::MidxOp;
pub use model::{
    BlameLine, Blob, CheckoutMode, CloneArgs, Commit, CommitDetails, CommitFile, CommitOptions,
    CommitOverview, CommitRef, Deco, DiffSpec, EmptyCommit, FetchArgs, FileActivity, GrepMatch,
    GrepQuery, Head, HunkRef, InitArgs, Lane, LanesState, LastCommit, LogEntry, LogOptions,
    LogOrder, MergeOptions, OpLogEntry, OpProgress, PickOptions, PullArgs, PushArgs, RawPair,
    RebaseOptions, RebaseProgress, RefEntry, RefKind, RefUpdate, Remote, RepoState, RepoStatus,
    ResetMode, RestackOutcome, RmOptions, SmartlogEntry, Stash, StatusCode, StatusEntry,
    SubmoduleInfo, SubmoduleOp, TagInfo, TreeEntry, Worktree, WorktreeAddArgs, activity_weights,
    group_decorations,
};
pub use name_rev::{NameRevOpts, name_rev};
pub use pack_tools::{
    IndexPackOpts, PackObjectsOpts, index_pack, pack_objects, prune_packed_objects, show_index,
    unpack_file, unpack_objects, update_server_info, verify_pack, write_pack_files,
};
pub use pathspec::{check_pathspecs, only_excludes, relocate_pathspec};
pub use plumbing::{
    GitGrep, GrepExpr, GrepHit, GrepSyntax, Ident, IgnoreRule, IndexItem, ObjectCounts, PathState,
    RawObject, RefDetail, ReflogItem, TreeItem, TreeWalk, WalkCommit, grep_dir, hash_object,
};
pub use range_diff::RangeDiffOpts;
pub use replace::{
    replace_convert_grafts, replace_delete, replace_edit_export, replace_edit_import,
    replace_graft, replace_list, replace_object,
};
pub use replay::{RefAction, ReplayOpts, replay};
pub use rev::range_base;
pub use show_branch::{ShowBranchOpts, show_branch, show_branch_defaults};
pub use sign::{SignatureCheck, sign_buffer};
pub use sparse::{
    Widened, narrow as sparse_narrow, outside_only, outside_sparse, sparse_checkout,
    sparse_everywhere, sparse_mv, widen as sparse_widen,
};
pub use text::{
    ColumnOpts, check_ref_format, collapse_slashes, column_finalize, column_mode, columns,
    comment_lines, patch_ids, quote_path, stripspace,
};
pub use trailers::{
    IfExists, IfMissing, NewTrailer, TrailerOpts, Where, interpret_trailers, parse_if_exists,
    parse_if_missing, parse_where,
};
pub use wt_status::{StatusFormat, StatusOpts, StatusReport, cut_line};
