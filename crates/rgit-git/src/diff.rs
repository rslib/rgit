use std::cell::Cell;

use git2::DiffFindOptions;

use crate::GitError;

/// Effective rename detection from `diff.renames`: none for `false`, renames
/// for `true` or unset, renames and copies for `copies`. An empty value is
/// false and one that is no boolean is fatal, as in git.
pub fn detect_level(config: Option<&str>) -> Result<u8, GitError> {
    let Some(value) = config else {
        return Ok(1);
    };
    match value.to_ascii_lowercase().as_str() {
        "copies" | "copy" => Ok(2),
        "" | "false" | "no" | "off" => Ok(0),
        "true" | "yes" | "on" => Ok(1),
        other => match other.parse::<i64>() {
            Ok(n) => Ok(u8::from(n != 0)),
            Err(_) => Err(GitError::Cli(format!(
                "fatal: bad boolean config value '{value}' for 'diff.renames'"
            ))),
        },
    }
}

thread_local! {
    static RENAMES: Cell<(Option<u8>, Option<u16>)> = const { Cell::new((None, None)) };
}

/// Set the command line's rename detection for the display diffs this thread
/// runs next: `level` (as `detect_level` returns) wins over `diff.renames`,
/// and `score` is the similarity of `-M<n>` or `-C<n>` (git's `50%`, `5`,
/// `0.5`). `None` leaves each to the config and libgit2's default. It is per
/// thread so that MCP calls, which share one process, cannot see each other's.
pub fn set_renames(level: Option<u8>, score: Option<&str>) {
    let score = score.map(crate::wt_status::parse_rename_score);
    RENAMES.with(|r| r.set((level, score)));
}

/// The rename level `set_renames` gave this thread, if any.
pub fn rename_override() -> Option<u8> {
    RENAMES.with(|r| r.get().0)
}

/// libgit2 rename options for `level` (`detect_level`), None when off.
pub fn find_options(level: u8) -> Option<DiffFindOptions> {
    if level == 0 {
        return None;
    }
    let mut opts = DiffFindOptions::new();
    opts.renames(true);
    if level >= 2 {
        opts.copies(true);
    }
    if let Some(score) = RENAMES.with(|r| r.get().1) {
        opts.rename_threshold(score);
        opts.copy_threshold(score);
    }
    Some(opts)
}

/// A diff for one file, produced in-process from a libgit2 patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The source path when this file was renamed or copied (rename detection
    /// on); `None` for a plain add/modify/delete. `path` is always the new side.
    pub old_path: Option<String>,
    /// How the file changed: added, deleted, modified, renamed, ...
    pub status: crate::StatusCode,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    /// git's file header (`diff --git ...` to `+++ b/...`, or the `Binary
    /// files ... differ` line), each line newline-terminated.
    pub header: String,
    /// git's rename or copy score in percent; 0 for other changes.
    pub similarity: u16,
    /// Old and new sizes in bytes, for a binary file's `Bin A -> B bytes`.
    pub sizes: (u64, u64),
    /// Old and new modes (0 for a missing side), for `--raw`.
    pub modes: (u32, u32),
    /// Old and new object ids (zeros for a missing side), for `--raw`.
    pub ids: (String, String),
    /// `false` when only the delta was read (a status snapshot): `hunks` and
    /// `header` stay empty until loaded on demand or by a full refresh.
    pub loaded: bool,
}

/// One `@@ ... @@` hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@` header line (without trailing newline).
    pub header: String,
    /// Start line on the new side; identifies the hunk when staging.
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOrigin {
    Context,
    Added,
    Removed,
    /// A marker line such as `\ No newline at end of file`.
    Meta,
}

/// One line of a hunk. `text` is the content without the leading origin marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub origin: LineOrigin,
    pub text: String,
}
