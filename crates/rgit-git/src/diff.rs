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
