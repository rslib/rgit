/// A diff for one file, produced in-process from a libgit2 patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
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
