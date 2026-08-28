use std::collections::HashMap;
use std::sync::Arc;

use rgit_git::FileDiff;
use rgit_git::{
    BlameLine, CommitDetails, GitBackend, GitError, Head, LanesState, LogEntry, LogOptions,
    OpLogEntry, RefEntry, Remote, RepoState, RepoStatus, ResetMode, SmartlogEntry, Worktree,
};
use rgit_model::{
    RefTarget, Section, Target, build, build_blame, build_commit, build_diff, build_log,
    build_refs, build_remotes, build_worktrees,
};

use crossterm::event::{KeyCode, KeyEvent};

use crate::buffer::Buffer;
use crate::forge::{self, PullRequest};

/// Where a streamed hook run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStatus {
    Running,
    Passed,
    Failed,
}

/// A git operation streamed into the operation console, with its libgit2
/// progress and git-style output shown live (network ops plus merge/rebase).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleOp {
    Fetch,
    Pull,
    Push {
        force: bool,
        force_with_lease: bool,
        set_upstream: bool,
        /// Target remote by name, or None to push to the branch's upstream.
        remote: Option<String>,
    },
    Merge {
        rev: String,
        no_ff: bool,
    },
    RebaseOnto(String),
    /// Fetch, fast-forward branches to their upstreams, and restack the stack.
    Sync,
    /// Push every branch in the stack and open a pull request per branch.
    Submit,
}

impl ConsoleOp {
    pub fn title(&self) -> &'static str {
        match self {
            ConsoleOp::Fetch => "fetch",
            ConsoleOp::Pull => "pull",
            ConsoleOp::Push { .. } => "push",
            ConsoleOp::Merge { .. } => "merge",
            ConsoleOp::RebaseOnto(_) => "rebase",
            ConsoleOp::Sync => "sync",
            ConsoleOp::Submit => "submit",
        }
    }
}

/// A live console for a git operation, streaming its output (hook scripts for a
/// commit, libgit2 progress for a network op) and holding enough context to
/// retry after a failure.
pub struct HookConsole {
    /// Short operation name shown in the title ("commit", "fetch", ...).
    pub title: String,
    pub lines: Vec<String>,
    pub status: HookStatus,
    /// Top visible line; follows the tail while running unless the user scrolls.
    pub scroll: usize,
    /// Whether the view is pinned to the tail (auto-scroll).
    pub follow: bool,
    /// Object transfer progress `(received, total)`, for the progress bar.
    pub progress: Option<(usize, usize)>,
    /// Commit message and mode, kept so a rejected commit can be edited/retried.
    pub message: String,
    pub amend: bool,
    /// The network op, kept so a failed fetch/pull/push can be re-run.
    pub net: Option<ConsoleOp>,
}

impl HookConsole {
    fn commit(amend: bool, message: String) -> Self {
        Self {
            title: "commit".to_owned(),
            lines: Vec::new(),
            status: HookStatus::Running,
            scroll: 0,
            follow: true,
            progress: None,
            message,
            amend,
            net: None,
        }
    }

    fn net(op: ConsoleOp) -> Self {
        Self {
            title: op.title().to_owned(),
            lines: Vec::new(),
            status: HookStatus::Running,
            scroll: 0,
            follow: true,
            progress: None,
            message: String::new(),
            amend: false,
            net: Some(op),
        }
    }
}

/// A minimal multi-line text editor for the commit message. Rolled by hand to
/// avoid a ratatui-version-locked editor crate; cursor positions are in chars.
pub struct CommitEditor {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize,
    pub amend: bool,
}

impl CommitEditor {
    fn new(amend: bool, prefill: &str) -> Self {
        let lines: Vec<String> = if prefill.is_empty() {
            vec![String::new()]
        } else {
            prefill.lines().map(String::from).collect()
        };
        Self {
            lines,
            row: 0,
            col: 0,
            amend,
        }
    }

    /// The message body: all lines joined, trimmed.
    pub fn message(&self) -> String {
        self.lines.join("\n").trim().to_owned()
    }

    /// Character count of the subject (first) line, for the live counter.
    pub fn subject_len(&self) -> usize {
        self.lines.first().map(|l| l.chars().count()).unwrap_or(0)
    }

    fn line_chars(&self, row: usize) -> Vec<char> {
        self.lines[row].chars().collect()
    }

    /// Replace the whole buffer (e.g. with an AI-generated draft).
    pub fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(String::from).collect()
        };
        self.row = 0;
        self.col = 0;
    }

    /// Apply a key event: text entry, cursor movement, and emacs/readline edits.
    pub fn input(&mut self, key: KeyEvent) {
        use crossterm::event::KeyModifiers;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if ctrl {
            match key.code {
                KeyCode::Char('a') => self.col = 0,
                KeyCode::Char('e') => self.col = self.line_chars(self.row).len(),
                KeyCode::Char('f') => self.move_right(),
                KeyCode::Char('b') => self.move_left(),
                KeyCode::Char('n') => self.move_down(),
                KeyCode::Char('p') => self.move_up(),
                KeyCode::Char('d') => self.delete_forward(),
                KeyCode::Char('k') => self.kill_to_end(),
                KeyCode::Char('u') => self.kill_to_start(),
                KeyCode::Char('w') => self.delete_word_back(),
                _ => {}
            }
            return;
        }
        if alt {
            match key.code {
                KeyCode::Char('f') => self.move_word_right(),
                KeyCode::Char('b') => self.move_word_left(),
                KeyCode::Backspace => self.delete_word_back(),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char(c) => {
                let mut chars = self.line_chars(self.row);
                chars.insert(self.col.min(chars.len()), c);
                self.lines[self.row] = chars.into_iter().collect();
                self.col += 1;
            }
            KeyCode::Backspace => {
                if self.col > 0 {
                    let mut chars = self.line_chars(self.row);
                    chars.remove(self.col - 1);
                    self.lines[self.row] = chars.into_iter().collect();
                    self.col -= 1;
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.line_chars(self.row).len();
                    self.lines[self.row].push_str(&line);
                }
            }
            KeyCode::Enter => {
                let chars = self.line_chars(self.row);
                let rest: String = chars[self.col.min(chars.len())..].iter().collect();
                let kept: String = chars[..self.col.min(chars.len())].iter().collect();
                self.lines[self.row] = kept;
                self.lines.insert(self.row + 1, rest);
                self.row += 1;
                self.col = 0;
            }
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.line_chars(self.row).len(),
            _ => {}
        }
    }

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line_chars(self.row).len();
        }
    }

    fn move_right(&mut self) {
        let len = self.line_chars(self.row).len();
        if self.col < len {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.line_chars(self.row).len());
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.line_chars(self.row).len());
        }
    }

    /// Delete the character under the cursor (or join the next line at EOL).
    fn delete_forward(&mut self) {
        let mut chars = self.line_chars(self.row);
        if self.col < chars.len() {
            chars.remove(self.col);
            self.lines[self.row] = chars.into_iter().collect();
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn kill_to_end(&mut self) {
        let chars = self.line_chars(self.row);
        if self.col < chars.len() {
            self.lines[self.row] = chars[..self.col].iter().collect();
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn kill_to_start(&mut self) {
        let chars = self.line_chars(self.row);
        self.lines[self.row] = chars[self.col.min(chars.len())..].iter().collect();
        self.col = 0;
    }

    /// The index of the start of the word before the cursor (readline C-w / M-Bksp).
    fn word_start(&self) -> usize {
        let chars = self.line_chars(self.row);
        let mut i = self.col.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn delete_word_back(&mut self) {
        let start = self.word_start();
        if start < self.col {
            let mut chars = self.line_chars(self.row);
            chars.drain(start..self.col);
            self.lines[self.row] = chars.into_iter().collect();
            self.col = start;
        } else {
            self.move_left();
        }
    }

    fn move_word_left(&mut self) {
        if self.col == 0 {
            self.move_left();
        } else {
            self.col = self.word_start();
        }
    }

    fn move_word_right(&mut self) {
        let chars = self.line_chars(self.row);
        let mut i = self.col;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        if i == self.col && self.col == chars.len() {
            self.move_right();
        } else {
            self.col = i;
        }
    }
}

/// One line of an interactive-rebase todo.
pub struct TodoEntry {
    pub action: char,
    pub short: String,
    pub subject: String,
}

/// The in-app interactive-rebase editor: reorder and mark commits, then run.
pub struct RebaseTodo {
    pub base: String,
    pub entries: Vec<TodoEntry>,
    pub cursor: usize,
}

impl RebaseTodo {
    /// The git rebase todo text for the current entries and order.
    pub fn todo_text(&self) -> String {
        self.entries
            .iter()
            .map(|e| {
                let action = match e.action {
                    's' => "squash",
                    'f' => "fixup",
                    'd' => "drop",
                    'r' => "reword",
                    'e' => "edit",
                    _ => "pick",
                };
                format!("{action} {} {}", e.short, e.subject)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The result of a background status refresh, delivered back as a [`Msg`].
pub type RefreshResult = Result<RepoStatus, GitError>;

/// Which screen a view shows. The status view is always the stack's root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Status,
    Log,
    Commit,
    Blame,
    Refs,
    Remotes,
    Worktrees,
    Forge,
    Review,
    Diff,
    SessionLog,
    Smartlog,
    Oplog,
    Stack,
    Lanes,
    Info,
}

/// One code-search result: a file location, its fused score, how it was found
/// (text/semantic/both), and a one-line preview.
#[derive(Debug, Clone)]
pub struct CodeHit {
    pub path: String,
    pub line: usize,
    pub score: f32,
    pub tag: &'static str,
    pub preview: String,
}

/// A backend operation that returns a status string, shown as a toast.
#[derive(Debug, Clone)]
pub enum TextOp {
    FlowInit(String),
    FlowStart(String),
    FlowFinish,
    WorkspaceNew(String),
    StackNew(String),
    /// Delete local branches merged into the given base; toast the result.
    PruneMerged(String),
}

/// The current branch's forge status, shown in the REMOTE section.
#[derive(Debug, Clone)]
pub struct RemoteSummary {
    /// The open PR for the current branch: `(number, state)`.
    pub pr: Option<(u64, String)>,
    /// The PR's CI rollup: `passing` / `failing` / `pending`.
    pub checks: Option<String>,
}

/// A lane mutation run from the lanes view, reported and then reloaded.
#[derive(Debug, Clone)]
pub enum LaneOp {
    New(String),
    Assign { lane: String, path: String },
    Unassign(String),
    Commit { lane: String, message: String },
    Rename { old: String, new: String },
    Delete(String),
    Push(String),
    Pr(String),
    Stack { name: String, parent: String },
    Restack,
}

/// A read-only text panel opened as the Info view.
#[derive(Debug, Clone, Copy)]
pub enum InfoKind {
    FlowStatus,
    Workspaces,
}

/// Which severities the session-log view shows, cycled with `f`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFilter {
    #[default]
    All,
    /// Warnings and errors only.
    WarnPlus,
    Errors,
}

impl LogFilter {
    fn next(self) -> Self {
        match self {
            LogFilter::All => LogFilter::WarnPlus,
            LogFilter::WarnPlus => LogFilter::Errors,
            LogFilter::Errors => LogFilter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LogFilter::All => "all",
            LogFilter::WarnPlus => "warnings+",
            LogFilter::Errors => "errors",
        }
    }

    fn admits(self, level: crate::session_log::LogLevel) -> bool {
        use crate::session_log::LogLevel;
        match self {
            LogFilter::All => true,
            LogFilter::WarnPlus => level <= LogLevel::Warn,
            LogFilter::Errors => level == LogLevel::Error,
        }
    }
}

/// One screen on the navigation stack: its kind and its buffer.
pub struct View {
    pub kind: ViewKind,
    pub buffer: Buffer,
}

/// A message: a user intent resolved from a key, or an async result. Everything
/// that can change the app flows through [`update`] as one of these.
#[derive(Debug)]
pub enum Msg {
    Quit,
    CursorUp,
    CursorDown,
    CursorTop,
    CursorBottom,
    CursorHalfUp,
    CursorHalfDown,
    /// Open the vim-profile leader (which-key) overlay.
    LeaderOpen,
    FocusPreview,
    FocusNav,
    ToggleFold,
    /// Linewise visual selection (vim V, magit v).
    ToggleSelect,
    /// Charwise visual selection (vim v).
    ToggleCharSelect,
    /// Caret motions for charwise visual (vim h/l/w/b/0/$).
    ColLeft,
    ColRight,
    ColWordForward,
    ColWordBack,
    ColWordEnd,
    ColLineStart,
    ColLineEnd,
    ColFirstNonBlank,
    /// Copy the visual selection (or cursor row) to the system clipboard.
    Yank,
    Stage,
    Unstage,
    StageAll,
    UnstageAll,
    Discard,
    /// Remove the tracked file under the cursor (git rm), after a confirm.
    RmAtCursor,
    /// Rename the tracked file under the cursor (git mv); prompts for the name.
    MvAtCursor,
    /// Open the live code-search finder.
    CodeFinderOpen,
    CodeFinderChar(char),
    CodeFinderBackspace,
    CodeFinderUp,
    CodeFinderDown,
    CodeFinderCancel,
    /// Open the file at the selected hit.
    CodeFinderSubmit,
    /// Fold the semantic index into the current query's results.
    CodeFinderSemantic,
    /// Search results for `query`; `semantic` is true when the index was used.
    CodeFinderHits {
        query: String,
        semantic: bool,
        hits: Result<Vec<CodeHit>, String>,
    },
    ConfirmAccept,
    ConfirmCancel,
    CommitMenu,
    BranchesLoaded(Vec<String>),
    Log,
    LogLoaded(Vec<LogEntry>),
    Enter,
    CommitLoaded(Box<CommitDetails>),
    /// A preview (commit, stash, or large file diff) finished building off-thread.
    PreviewBuilt {
        key: PreviewKey,
        sections: Vec<Section>,
    },
    BlameLoaded {
        path: String,
        lines: Vec<BlameLine>,
    },
    Refs,
    RefsLoaded(Vec<RefEntry>),
    RemoteMenu,
    RemotesLoaded(Vec<Remote>),
    /// Remotes loaded specifically to pick a push target.
    PushRemotesLoaded(Vec<Remote>),
    WorktreeMenu,
    WorktreesLoaded(Vec<Worktree>),
    Forge,
    ForgeLoaded(Result<Vec<PullRequest>, String>),
    DiffPrompt,
    DiffLoaded {
        title: String,
        files: Vec<FileDiff>,
    },
    Error(String),
    PromptInput(KeyEvent),
    PromptUp,
    PromptDown,
    PromptSubmit,
    PromptCancel,
    Fetch,
    Pull,
    PushMenu,
    HelpToggle,
    HelpClose,
    StashMenu,
    StashPop,
    OpenSessionLog,
    CycleLogFilter,
    /// Open the smartlog view.
    OpenSmartlog,
    SmartlogLoaded(Vec<SmartlogEntry>),
    /// Open the operation-log (timeline) view.
    OpenOplog,
    OplogLoaded(Vec<OpLogEntry>),
    /// Open the stacked-branches view.
    OpenStack,
    StackLoaded {
        parents: Vec<(String, Option<String>)>,
        current: Option<String>,
    },
    /// Restack every stacked branch onto its parent's new tip.
    Restack,
    /// A note (shown as a toast) that an amend auto-restacked its children.
    AutoRestackNote(String),
    /// The current branch's forge PR/CI status finished loading.
    RemoteStatusLoaded(Option<RemoteSummary>),
    /// A network operation needs a credential; open a (masked) prompt and reply
    /// with the value the user enters, or `None` if they cancel.
    CredentialRequest {
        label: String,
        masked: bool,
        reply: std::sync::mpsc::Sender<Option<String>>,
    },
    /// Open the lanes view.
    OpenLanes,
    /// The lanes state finished loading; (re)build the lanes view.
    LanesLoaded(LanesState),
    /// A one-line result of a lane operation, shown as a toast.
    LaneNotice(String),
    /// Prompt for a new lane name.
    LaneNewPrompt,
    /// Prompt for the lane to assign the file under the cursor to.
    LaneAssignPrompt,
    /// Return the file under the cursor to the default lane.
    LaneUnassignAtCursor,
    /// Prompt for a commit message for the lane the cursor is in.
    LaneCommitPrompt,
    /// Prompt for a new name for the lane the cursor is in.
    LaneRenamePrompt,
    /// Prompt for a name for a new lane stacked on the lane the cursor is in.
    LaneStackPrompt,
    /// Delete the lane the cursor is in.
    LaneDeleteAtCursor,
    /// Push the branch of the lane the cursor is in.
    LanePushAtCursor,
    /// Push and open a PR for the lane the cursor is in.
    LanePrAtCursor,
    /// Restack all stacked lanes onto their parents' tips.
    LaneRestack,
    /// Fold pending changes into the commits that last touched those lines.
    Absorb,
    /// Prompt for a workflow preset, then set it.
    FlowInitPrompt,
    /// Prompt for a feature name, then start it per the active workflow.
    FlowStartPrompt,
    /// Finish the current feature per the active workflow.
    FlowFinish,
    /// Open the workflow status panel.
    OpenFlowStatus,
    /// Prompt for a name, then create a CoW workspace.
    WorkspaceNewPrompt,
    /// Open the workspaces panel.
    OpenWorkspaces,
    /// Prompt for a name, then create a stacked branch.
    StackNewPrompt,
    /// Open the operations menu (undo/redo, smartlog, op-log, stack, absorb, …).
    OperationsMenu,
    /// Result of a text-returning backend op, shown as a toast.
    TextResult(Result<String, String>),
    /// A loaded read-only text panel.
    InfoLoaded {
        title: String,
        text: Result<String, String>,
    },
    BranchMenu,
    RebaseMenu,
    MergeMenu,
    ResetMenu,
    TagMenu,
    CherryPick,
    Revert,
    ResolveOurs,
    ResolveTheirs,
    BisectStartPrompt,
    BisectGood,
    BisectBad,
    BisectReset,
    AiReview,
    AiReviewLoaded(String),
    Undo,
    Redo,
    ShowRebaseTodo {
        base: String,
        entries: Vec<(String, String)>,
    },
    RebaseTodoUp,
    RebaseTodoDown,
    RebaseTodoMoveUp,
    RebaseTodoMoveDown,
    RebaseTodoSetAction(char),
    RebaseTodoRun,
    RebaseTodoCancel,
    TransientChar(char),
    TransientCancel,
    PaletteOpen,
    PaletteChar(char),
    PaletteBackspace,
    PaletteUp,
    PaletteDown,
    PaletteSubmit,
    PaletteCancel,
    SearchOpen,
    SearchChar(char),
    SearchBackspace,
    SearchSubmit,
    SearchCancel,
    SearchNext,
    SearchPrev,
    ShowCommitEditor {
        amend: bool,
        prefill: String,
    },
    CommitEditorInput(KeyEvent),
    CommitEditorSubmit,
    CommitEditorCancel,
    CommitEditorEditor,
    CommitEditorGenerate,
    SetCommitEditorText(String),
    /// A line of output for the operation console (hook or network op).
    HookOutput(String),
    /// Object transfer progress for the console's progress bar.
    HookProgress {
        received: usize,
        total: usize,
    },
    /// The streaming commit finished; `ok` is whether it succeeded, `summary`
    /// the git-style result line to toast on success.
    HookFinished {
        ok: bool,
        summary: Option<String>,
    },
    /// Scroll the hook console by a signed number of rows.
    HookScroll(isize),
    /// Dismiss the hook console (after a pass, or discarding a failed attempt).
    HookConsoleClose,
    /// Reopen the commit editor with the rejected message, to fix and retry.
    HookConsoleRetry,
    /// Wheel scroll by a signed number of rows.
    /// Mouse wheel over the navigator: scroll it (and pull more log history).
    Scroll(isize),
    /// Mouse wheel over the preview pane: scroll it without taking focus.
    ScrollPreview(isize),
    /// Left-click on the body row at the given zero-based body offset.
    ClickRow(usize),
    /// A click in the preview pane at this viewport-row offset.
    ClickPreview(usize),
    Refresh,
    /// A refresh triggered by the file watcher; quiet (no loading indicator).
    AutoRefresh,
    /// Boxed because a `RepoStatus` is far larger than the other variants.
    Refreshed(Box<RefreshResult>),
}

/// A repository mutation, resolved from the target under the cursor.
#[derive(Debug)]
pub enum Mutation {
    StageFile(String),
    UnstageFile(String),
    StageHunk {
        path: String,
        new_start: u32,
    },
    UnstageHunk {
        path: String,
        new_start: u32,
    },
    StageLines {
        path: String,
        new_start: u32,
        lines: Vec<usize>,
    },
    UnstageLines {
        path: String,
        new_start: u32,
        lines: Vec<usize>,
    },
    StageAll,
    UnstageAll,
    DiscardFile(String),
    DiscardHunk {
        path: String,
        new_start: u32,
    },
    DiscardLines {
        path: String,
        new_start: u32,
        lines: Vec<usize>,
    },
    CheckoutBranch(String),
    CreateBranch(String),
    CheckoutDetached(String),
    DeleteBranch(String),
    RebaseAbort,
    RebaseContinue,
    RebaseSkip,
    /// Undo the last operation from the op-log, keeping uncommitted work.
    Undo,
    /// Redo the operation most recently undone.
    Redo,
    /// Fold pending changes into the commits that last touched those lines.
    Absorb,
    /// Rebase every stacked branch onto its parent's new tip.
    Restack,
    /// A `git bisect` subcommand and its args (without the leading `bisect`).
    Bisect(Vec<String>),
    Reset {
        rev: String,
        mode: ResetMode,
    },
    CherryPick(String),
    Revert(String),
    ResolveConflict {
        path: String,
        ours: bool,
    },
    CreateTag(String),
    DeleteTag(String),
    StashPush,
    StashPushMessage(String),
    StashPop(usize),
    StashApply(usize),
    StashDrop(usize),
    RenameBranch {
        old: String,
        new: String,
    },
    AddRemote {
        name: String,
        url: String,
    },
    RemoveRemote(String),
    AddWorktree {
        name: String,
        path: String,
    },
    RemoveWorktree(String),
    Extend,
    /// Rewrite `rev`'s message, replaying its descendants onto the new commit.
    Reword {
        rev: String,
        message: String,
    },
    /// Fold `rev` into its parent, keeping the parent's message.
    Squash(String),
    /// Undo the last `n` commits, keeping their changes in the working tree.
    Uncommit(usize),
    /// Check out the branch stacked on the current one (up the stack).
    StackNext,
    /// Check out the current branch's stack parent (down the stack).
    StackPrev,
    /// Remove untracked files and directories (git clean -fd).
    Clean,
    /// Move `rev` to just before `target` in the current branch's history.
    Reorder {
        rev: String,
        target: String,
    },
    /// Split `rev` into two commits, `paths` going into the first.
    Split {
        rev: String,
        paths: Vec<String>,
    },
    /// Remove a tracked path from the index and working tree (git rm).
    RemovePath(String),
    /// Rename a tracked path (git mv).
    MovePath {
        from: String,
        to: String,
    },
}

/// A destructive action awaiting a yes/no answer in the status bar.
pub struct PendingConfirm {
    pub prompt: String,
    pub mutation: Mutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
}

/// A transient notification shown briefly in the corner. `ticks` counts down on
/// each logic tick; the toast is dropped at zero.
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub ticks: u8,
}

/// A toggleable switch in a transient menu (e.g. `--force`).
pub struct ArgToggle {
    pub key: char,
    pub label: String,
    pub on: bool,
}

/// A suffix command in a transient menu.
pub struct TransientAction {
    pub key: char,
    pub label: String,
    pub kind: ActionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Push,
    PushElsewhere,
    Commit,
    Amend,
    Extend,
    RebaseUpstream,
    RebaseElsewhere,
    RebaseInteractive,
    RebaseContinue,
    RebaseSkip,
    RebaseAbort,
    ResetSoft,
    ResetMixed,
    ResetHard,
    MergeBranch,
    TagCreate,
    TagDelete,
    StashPush,
    StashMessage,
    StashPopAt,
    StashApplyAt,
    StashDropAt,
    BranchCheckout,
    BranchCreateNew,
    BranchRename,
    BranchDelete,
    RemoteList,
    RemoteAdd,
    RemoteRemove,
    WorktreeList,
    WorktreeAdd,
    WorktreeRemove,
    LogShow,
    LogAuthor,
    OpUndo,
    OpRedo,
    OpOplog,
    OpSmartlog,
    OpStack,
    OpStackNew,
    OpRestack,
    OpAbsorb,
    OpFlowStatus,
    OpWorkspaces,
    OpLanes,
    Reword,
    Squash,
    Uncommit,
    OpSync,
    OpSubmit,
    OpPruneMerged,
    OpClean,
    OpStackNext,
    OpStackPrev,
    Reorder,
    Split,
}

/// A transient popup: a title, sticky argument toggles, and suffix actions that
/// run with the accumulated arguments.
pub struct Transient {
    pub title: String,
    pub args: Vec<ArgToggle>,
    pub actions: Vec<TransientAction>,
}

impl Transient {
    fn push() -> Self {
        Self {
            title: "Push".into(),
            args: vec![
                ArgToggle {
                    key: 'f',
                    label: "--force-with-lease".into(),
                    on: false,
                },
                ArgToggle {
                    key: 'F',
                    label: "--force".into(),
                    on: false,
                },
                ArgToggle {
                    key: 'u',
                    label: "--set-upstream".into(),
                    on: false,
                },
            ],
            actions: vec![
                TransientAction {
                    key: 'p',
                    label: "push to upstream".into(),
                    kind: ActionKind::Push,
                },
                TransientAction {
                    key: 'e',
                    label: "push to other remote\u{2026}".into(),
                    kind: ActionKind::PushElsewhere,
                },
            ],
        }
    }

    fn commit() -> Self {
        Self {
            title: "Commit".into(),
            args: Vec::new(),
            actions: vec![
                TransientAction {
                    key: 'c',
                    label: "commit".into(),
                    kind: ActionKind::Commit,
                },
                TransientAction {
                    key: 'a',
                    label: "amend".into(),
                    kind: ActionKind::Amend,
                },
                TransientAction {
                    key: 'e',
                    label: "extend (keep message)".into(),
                    kind: ActionKind::Extend,
                },
            ],
        }
    }

    fn rebase(rebasing: bool) -> Self {
        let actions = if rebasing {
            vec![
                action('r', "continue", ActionKind::RebaseContinue),
                action('s', "skip", ActionKind::RebaseSkip),
                action('a', "abort", ActionKind::RebaseAbort),
            ]
        } else {
            vec![
                action('u', "onto upstream", ActionKind::RebaseUpstream),
                action('e', "onto elsewhere…", ActionKind::RebaseElsewhere),
                action('i', "interactive…", ActionKind::RebaseInteractive),
            ]
        };
        Self {
            title: "Rebase".into(),
            args: Vec::new(),
            actions,
        }
    }

    fn reset() -> Self {
        Self {
            title: "Reset".into(),
            args: Vec::new(),
            actions: vec![
                action('s', "soft  (move HEAD)", ActionKind::ResetSoft),
                action('m', "mixed (+ index)", ActionKind::ResetMixed),
                action('h', "hard  (+ worktree)", ActionKind::ResetHard),
            ],
        }
    }

    fn merge() -> Self {
        Self {
            title: "Merge".into(),
            args: vec![ArgToggle {
                key: 'n',
                label: "--no-ff".into(),
                on: false,
            }],
            actions: vec![action('m', "merge a branch…", ActionKind::MergeBranch)],
        }
    }

    fn tag() -> Self {
        Self {
            title: "Tag".into(),
            args: Vec::new(),
            actions: vec![
                action('t', "create at HEAD…", ActionKind::TagCreate),
                action('k', "delete…", ActionKind::TagDelete),
            ],
        }
    }

    fn stash() -> Self {
        Self {
            title: "Stash".into(),
            args: Vec::new(),
            actions: vec![
                action('z', "save", ActionKind::StashPush),
                action('s', "save with message…", ActionKind::StashMessage),
                action('p', "pop at point", ActionKind::StashPopAt),
                action('a', "apply at point", ActionKind::StashApplyAt),
                action('k', "drop at point", ActionKind::StashDropAt),
            ],
        }
    }

    fn operations() -> Self {
        Self {
            title: "Operations".into(),
            args: Vec::new(),
            actions: vec![
                action('u', "undo", ActionKind::OpUndo),
                action('r', "redo", ActionKind::OpRedo),
                action('o', "op-log (timeline)", ActionKind::OpOplog),
                action('s', "smartlog", ActionKind::OpSmartlog),
                action('k', "stack", ActionKind::OpStack),
                action('n', "stack: new branch…", ActionKind::OpStackNew),
                action('R', "restack", ActionKind::OpRestack),
                action('a', "absorb", ActionKind::OpAbsorb),
                action('e', "reword…", ActionKind::Reword),
                action('q', "squash into parent…", ActionKind::Squash),
                action('U', "uncommit (soft-reset HEAD~1)", ActionKind::Uncommit),
                action('m', "move commit (reorder)…", ActionKind::Reorder),
                action('v', "split commit by path…", ActionKind::Split),
                action('y', "sync (fetch, ff, restack)", ActionKind::OpSync),
                action('S', "submit stack (push + PRs)", ActionKind::OpSubmit),
                action('J', "next: up the stack", ActionKind::OpStackNext),
                action('K', "prev: down the stack", ActionKind::OpStackPrev),
                action('p', "prune merged branches", ActionKind::OpPruneMerged),
                action('x', "clean untracked", ActionKind::OpClean),
                action('f', "flow status", ActionKind::OpFlowStatus),
                action('w', "workspaces", ActionKind::OpWorkspaces),
                action('l', "lanes", ActionKind::OpLanes),
            ],
        }
    }

    fn branch() -> Self {
        Self {
            title: "Branch".into(),
            args: Vec::new(),
            actions: vec![
                action('b', "checkout…", ActionKind::BranchCheckout),
                action('n', "create & checkout…", ActionKind::BranchCreateNew),
                action('m', "rename current…", ActionKind::BranchRename),
                action('k', "delete…", ActionKind::BranchDelete),
            ],
        }
    }

    fn log() -> Self {
        Self {
            title: "Log".into(),
            args: vec![ArgToggle {
                key: 'a',
                label: "--all (every ref)".into(),
                on: false,
            }],
            actions: vec![
                action('l', "show", ActionKind::LogShow),
                action('u', "filter by author…", ActionKind::LogAuthor),
            ],
        }
    }

    fn remote() -> Self {
        Self {
            title: "Remote".into(),
            args: Vec::new(),
            actions: vec![
                action('l', "list", ActionKind::RemoteList),
                action('a', "add…", ActionKind::RemoteAdd),
                action('k', "remove…", ActionKind::RemoteRemove),
            ],
        }
    }

    fn worktree() -> Self {
        Self {
            title: "Worktree".into(),
            args: Vec::new(),
            actions: vec![
                action('l', "list", ActionKind::WorktreeList),
                action('a', "add…", ActionKind::WorktreeAdd),
                action('k', "remove…", ActionKind::WorktreeRemove),
            ],
        }
    }

    fn arg_on(&self, key: char) -> bool {
        self.args.iter().any(|a| a.key == key && a.on)
    }
}

fn action(key: char, label: &str, kind: ActionKind) -> TransientAction {
    TransientAction {
        key,
        label: label.into(),
        kind,
    }
}

/// Open a minibuffer prompt where the user types a revision (no candidates).
fn revision_prompt(app: &mut App, label: &str, action: PromptAction) {
    app.transient = None;
    app.prompt = Some(Prompt {
        label: label.into(),
        input: String::new(),
        cursor: 0,
        candidates: Vec::new(),
        selected: 0,
        action,
        masked: false,
    });
}

/// What a submitted [`Prompt`] does with its value.
#[derive(Debug, Clone, Copy)]
pub enum PromptAction {
    CheckoutBranch,
    CreateBranch,
    RebaseOnto,
    RebaseInteractive,
    ResetSoft,
    ResetMixed,
    ResetHard,
    MergeBranch,
    CherryPick,
    Revert,
    CreateTag,
    DeleteTag,
    StashMessage,
    RenameBranch,
    DeleteBranch,
    AddRemote,
    RemoveRemote,
    AddWorktree,
    RemoveWorktree,
    LogAuthor,
    DiffRefs,
    BisectStart,
    FlowInit,
    FlowStart,
    WorkspaceNew,
    StackNew,
    LaneNew,
    /// Assign `pending_lane_path` to the lane named by the prompt value.
    LaneAssign,
    /// Commit `pending_lane` with the message from the prompt value.
    LaneCommit,
    /// Rename `pending_lane` to the prompt value.
    LaneRename,
    /// Create a lane named by the prompt value, stacked on `pending_lane`.
    LaneStack,
    /// Answer a credential request via `pending_cred_reply`.
    Credential,
    /// Pick the commit to reword; stash it in `pending_reword_rev` and prompt
    /// for the new message.
    RewordRev,
    /// Reword `pending_reword_rev` with the prompt value.
    RewordMessage,
    /// Squash the commit named by the prompt value into its parent.
    Squash,
    /// Push to the remote named by the prompt value, with `pending_push` args.
    PushRemote,
    /// Pick the commit to move; then prompt for the target it moves before.
    ReorderRev,
    /// Move `pending_reorder_rev` before the commit named by the prompt value.
    ReorderTarget,
    /// Pick the commit to split; then prompt for the paths for the first part.
    SplitRev,
    /// Split `pending_split_rev`, the prompt value's paths going in the first.
    SplitPaths,
    /// Rename `pending_move_from` to the path named by the prompt value.
    MoveFile,
}

/// A minibuffer: a label, an editable input, and optional filterable candidates.
pub struct Prompt {
    pub label: String,
    pub input: String,
    /// Caret position as a char index into `input`.
    pub cursor: usize,
    pub candidates: Vec<String>,
    pub selected: usize,
    pub action: PromptAction,
    /// Render the input as dots (for secrets like a password/passphrase).
    pub masked: bool,
}

impl Prompt {
    /// Apply an editing key to the single-line input: text entry plus emacs/
    /// readline motions and kills. Any edit resets the candidate selection.
    pub fn apply(&mut self, key: KeyEvent) {
        use crossterm::event::KeyModifiers;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let mut chars: Vec<char> = self.input.chars().collect();
        let len = chars.len();
        self.cursor = self.cursor.min(len);
        if ctrl {
            match key.code {
                KeyCode::Char('a') => self.cursor = 0,
                KeyCode::Char('e') => self.cursor = len,
                KeyCode::Char('f') => self.cursor = (self.cursor + 1).min(len),
                KeyCode::Char('b') => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Char('d') => {
                    if self.cursor < len {
                        chars.remove(self.cursor);
                    }
                }
                KeyCode::Char('k') => chars.truncate(self.cursor),
                KeyCode::Char('u') => {
                    chars.drain(..self.cursor);
                    self.cursor = 0;
                }
                KeyCode::Char('w') => {
                    let start = word_start(&chars, self.cursor);
                    chars.drain(start..self.cursor);
                    self.cursor = start;
                }
                _ => {}
            }
        } else if alt {
            match key.code {
                KeyCode::Char('b') => self.cursor = word_start(&chars, self.cursor),
                KeyCode::Char('f') => self.cursor = word_end(&chars, self.cursor),
                KeyCode::Backspace => {
                    let start = word_start(&chars, self.cursor);
                    chars.drain(start..self.cursor);
                    self.cursor = start;
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Char(c) => {
                    chars.insert(self.cursor, c);
                    self.cursor += 1;
                }
                KeyCode::Backspace => {
                    if self.cursor > 0 {
                        chars.remove(self.cursor - 1);
                        self.cursor -= 1;
                    }
                }
                KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
                KeyCode::Home => self.cursor = 0,
                KeyCode::End => self.cursor = len,
                _ => {}
            }
        }
        self.input = chars.into_iter().collect();
        self.selected = 0;
    }

    /// Candidates containing the current input, in order.
    pub fn filtered(&self) -> Vec<&str> {
        self.candidates
            .iter()
            .filter(|c| c.contains(&self.input))
            .map(String::as_str)
            .collect()
    }

    /// The value to submit: the highlighted candidate, or the raw input.
    fn value(&self) -> String {
        self.filtered()
            .get(self.selected)
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.input.clone())
    }
}

/// One command reachable from the fuzzy command palette. `make` yields the same
/// [`Msg`] the command's key binding would, so the palette teaches the key.
pub struct PaletteEntry {
    pub label: &'static str,
    pub keyhint: &'static str,
    pub make: fn() -> Msg,
}

/// Every command the palette can run, in a stable display order.
pub static PALETTE: &[PaletteEntry] = &[
    PaletteEntry {
        label: "Commit",
        keyhint: "c",
        make: || Msg::CommitMenu,
    },
    PaletteEntry {
        label: "Smartlog",
        keyhint: "",
        make: || Msg::OpenSmartlog,
    },
    PaletteEntry {
        label: "Operation log (timeline)",
        keyhint: "",
        make: || Msg::OpenOplog,
    },
    PaletteEntry {
        label: "Stack: view",
        keyhint: "",
        make: || Msg::OpenStack,
    },
    PaletteEntry {
        label: "Stack: new branch",
        keyhint: "",
        make: || Msg::StackNewPrompt,
    },
    PaletteEntry {
        label: "Stack: restack",
        keyhint: "",
        make: || Msg::Restack,
    },
    PaletteEntry {
        label: "Absorb changes",
        keyhint: "",
        make: || Msg::Absorb,
    },
    PaletteEntry {
        label: "Flow: status",
        keyhint: "",
        make: || Msg::OpenFlowStatus,
    },
    PaletteEntry {
        label: "Flow: init workflow",
        keyhint: "",
        make: || Msg::FlowInitPrompt,
    },
    PaletteEntry {
        label: "Flow: start feature",
        keyhint: "",
        make: || Msg::FlowStartPrompt,
    },
    PaletteEntry {
        label: "Flow: finish",
        keyhint: "",
        make: || Msg::FlowFinish,
    },
    PaletteEntry {
        label: "Workspace: list",
        keyhint: "",
        make: || Msg::OpenWorkspaces,
    },
    PaletteEntry {
        label: "Workspace: new (CoW clone)",
        keyhint: "",
        make: || Msg::WorkspaceNewPrompt,
    },
    PaletteEntry {
        label: "Stage all",
        keyhint: "S",
        make: || Msg::StageAll,
    },
    PaletteEntry {
        label: "Unstage all",
        keyhint: "U",
        make: || Msg::UnstageAll,
    },
    PaletteEntry {
        label: "Log",
        keyhint: "l",
        make: || Msg::Log,
    },
    PaletteEntry {
        label: "Session log",
        keyhint: "L",
        make: || Msg::OpenSessionLog,
    },
    PaletteEntry {
        label: "Refs",
        keyhint: "y",
        make: || Msg::Refs,
    },
    PaletteEntry {
        label: "Diff revisions",
        keyhint: "d",
        make: || Msg::DiffPrompt,
    },
    PaletteEntry {
        label: "Branch",
        keyhint: "b",
        make: || Msg::BranchMenu,
    },
    PaletteEntry {
        label: "Stash",
        keyhint: "z",
        make: || Msg::StashMenu,
    },
    PaletteEntry {
        label: "Tag",
        keyhint: "t",
        make: || Msg::TagMenu,
    },
    PaletteEntry {
        label: "Rebase",
        keyhint: "r",
        make: || Msg::RebaseMenu,
    },
    PaletteEntry {
        label: "Merge",
        keyhint: "m",
        make: || Msg::MergeMenu,
    },
    PaletteEntry {
        label: "Reset",
        keyhint: "O",
        make: || Msg::ResetMenu,
    },
    PaletteEntry {
        label: "Cherry-pick",
        keyhint: "A",
        make: || Msg::CherryPick,
    },
    PaletteEntry {
        label: "Revert",
        keyhint: "V",
        make: || Msg::Revert,
    },
    PaletteEntry {
        label: "Fetch",
        keyhint: "f",
        make: || Msg::Fetch,
    },
    PaletteEntry {
        label: "Pull",
        keyhint: "F",
        make: || Msg::Pull,
    },
    PaletteEntry {
        label: "Push",
        keyhint: "P",
        make: || Msg::PushMenu,
    },
    PaletteEntry {
        label: "Remote",
        keyhint: "M",
        make: || Msg::RemoteMenu,
    },
    PaletteEntry {
        label: "Worktree",
        keyhint: "W",
        make: || Msg::WorktreeMenu,
    },
    PaletteEntry {
        label: "Pull requests",
        keyhint: "",
        make: || Msg::Forge,
    },
    PaletteEntry {
        label: "Resolve: take ours",
        keyhint: "",
        make: || Msg::ResolveOurs,
    },
    PaletteEntry {
        label: "Resolve: take theirs",
        keyhint: "",
        make: || Msg::ResolveTheirs,
    },
    PaletteEntry {
        label: "Bisect: start",
        keyhint: "",
        make: || Msg::BisectStartPrompt,
    },
    PaletteEntry {
        label: "Bisect: good",
        keyhint: "",
        make: || Msg::BisectGood,
    },
    PaletteEntry {
        label: "Bisect: bad",
        keyhint: "",
        make: || Msg::BisectBad,
    },
    PaletteEntry {
        label: "Bisect: reset",
        keyhint: "",
        make: || Msg::BisectReset,
    },
    PaletteEntry {
        label: "AI: review staged",
        keyhint: "",
        make: || Msg::AiReview,
    },
    PaletteEntry {
        label: "Undo last operation",
        keyhint: "",
        make: || Msg::Undo,
    },
    PaletteEntry {
        label: "Redo last undone operation",
        keyhint: "",
        make: || Msg::Redo,
    },
    PaletteEntry {
        label: "Refresh",
        keyhint: "g",
        make: || Msg::Refresh,
    },
    PaletteEntry {
        label: "Help",
        keyhint: "?",
        make: || Msg::HelpToggle,
    },
];

/// The fuzzy command palette overlay.
pub struct Palette {
    pub input: String,
    pub selected: usize,
}

/// The live code-search finder: an editable query, the current hits (lexical as
/// you type, re-ranked with the semantic index on demand), and the selection.
pub struct CodeFinder {
    pub input: String,
    pub selected: usize,
    pub hits: Vec<CodeHit>,
    /// True once the semantic index has been folded into `hits` for this query.
    pub semantic: bool,
    /// A search is in flight, for the spinner in the finder header.
    pub searching: bool,
}

impl Palette {
    /// Entries whose label matches the input as a case-insensitive subsequence.
    pub fn matches(input: &str) -> Vec<&'static PaletteEntry> {
        if input.is_empty() {
            return PALETTE.iter().collect();
        }
        let needle = input.to_lowercase();
        PALETTE
            .iter()
            .filter(|e| is_subsequence(&needle, &e.label.to_lowercase()))
            .collect()
    }
}

/// Whether every char of `needle` appears in `haystack` in order.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|n| chars.any(|h| h == n))
}

/// A side effect the runtime performs on the app's behalf. `update` returns
/// these instead of doing I/O itself, which keeps it pure and testable.
#[derive(Debug)]
pub enum Effect {
    Refresh,
    Mutate(Mutation),
    /// Copy text to the system clipboard (OSC 52).
    CopyToClipboard(String),
    /// Suspend the TUI, open the editor for a commit message, then commit
    /// (replacing HEAD when `amend`).
    Commit {
        amend: bool,
    },
    /// Suspend the TUI and run `git <args>` in the foreground (for operations
    /// that drive their own editor, like interactive rebase), then refresh.
    RunGit {
        args: Vec<String>,
        label: String,
    },
    /// Load the branch list, then open the checkout prompt.
    LoadBranches,
    /// Load the commit log with the given filters, then push the log view.
    LoadLog(LogOptions),
    /// Load a commit's details, then push the commit view.
    LoadCommit(String),
    /// Fetch and build a commit's or stash's diff for the preview pane off-thread.
    LoadPreview {
        key: PreviewKey,
        rev: String,
    },
    /// Build a large file diff for the preview pane off-thread.
    BuildFilePreview {
        key: PreviewKey,
        diff: Box<FileDiff>,
    },
    /// Blame a file, then push the blame view.
    LoadBlame(String),
    /// Grep-only search (instant), for the live finder as the query is typed.
    CodeSearchLive(String),
    /// Full search (grep fused with the semantic index) for the query.
    CodeSearch(String),
    /// Build a preview of `path` windowed around `line` for a code-search hit.
    BuildSnippetPreview {
        key: PreviewKey,
        path: String,
        line: usize,
    },
    /// Load all refs, then push the refs view.
    LoadRefs,
    /// Load the smartlog, then push the smartlog view.
    LoadSmartlog,
    /// Load the operation log, then push the oplog view.
    LoadOplog,
    /// Load the stacked-branch parents, then push the stack view.
    LoadStack,
    /// Fetch the current branch's forge PR/CI status for the REMOTE section.
    LoadRemoteStatus,
    /// Load the lanes state, then (re)build the lanes view.
    LoadLanes,
    /// Run a lane operation, toast the result, then reload the lanes view.
    LaneOp(LaneOp),
    /// Undo `n` operations in sequence (op-log restore-to-cursor).
    UndoTimes(usize),
    /// Run a text-returning backend op (flow/workspace), toast the result.
    RunText(TextOp),
    /// Load a read-only text panel (flow status, workspaces).
    LoadInfo {
        title: &'static str,
        kind: InfoKind,
    },
    /// Load configured remotes, then push the remotes view.
    LoadRemotes,
    /// Load remotes, then open a picker to push the current branch to one.
    LoadPushRemotes,
    /// Load linked worktrees, then push the worktrees view.
    LoadWorktrees,
    /// Load forge pull requests, then push the forge view.
    LoadForge,
    /// Open the in-app commit editor (loading the HEAD message when amending).
    OpenCommitEditor {
        amend: bool,
    },
    /// Commit via `git commit`, streaming its hook output into the hook console
    /// so the user watches the hooks run and, on failure, sees the full output.
    CommitConsole {
        amend: bool,
        message: String,
    },
    /// Run a network op via libgit2, streaming its git-style progress into the
    /// operation console.
    OpConsole(ConsoleOp),
    /// Draft a commit message from the staged diff with an LLM.
    GenerateCommit,
    /// Run an LLM review over the staged diff.
    ReviewStaged,
    /// Load the commits to rebase, then open the todo editor.
    LoadRebaseTodo {
        base: String,
    },
    /// Run an interactive rebase with the given todo text.
    RunRebaseTodo {
        base: String,
        todo: String,
    },
    /// Diff two revisions, then push the diff view.
    LoadDiff {
        from: String,
        to: String,
    },
}

/// The active level of the vim-profile leader (which-key) overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leader {
    /// Top level: git actions plus the `w` window group.
    Root,
    /// Window submenu: h/j/k/l focus panes.
    Window,
}

/// What the split preview pane is showing, used to skip needless rebuilds and
/// to key the async diff cache. File diffs come from the in-memory snapshot;
/// commit and stash diffs are fetched asynchronously and cached.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PreviewKey {
    File { path: String, staged: bool },
    Commit { id: String },
    Stash { index: usize },
    /// A window of a file's content around a code-search hit.
    Snippet { path: String, line: usize },
}

/// File diffs with more than this many lines are built off the main thread;
/// smaller ones build inline so the common case has no placeholder flash.
const ASYNC_DIFF_LINES: usize = 200;

/// Commits fetched per log auto-load-more step.
const LOG_BATCH: usize = 300;

impl PreviewKey {
    /// The revision to hand `commit_details` for the async-loaded kinds; `None`
    /// for file diffs, which are built synchronously from the snapshot.
    fn rev(&self) -> Option<String> {
        match self {
            PreviewKey::Commit { id } => Some(id.clone()),
            PreviewKey::Stash { index } => Some(format!("stash@{{{index}}}")),
            PreviewKey::File { .. } | PreviewKey::Snippet { .. } => None,
        }
    }
}

/// All app state. Rendering reads it; [`update`] is the only thing that mutates
/// it.
pub struct App {
    backend: Arc<dyn GitBackend>,
    /// The navigation stack; `views[0]` is the status screen, the last is active.
    views: Vec<View>,
    pub head: Option<Head>,
    /// The repository's sequencer state (rebase/merge/etc. in progress).
    pub state: RepoState,
    /// Count of changed working-tree entries, for the title badge.
    pub changed: usize,
    pub loading: bool,
    /// A labeled in-progress operation (e.g. "pushing"), shown in the status bar.
    pub busy: Option<String>,
    /// A destructive action awaiting confirmation.
    pub confirm: Option<PendingConfirm>,
    /// An active minibuffer prompt.
    pub prompt: Option<Prompt>,
    /// An active transient menu.
    pub transient: Option<Transient>,
    /// The fuzzy command palette, when open.
    pub palette: Option<Palette>,
    pub code_finder: Option<CodeFinder>,
    /// The in-app commit editor, when open.
    pub commit_editor: Option<CommitEditor>,
    /// The live hook console for an in-progress `git commit`, when open.
    pub hook_console: Option<HookConsole>,
    /// The in-app interactive-rebase editor, when open.
    pub rebase_todo: Option<RebaseTodo>,
    /// The vim-profile leader (which-key) overlay level, when open.
    pub leader: Option<Leader>,
    /// The incremental search input, when the search minibuffer is open.
    pub search: Option<String>,
    /// The last confirmed search query, repeated by `n` / `N`.
    last_search: Option<String>,
    /// Active corner notifications, newest last.
    pub toasts: Vec<Toast>,
    /// Monotonic logic-tick counter, driving the spinner and toast aging.
    pub tick_count: usize,
    /// Whether the full keybinding help overlay is showing.
    pub help: bool,
    /// Carries the log transient's `--all` toggle into a follow-up author prompt.
    pending_log_all: bool,
    /// Default number of commits the log view walks (from config).
    log_limit: usize,
    /// The filters the open log view was loaded with, so it can auto-extend.
    log_options: Option<LogOptions>,
    /// Commits currently loaded in the log view.
    log_loaded: usize,
    /// The walk reached the end of history (a load returned fewer than asked).
    log_exhausted: bool,
    /// A load-more request is in flight (avoids firing duplicates while scrolling).
    log_loading: bool,
    /// The severity filter for the session-log view.
    log_filter: LogFilter,
    /// Number of entries in the op-log view (0 = the empty placeholder row).
    oplog_len: usize,
    /// Sign commits via git's -S when set.
    gpg_sign: bool,
    /// Restack stacked children after an amend/reword/extend when set.
    auto_restack: bool,
    /// The lane targeted by a pending lane-commit prompt.
    pending_reword_rev: Option<String>,
    /// Push toggles (force, force-with-lease, set-upstream) stashed while the
    /// user picks a remote for "push to other remote".
    pending_push: Option<(bool, bool, bool)>,
    /// The merge transient's --no-ff toggle, stashed while the branch is picked.
    pending_merge_no_ff: bool,
    /// The commit to reorder / split, stashed between the two chained prompts.
    pending_reorder_rev: Option<String>,
    pending_split_rev: Option<String>,
    /// The file being renamed, stashed while the new name is entered.
    pending_move_from: Option<String>,
    pending_lane: Option<String>,
    /// The path targeted by a pending lane-assign prompt.
    pending_lane_path: Option<String>,
    /// Where to send the value of a pending credential prompt.
    pending_cred_reply: Option<std::sync::mpsc::Sender<Option<String>>>,
    /// The current branch's forge PR/CI status, shown as a REMOTE section. None
    /// until loaded or when the forge is unavailable.
    remote_status: Option<RemoteSummary>,
    /// When the forge status was last fetched, to throttle network calls.
    last_remote_fetch: Option<std::time::Instant>,
    /// Last status snapshot, for the adaptive preview pane.
    snapshot: Option<RepoStatus>,
    /// What the preview buffer currently holds, so it is rebuilt only when the
    /// cursor moves to a different file or commit.
    preview_key: Option<PreviewKey>,
    preview_buf: Buffer,
    /// A background preview build is in flight, so the pane shows a spinner.
    preview_loading: bool,
    /// Async-loaded commit/stash diffs, so revisiting one is instant instead of
    /// re-shelling git.
    preview_cache: HashMap<PreviewKey, Vec<Section>>,
    /// The hunk the preview is already scrolled to, so the follow-scroll search
    /// runs once per hunk change rather than on every 30fps frame.
    preview_followed_hunk: Option<u32>,
    /// Whether the preview pane currently has focus (set by keys).
    pub preview_focus: bool,
    /// Whether the split preview is on screen (set each frame by the renderer).
    pub preview_visible: bool,
    /// Left column of the preview pane when the split is on screen, so a mouse
    /// wheel can scroll whichever pane the pointer is over.
    pub split_x: Option<u16>,
    /// Terminal row where the navigator's first visible row is drawn, and the
    /// same for the preview, so a click maps to the right item under the header.
    pub body_top: u16,
    pub preview_top: u16,
    /// The last click (in-preview?, row, time), for double-click fold detection.
    last_click: Option<(bool, usize, std::time::Instant)>,
    /// Which regions let the terminal background show through.
    pub transparency: crate::config::Transparency,
    pub error: Option<String>,
    pub should_quit: bool,
}

impl App {
    pub fn new(backend: Arc<dyn GitBackend>, config: crate::config::Config) -> Self {
        Self {
            backend,
            views: vec![View {
                kind: ViewKind::Status,
                buffer: Buffer::default(),
            }],
            head: None,
            state: RepoState::Clean,
            changed: 0,
            loading: true,
            busy: None,
            confirm: None,
            prompt: None,
            transient: None,
            palette: None,
            code_finder: None,
            commit_editor: None,
            hook_console: None,
            rebase_todo: None,
            leader: None,
            search: None,
            last_search: None,
            toasts: Vec::new(),
            tick_count: 0,
            help: false,
            pending_log_all: false,
            log_limit: config.ui.log_limit,
            log_options: None,
            log_loaded: 0,
            log_exhausted: false,
            log_loading: false,
            log_filter: LogFilter::All,
            oplog_len: 0,
            gpg_sign: config.commit.gpg_sign,
            auto_restack: config.commit.auto_restack,
            pending_reword_rev: None,
            pending_push: None,
            pending_merge_no_ff: false,
            pending_reorder_rev: None,
            pending_split_rev: None,
            pending_move_from: None,
            pending_lane: None,
            pending_lane_path: None,
            pending_cred_reply: None,
            remote_status: None,
            last_remote_fetch: None,
            snapshot: None,
            preview_key: None,
            preview_buf: Buffer::default(),
            preview_loading: false,
            preview_cache: HashMap::new(),
            preview_followed_hunk: None,
            preview_focus: false,
            preview_visible: false,
            split_x: None,
            body_top: 1,
            preview_top: 1,
            last_click: None,
            transparency: config.ui.transparent,
            error: None,
            should_quit: false,
        }
    }

    pub fn backend(&self) -> Arc<dyn GitBackend> {
        self.backend.clone()
    }

    /// Whether an amend/reword/extend should restack the stacked children after.
    pub fn auto_restack(&self) -> bool {
        self.auto_restack
    }

    /// The current HEAD summary (branch, upstream, ahead/behind), if loaded.
    pub fn head(&self) -> Option<&Head> {
        self.head.as_ref()
    }

    /// The short id of the commit under the cursor, if any.
    fn cursor_commit_id(&self) -> Option<String> {
        match self.buffer().target_at_cursor() {
            Some(Target::Commit { id }) => Some(id),
            _ => None,
        }
    }

    /// The working directory's final component, for the title bar.
    pub fn repo_name(&self) -> String {
        self.backend
            .workdir()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The preview key for whatever the cursor points at: a file (or one of its
    /// hunks), a commit, or a stash. `None` when the row has no previewable
    /// target.
    fn cursor_preview_key(&self) -> Option<PreviewKey> {
        // The finder drives the preview from its own selection, not a buffer row.
        if let Some(f) = &self.code_finder {
            let h = f.hits.get(f.selected)?;
            return Some(PreviewKey::Snippet {
                path: h.path.clone(),
                line: h.line,
            });
        }
        match self.buffer().target_at_cursor()? {
            Target::File { path, staged } => Some(PreviewKey::File { path, staged }),
            Target::Hunk { path, staged, .. } => Some(PreviewKey::File { path, staged }),
            Target::Commit { id } => Some(PreviewKey::Commit { id }),
            Target::Stash { index } => Some(PreviewKey::Stash { index }),
            Target::CodeHit { path, line } => Some(PreviewKey::Snippet { path, line }),
            Target::Ref { .. } => None,
        }
    }

    /// Ensure the preview buffer holds the diff for whatever the cursor points
    /// at, and, for files, follow the hunk under the cursor. Returns whether a
    /// preview is available. Commit and stash diffs are loaded asynchronously;
    /// see [`sync_preview`](Self::sync_preview).
    pub fn refresh_preview(&mut self, height: usize) -> bool {
        match self.cursor_preview_key() {
            Some(PreviewKey::File { path, staged }) => {
                self.refresh_file_preview(&path, staged, height)
            }
            Some(key) => {
                // Populated by the async load flow; here we only confirm there
                // is something to show and size it.
                let showing =
                    self.preview_key.as_ref() == Some(&key) && !self.preview_buf.is_empty();
                if showing {
                    self.preview_buf.set_height(height);
                }
                showing
            }
            None => {
                // No previewable target here (Head, a section header, a graph
                // link row): collapse the split so the preview only shows while
                // the cursor is on a file or commit. Drop the stale key so
                // returning to the same file rebuilds it.
                self.preview_key = None;
                false
            }
        }
    }

    fn refresh_file_preview(&mut self, path: &str, staged: bool, height: usize) -> bool {
        let hunk_start = match self.buffer().target_at_cursor() {
            Some(Target::Hunk { new_start, .. }) => Some(new_start),
            _ => None,
        };
        let key = PreviewKey::File {
            path: path.to_owned(),
            staged,
        };
        if self.preview_key.as_ref() != Some(&key) {
            let sections = self.snapshot.as_ref().and_then(|status| {
                let diff = if staged {
                    status.staged_diff(path)
                } else {
                    status.unstaged_diff(path)
                }?;
                Some(build_diff(path, std::slice::from_ref(diff)))
            });
            match sections {
                Some(sections) => {
                    self.preview_buf = Buffer::default();
                    self.preview_buf.set_content(sections);
                    self.preview_key = Some(key);
                    self.preview_followed_hunk = None;
                }
                None => {
                    self.preview_key = None;
                    return false;
                }
            }
        }
        self.preview_buf.set_height(height);
        // Scroll to the hunk under the cursor, unless the user is driving the
        // preview themselves. The search over the whole diff is O(rows), so run
        // it only when the target hunk actually changes, not every frame.
        if self.preview_focus {
            self.preview_followed_hunk = None; // re-follow when focus returns
        } else if hunk_start != self.preview_followed_hunk {
            if let Some(header) = hunk_start.and_then(|ns| self.hunk_header(path, staged, ns)) {
                if let Some(idx) = self.preview_buf.search(&header, 0, true) {
                    self.preview_buf.set_cursor(idx);
                }
            }
            self.preview_followed_hunk = hunk_start;
        }
        true
    }

    /// Keep the preview pane in sync with whatever the cursor points at. Serves a
    /// cached diff immediately; otherwise defers the build to a background thread
    /// (with a placeholder) for commits, stashes, and large file diffs, while
    /// small file diffs are left for `refresh_preview` to build synchronously so
    /// the common case has no placeholder flash. A no-op while the preview itself
    /// is focused.
    pub fn sync_preview(&mut self) -> Vec<Effect> {
        if self.preview_focus {
            return Vec::new();
        }
        let Some(key) = self.cursor_preview_key() else {
            return Vec::new();
        };
        if self.preview_key.as_ref() == Some(&key) {
            return Vec::new();
        }
        if let Some(sections) = self.preview_cache.get(&key) {
            let sections = sections.clone();
            self.show_preview(key, sections);
            return Vec::new();
        }
        match &key {
            PreviewKey::File { path, staged } => {
                // Build small diffs synchronously (no flash); defer the ones
                // whose build - chiefly syntax highlighting - can be felt.
                let big = self.file_diff(path, *staged).and_then(|diff| {
                    let lines: usize = diff.hunks.iter().map(|h| h.lines.len()).sum();
                    (lines > ASYNC_DIFF_LINES).then(|| diff.clone())
                });
                match big {
                    Some(diff) => {
                        self.set_preview_placeholder();
                        self.preview_key = Some(key.clone());
                        self.preview_followed_hunk = None;
                        vec![Effect::BuildFilePreview {
                            key,
                            diff: Box::new(diff),
                        }]
                    }
                    None => Vec::new(),
                }
            }
            PreviewKey::Snippet { path, line } => {
                let (path, line) = (path.clone(), *line);
                self.set_preview_placeholder();
                self.preview_key = Some(key.clone());
                vec![Effect::BuildSnippetPreview { key, path, line }]
            }
            _ => {
                let rev = key.rev().expect("commit/stash keys resolve to a rev");
                self.set_preview_placeholder();
                self.preview_key = Some(key.clone());
                vec![Effect::LoadPreview { key, rev }]
            }
        }
    }

    /// Whether this click completes a double-click on the same pane and row as
    /// the previous one, within the double-click window. Records the click so
    /// the next one can compare.
    fn is_double_click(&mut self, preview: bool, idx: usize) -> bool {
        const WINDOW: std::time::Duration = std::time::Duration::from_millis(400);
        let now = std::time::Instant::now();
        let double = self
            .last_click
            .is_some_and(|(p, i, t)| p == preview && i == idx && now.duration_since(t) < WINDOW);
        // Reset after a double so a third quick click starts a fresh pair.
        self.last_click = if double {
            None
        } else {
            Some((preview, idx, now))
        };
        double
    }

    /// Open the session-log view: the in-memory log lines under the current
    /// filter, scrolled to the tail.
    pub fn open_session_log(&mut self) {
        let mut buffer = Buffer::default();
        buffer.set_content(build_session_log(
            &crate::session_log::snapshot(),
            self.log_filter,
        ));
        buffer.cursor_bottom();
        self.push_view(ViewKind::SessionLog, buffer);
    }

    /// The current session-log severity filter label, for the footer.
    pub fn log_filter_label(&self) -> &'static str {
        self.log_filter.label()
    }

    /// Rebuild the session-log view in place (after a filter change or refresh).
    fn refresh_session_log(&mut self) {
        if self.active_kind() != ViewKind::SessionLog {
            return;
        }
        let sections = build_session_log(&crate::session_log::snapshot(), self.log_filter);
        let buf = self.buffer_mut();
        buf.set_content(sections);
        buf.cursor_bottom();
    }

    /// Record the filters a fresh log load starts from, for auto-extension.
    fn begin_log(&mut self, opts: LogOptions) {
        self.log_options = Some(opts);
        self.log_loaded = 0;
        self.log_exhausted = false;
        self.log_loading = false;
    }

    /// Effects to run after the navigator cursor moves: keep the preview in sync
    /// and pull the next batch of log history when scrolling near the bottom.
    fn after_cursor_move(&mut self) -> Vec<Effect> {
        let mut fx = self.sync_preview();
        fx.extend(self.maybe_load_more_log());
        fx
    }

    /// When the log cursor nears the end of the loaded commits, fetch a larger
    /// batch and rebuild the view in place (the walk prefix is deterministic, so
    /// the cursor keeps its place). A no-op once history is exhausted.
    fn maybe_load_more_log(&mut self) -> Vec<Effect> {
        if self.active_kind() != ViewKind::Log || self.log_exhausted || self.log_loading {
            return Vec::new();
        }
        let Some(opts) = self.log_options.clone() else {
            return Vec::new();
        };
        let buf = self.buffer();
        let below = buf.len().saturating_sub(buf.cursor());
        // Prefetch when within roughly a screen of the last loaded commit.
        if below > buf.page() + 8 {
            return Vec::new();
        }
        self.log_loading = true;
        self.busy = Some("loading history".to_owned());
        let opts = LogOptions {
            limit: self.log_loaded + LOG_BATCH,
            ..opts
        };
        self.log_options = Some(opts.clone());
        vec![Effect::LoadLog(opts)]
    }

    fn file_diff(&self, path: &str, staged: bool) -> Option<&FileDiff> {
        let status = self.snapshot.as_ref()?;
        if staged {
            status.staged_diff(path)
        } else {
            status.unstaged_diff(path)
        }
    }

    fn set_preview_placeholder(&mut self) {
        self.preview_buf = Buffer::default();
        self.preview_loading = true;
    }

    /// Whether the preview pane is waiting on a background build.
    pub fn preview_loading(&self) -> bool {
        self.preview_loading
    }

    fn show_preview(&mut self, key: PreviewKey, sections: Vec<Section>) {
        self.preview_buf = Buffer::default();
        self.preview_buf.set_content(sections);
        self.preview_key = Some(key);
        self.preview_followed_hunk = None;
        self.preview_loading = false;
    }

    /// Cache a background-built preview and, if it is still under the cursor,
    /// show it. Covers commit, stash, and large-file diffs.
    pub fn preview_built(&mut self, key: PreviewKey, sections: Vec<Section>) {
        self.preview_cache.insert(key.clone(), sections.clone());
        if self.cursor_preview_key().as_ref() == Some(&key) {
            self.show_preview(key, sections);
        }
        // The awaited build is done; drop the spinner even if the cursor has
        // since moved off the target (the result is cached for later).
        self.preview_loading = false;
    }

    /// The header line of the hunk at `new_start` in the given file's diff.
    fn hunk_header(&self, path: &str, staged: bool, new_start: u32) -> Option<String> {
        let status = self.snapshot.as_ref()?;
        let diff = if staged {
            status.staged_diff(path)
        } else {
            status.unstaged_diff(path)
        }?;
        diff.hunks
            .iter()
            .find(|h| h.new_start == new_start)
            .map(|h| h.header.clone())
    }

    pub fn preview_buffer(&self) -> &Buffer {
        &self.preview_buf
    }

    /// The subject the preview pane is showing, for its header (a path, a commit
    /// id, or a stash ref).
    pub fn preview_title(&self) -> String {
        match &self.preview_key {
            Some(PreviewKey::File { path, .. }) => path.clone(),
            Some(PreviewKey::Snippet { path, line }) => format!("{path}:{line}"),
            Some(PreviewKey::Commit { id }) => id.clone(),
            Some(PreviewKey::Stash { index }) => format!("stash@{{{index}}}"),
            None => String::new(),
        }
    }

    /// Record whether the split preview is on screen; drops focus when it isn't.
    pub fn set_preview_visible(&mut self, visible: bool) {
        self.preview_visible = visible;
        if !visible {
            self.preview_focus = false;
        }
    }

    /// The buffer that keys act on: the focused preview, or the active view.
    pub fn active_buffer_mut(&mut self) -> &mut Buffer {
        if self.preview_focus {
            &mut self.preview_buf
        } else {
            self.buffer_mut()
        }
    }

    pub fn active_buffer(&self) -> &Buffer {
        if self.preview_focus {
            &self.preview_buf
        } else {
            self.buffer()
        }
    }

    /// Number of stashes in the last snapshot, for the status dashboard chip.
    pub fn stash_count(&self) -> usize {
        self.snapshot.as_ref().map_or(0, |s| s.stashes.len())
    }

    /// Advance the logic clock: bump the spinner and age out expired toasts.
    pub fn tick(&mut self) {
        self.tick_count = self.tick_count.wrapping_add(1);
        for toast in &mut self.toasts {
            toast.ticks = toast.ticks.saturating_sub(1);
        }
        self.toasts.retain(|t| t.ticks > 0);
    }

    /// Whether the screen has time-varying content (spinner, toasts) that needs
    /// periodic redraws even without input. When false, the loop can idle.
    pub fn is_animating(&self) -> bool {
        self.loading
            || self.busy.is_some()
            || self.preview_loading
            || !self.toasts.is_empty()
            || self
                .hook_console
                .as_ref()
                .is_some_and(|c| c.status == HookStatus::Running)
    }

    pub fn push_toast(&mut self, kind: ToastKind, text: String) {
        if kind == ToastKind::Error {
            tracing::warn!(error = %text, "operation failed");
        }
        // Errors linger longer than confirmations of success.
        let ticks = if kind == ToastKind::Error { 24 } else { 12 };
        self.toasts.push(Toast { text, kind, ticks });
    }

    /// The active screen's buffer.
    pub fn buffer(&self) -> &Buffer {
        &self.views.last().expect("nonempty view stack").buffer
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.views.last_mut().expect("nonempty view stack").buffer
    }

    pub fn active_kind(&self) -> ViewKind {
        self.views.last().expect("nonempty view stack").kind
    }

    /// Push a new screen onto the stack.
    pub fn push_view(&mut self, kind: ViewKind, buffer: Buffer) {
        self.views.push(View { kind, buffer });
    }

    /// Pop the active screen; returns false if already at the status root.
    pub fn pop_view(&mut self) -> bool {
        if self.views.len() > 1 {
            self.views.pop();
            true
        } else {
            false
        }
    }

    /// The status screen's buffer, updated by refreshes regardless of the active
    /// screen.
    fn status_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.views[0].buffer
    }
}

/// Apply a message, returning the effects the runtime should perform.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        // `q`/Esc walk back one step at a time: clear a visual selection, leave a
        // focused preview pane, then pop screens, and only quit at the root.
        Msg::Quit => {
            if app.active_buffer_mut().has_selection() {
                app.active_buffer_mut().clear_selection();
            } else if app.preview_focus {
                app.preview_focus = false;
            } else if !app.pop_view() {
                app.should_quit = true;
            }
        }
        Msg::CursorDown => {
            app.active_buffer_mut().move_cursor(1);
            return app.after_cursor_move();
        }
        Msg::CursorUp => {
            app.active_buffer_mut().move_cursor(-1);
            return app.after_cursor_move();
        }
        Msg::CursorTop => {
            app.active_buffer_mut().cursor_top();
            return app.after_cursor_move();
        }
        Msg::CursorBottom => {
            app.active_buffer_mut().cursor_bottom();
            return app.after_cursor_move();
        }
        Msg::CursorHalfDown => {
            let half = app.active_buffer_mut().page().max(1) as isize / 2;
            app.active_buffer_mut().move_cursor(half.max(1));
            return app.after_cursor_move();
        }
        Msg::CursorHalfUp => {
            let half = app.active_buffer_mut().page().max(1) as isize / 2;
            app.active_buffer_mut().move_cursor(-half.max(1));
            return app.after_cursor_move();
        }
        Msg::LeaderOpen => app.leader = Some(Leader::Root),
        // Move focus into the preview pane (only when the split is on screen).
        Msg::FocusPreview => {
            if app.preview_visible {
                app.preview_focus = true;
                app.preview_buf.cursor_to_first_foldable();
            }
        }
        Msg::FocusNav => app.preview_focus = false,
        // Fold whichever pane is focused: hunks in the preview, sections in nav.
        Msg::ToggleFold => app.active_buffer_mut().toggle_fold(),
        Msg::ToggleSelect => app.active_buffer_mut().toggle_selection(),
        Msg::ToggleCharSelect => app.active_buffer_mut().toggle_char_selection(),
        Msg::ColLeft => app.active_buffer_mut().move_col(-1),
        Msg::ColRight => app.active_buffer_mut().move_col(1),
        Msg::ColWordForward => app.active_buffer_mut().col_word_forward(),
        Msg::ColWordBack => app.active_buffer_mut().col_word_back(),
        Msg::ColWordEnd => app.active_buffer_mut().col_word_end(),
        Msg::ColLineStart => app.active_buffer_mut().col_line_start(),
        Msg::ColLineEnd => app.active_buffer_mut().col_line_end(),
        Msg::ColFirstNonBlank => app.active_buffer_mut().col_first_nonblank(),
        Msg::Yank => {
            let mut text = app.active_buffer_mut().selected_text();
            app.active_buffer_mut().clear_selection();
            if text.is_empty() {
                return Vec::new();
            }
            // Bound the payload: an oversized OSC 52 sequence can exceed a
            // terminal/tmux buffer limit and stall the write. Cap well under
            // tmux's ~75 KB sequence cap (base64 inflates the raw bytes ~4/3).
            const MAX_YANK: usize = 48 * 1024;
            let truncated = text.len() > MAX_YANK;
            if truncated {
                let mut end = MAX_YANK;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
            }
            let lines = text.lines().count();
            app.push_toast(
                ToastKind::Success,
                format!(
                    "yanked {lines} line{}{}",
                    if lines == 1 { "" } else { "s" },
                    if truncated { " (truncated)" } else { "" }
                ),
            );
            return vec![Effect::CopyToClipboard(text)];
        }
        Msg::Stage => return stage_at_cursor(app),
        Msg::Unstage => return unstage_at_cursor(app),
        Msg::StageAll => {
            app.loading = true;
            return vec![Effect::Mutate(Mutation::StageAll)];
        }
        Msg::UnstageAll => {
            app.loading = true;
            return vec![Effect::Mutate(Mutation::UnstageAll)];
        }
        Msg::Discard => discard_at_cursor(app),
        Msg::RmAtCursor => {
            if let Some(Target::File { path, .. }) = app.buffer().target_at_cursor() {
                app.confirm = Some(PendingConfirm {
                    prompt: format!("Remove {path} from the index and working tree?"),
                    mutation: Mutation::RemovePath(path),
                });
            }
        }
        Msg::MvAtCursor => {
            if let Some(Target::File { path, .. }) = app.buffer().target_at_cursor() {
                app.pending_move_from = Some(path.clone());
                revision_prompt(app, &format!("Rename {path} to"), PromptAction::MoveFile);
            }
        }
        Msg::CodeFinderOpen => {
            app.code_finder = Some(CodeFinder {
                input: String::new(),
                selected: 0,
                hits: Vec::new(),
                semantic: false,
                searching: false,
            });
        }
        Msg::CodeFinderChar(c) => return code_finder_edit(app, |f| f.input.push(c)),
        Msg::CodeFinderBackspace => {
            return code_finder_edit(app, |f| {
                f.input.pop();
            });
        }
        Msg::CodeFinderUp => {
            if let Some(f) = &mut app.code_finder {
                f.selected = f.selected.saturating_sub(1);
            }
            return app.sync_preview();
        }
        Msg::CodeFinderDown => {
            if let Some(f) = &mut app.code_finder {
                f.selected = (f.selected + 1).min(f.hits.len().saturating_sub(1));
            }
            return app.sync_preview();
        }
        Msg::CodeFinderSemantic => {
            if let Some(f) = &mut app.code_finder {
                let q = f.input.trim().to_owned();
                if !q.is_empty() {
                    f.searching = true;
                    return vec![Effect::CodeSearch(q)];
                }
            }
        }
        Msg::CodeFinderSubmit => {
            if let Some(f) = app.code_finder.take() {
                if let Some(h) = f.hits.get(f.selected) {
                    return vec![Effect::LoadBlame(h.path.clone())];
                }
            }
        }
        Msg::CodeFinderCancel => app.code_finder = None,
        Msg::CodeFinderHits {
            query,
            semantic,
            hits,
        } => {
            match &mut app.code_finder {
                // Ignore results for a query the user has since edited.
                Some(f) if f.input.trim() == query => {
                    f.searching = false;
                    match hits {
                        Ok(h) => {
                            f.hits = h;
                            f.semantic = semantic;
                            f.selected = f.selected.min(f.hits.len().saturating_sub(1));
                        }
                        Err(e) => {
                            f.hits.clear();
                            app.error = Some(e);
                        }
                    }
                }
                _ => return Vec::new(),
            }
            return app.sync_preview();
        }
        Msg::ConfirmAccept => {
            if let Some(pending) = app.confirm.take() {
                app.loading = true;
                return vec![Effect::Mutate(pending.mutation)];
            }
        }
        Msg::ConfirmCancel => app.confirm = None,
        Msg::CommitMenu => app.transient = Some(Transient::commit()),
        Msg::Log => app.transient = Some(Transient::log()),
        Msg::LogLoaded(entries) => {
            app.log_loading = false;
            app.busy = None;
            let asked = app.log_options.as_ref().map(|o| o.limit).unwrap_or(0);
            app.log_exhausted = entries.len() < asked;
            app.log_loaded = entries.len();
            let sections = build_log(&entries);
            if app.active_kind() == ViewKind::Log {
                // A load-more: extend the open view in place, keeping the cursor.
                app.buffer_mut().set_content(sections);
            } else {
                let mut buffer = Buffer::default();
                buffer.set_content(sections);
                app.push_view(ViewKind::Log, buffer);
            }
            return app.sync_preview();
        }
        // `RET` is section-aware: open a commit's diff, blame a file, or check
        // out a reference.
        Msg::Enter if app.active_kind() == ViewKind::Oplog => {
            // Ignore Enter on the "op-log is empty" placeholder row.
            if app.oplog_len > 0 {
                let steps = (app.buffer().cursor() + 1).min(app.oplog_len);
                app.pop_view();
                app.busy = Some("restoring".into());
                return vec![Effect::UndoTimes(steps)];
            }
        }
        Msg::Enter => match app.buffer().target_at_cursor() {
            Some(Target::Commit { id }) => return vec![Effect::LoadCommit(id)],
            Some(Target::CodeHit { path, .. }) => return vec![Effect::LoadBlame(path)],
            Some(Target::File { path, .. }) => return vec![Effect::LoadBlame(path)],
            Some(Target::Ref { name, kind }) => {
                app.pop_view();
                app.loading = true;
                let mutation = match kind {
                    RefTarget::Local => Mutation::CheckoutBranch(name),
                    RefTarget::Remote | RefTarget::Tag => Mutation::CheckoutDetached(name),
                };
                return vec![Effect::Mutate(mutation)];
            }
            _ => {}
        },
        Msg::Refs => return vec![Effect::LoadRefs],
        Msg::RefsLoaded(refs) => {
            let mut buffer = Buffer::default();
            buffer.set_content(build_refs(&refs));
            app.push_view(ViewKind::Refs, buffer);
        }
        Msg::OpenSmartlog => return vec![Effect::LoadSmartlog],
        Msg::SmartlogLoaded(entries) => {
            let mut buffer = Buffer::default();
            buffer.set_content(build_smartlog(&entries));
            app.push_view(ViewKind::Smartlog, buffer);
        }
        Msg::OpenOplog => return vec![Effect::LoadOplog],
        Msg::OplogLoaded(entries) => {
            app.oplog_len = entries.len();
            let mut buffer = Buffer::default();
            buffer.set_content(build_oplog(&entries));
            app.push_view(ViewKind::Oplog, buffer);
        }
        Msg::Restack => {
            app.busy = Some("restacking".into());
            return vec![Effect::Mutate(Mutation::Restack)];
        }
        Msg::Absorb => {
            app.busy = Some("absorbing".into());
            return vec![Effect::Mutate(Mutation::Absorb)];
        }
        Msg::FlowInitPrompt => revision_prompt(
            app,
            "Workflow (gitflow/github/gitlab/trunk/release-flow)",
            PromptAction::FlowInit,
        ),
        Msg::FlowStartPrompt => revision_prompt(app, "Feature name", PromptAction::FlowStart),
        Msg::FlowFinish => {
            app.busy = Some("finishing".into());
            return vec![Effect::RunText(TextOp::FlowFinish)];
        }
        Msg::OpenFlowStatus => {
            return vec![Effect::LoadInfo {
                title: "workflow",
                kind: InfoKind::FlowStatus,
            }];
        }
        Msg::WorkspaceNewPrompt => {
            revision_prompt(app, "Workspace name", PromptAction::WorkspaceNew)
        }
        Msg::StackNewPrompt => {
            revision_prompt(app, "New stacked branch name", PromptAction::StackNew)
        }
        Msg::OpenWorkspaces => {
            return vec![Effect::LoadInfo {
                title: "workspaces",
                kind: InfoKind::Workspaces,
            }];
        }
        Msg::AutoRestackNote(text) => app.push_toast(ToastKind::Success, text),
        Msg::RemoteStatusLoaded(summary) => {
            app.remote_status = summary;
            rebuild_status(app);
        }
        Msg::CredentialRequest {
            label,
            masked,
            reply,
        } => {
            app.pending_cred_reply = Some(reply);
            app.prompt = Some(Prompt {
                label,
                input: String::new(),
                cursor: 0,
                candidates: Vec::new(),
                selected: 0,
                action: PromptAction::Credential,
                masked,
            });
        }
        Msg::OpenLanes => return vec![Effect::LoadLanes],
        Msg::LanesLoaded(state) => {
            let content = build_lanes(&state);
            if app.active_kind() == ViewKind::Lanes {
                app.buffer_mut().set_content(content);
            } else {
                let mut buffer = Buffer::default();
                buffer.set_content(content);
                app.push_view(ViewKind::Lanes, buffer);
            }
        }
        Msg::LaneNotice(text) => {
            let kind = if text.starts_with("error") {
                ToastKind::Error
            } else {
                ToastKind::Success
            };
            app.push_toast(kind, text);
        }
        Msg::LaneNewPrompt => revision_prompt(app, "New lane name", PromptAction::LaneNew),
        Msg::LaneAssignPrompt => match file_at_cursor(app) {
            Some((_lane, path)) => {
                app.pending_lane_path = Some(path);
                revision_prompt(app, "Assign to lane", PromptAction::LaneAssign)
            }
            None => app.push_toast(ToastKind::Error, "move onto a file first".into()),
        },
        Msg::LaneUnassignAtCursor => match file_at_cursor(app) {
            Some((_lane, path)) => return vec![Effect::LaneOp(LaneOp::Unassign(path))],
            None => app.push_toast(ToastKind::Error, "move onto a file first".into()),
        },
        Msg::LaneCommitPrompt => match lane_at_cursor(app) {
            Some(lane) => {
                app.pending_lane = Some(lane);
                revision_prompt(app, "Commit message", PromptAction::LaneCommit)
            }
            None => app.push_toast(ToastKind::Error, "move onto a lane first".into()),
        },
        Msg::LaneRenamePrompt => match lane_at_cursor(app) {
            Some(lane) => {
                app.pending_lane = Some(lane);
                revision_prompt(app, "Rename lane to", PromptAction::LaneRename)
            }
            None => app.push_toast(ToastKind::Error, "move onto a lane first".into()),
        },
        Msg::LaneStackPrompt => match lane_at_cursor(app) {
            Some(parent) => {
                app.pending_lane = Some(parent.clone());
                revision_prompt(app, &format!("New lane stacked on {parent}"), PromptAction::LaneStack)
            }
            None => app.push_toast(ToastKind::Error, "move onto a lane first".into()),
        },
        Msg::LaneDeleteAtCursor => match lane_at_cursor(app) {
            Some(lane) => return vec![Effect::LaneOp(LaneOp::Delete(lane))],
            None => app.push_toast(ToastKind::Error, "move onto a lane first".into()),
        },
        Msg::LanePushAtCursor => match lane_at_cursor(app) {
            Some(lane) => return vec![Effect::LaneOp(LaneOp::Push(lane))],
            None => app.push_toast(ToastKind::Error, "move onto a lane first".into()),
        },
        Msg::LanePrAtCursor => match lane_at_cursor(app) {
            Some(lane) => return vec![Effect::LaneOp(LaneOp::Pr(lane))],
            None => app.push_toast(ToastKind::Error, "move onto a lane first".into()),
        },
        Msg::LaneRestack => return vec![Effect::LaneOp(LaneOp::Restack)],
        Msg::TextResult(Ok(text)) => {
            app.busy = None;
            app.push_toast(ToastKind::Success, text);
            return vec![Effect::Refresh];
        }
        Msg::TextResult(Err(e)) => {
            app.busy = None;
            app.push_toast(ToastKind::Error, e);
        }
        Msg::InfoLoaded { title, text } => {
            let mut buffer = Buffer::default();
            buffer.set_content(build_info(&title, &text));
            app.push_view(ViewKind::Info, buffer);
        }
        Msg::OpenStack => return vec![Effect::LoadStack],
        Msg::StackLoaded { parents, current } => {
            let mut buffer = Buffer::default();
            buffer.set_content(build_stack(&parents, current.as_deref()));
            app.push_view(ViewKind::Stack, buffer);
        }
        Msg::RemoteMenu => app.transient = Some(Transient::remote()),
        Msg::RemotesLoaded(remotes) => {
            let mut buffer = Buffer::default();
            buffer.set_content(build_remotes(&remotes));
            app.push_view(ViewKind::Remotes, buffer);
        }
        Msg::PushRemotesLoaded(remotes) => {
            if remotes.is_empty() {
                app.pending_push = None;
                app.push_toast(ToastKind::Error, "no remotes configured".into());
            } else {
                app.prompt = Some(Prompt {
                    label: "Push to remote".into(),
                    input: String::new(),
                    cursor: 0,
                    candidates: remotes.into_iter().map(|r| r.name).collect(),
                    selected: 0,
                    action: PromptAction::PushRemote,
                    masked: false,
                });
            }
        }
        Msg::WorktreeMenu => app.transient = Some(Transient::worktree()),
        Msg::WorktreesLoaded(worktrees) => {
            let mut buffer = Buffer::default();
            buffer.set_content(build_worktrees(&worktrees));
            app.push_view(ViewKind::Worktrees, buffer);
        }
        Msg::Forge => return vec![Effect::LoadForge],
        Msg::ForgeLoaded(Ok(prs)) => {
            let mut buffer = Buffer::default();
            buffer.set_content(forge::build_view(&prs));
            app.push_view(ViewKind::Forge, buffer);
        }
        Msg::ForgeLoaded(Err(e)) => app.push_toast(ToastKind::Error, e),
        Msg::Error(e) => {
            app.loading = false;
            app.busy = None;
            app.error = Some(e);
        }
        Msg::DiffPrompt => revision_prompt(app, "Diff (revA revB)", PromptAction::DiffRefs),
        Msg::DiffLoaded { title, files } => {
            let mut buffer = Buffer::default();
            buffer.set_content(build_diff(&title, &files));
            app.push_view(ViewKind::Diff, buffer);
        }
        Msg::CommitLoaded(details) => {
            let mut buffer = Buffer::default();
            buffer.set_content(build_commit(&details));
            app.push_view(ViewKind::Commit, buffer);
        }
        Msg::PreviewBuilt { key, sections } => app.preview_built(key, sections),
        Msg::BlameLoaded { path, lines } => {
            let mut buffer = Buffer::default();
            buffer.set_content(build_blame(&path, &lines));
            app.push_view(ViewKind::Blame, buffer);
        }
        Msg::BranchesLoaded(candidates) => {
            app.prompt = Some(Prompt {
                label: "Checkout branch".into(),
                input: String::new(),
                cursor: 0,
                candidates,
                selected: 0,
                action: PromptAction::CheckoutBranch,
                masked: false,
            });
        }
        Msg::PromptInput(key) => prompt_edit(app, |p| p.apply(key)),
        Msg::PromptUp => prompt_edit(app, |p| p.selected = p.selected.saturating_sub(1)),
        Msg::PromptDown => prompt_edit(app, |p| {
            let last = p.filtered().len().saturating_sub(1);
            p.selected = (p.selected + 1).min(last);
        }),
        Msg::PromptCancel => {
            app.prompt = None;
            // Unblock a waiting credential request so the op fails cleanly.
            if let Some(reply) = app.pending_cred_reply.take() {
                let _ = reply.send(None);
            }
        }
        Msg::PromptSubmit => return prompt_submit(app),
        Msg::Fetch => return open_op(app, ConsoleOp::Fetch),
        Msg::Pull => return open_op(app, ConsoleOp::Pull),
        Msg::PushMenu => app.transient = Some(Transient::push()),
        Msg::HelpToggle => app.help = !app.help,
        Msg::HelpClose => app.help = false,
        Msg::StashMenu => app.transient = Some(Transient::stash()),
        // Pop the stash under the cursor directly (no menu); errors off a stash.
        Msg::StashPop => return stash_at_point(app, Mutation::StashPop),
        Msg::OpenSessionLog => app.open_session_log(),
        Msg::CycleLogFilter => {
            app.log_filter = app.log_filter.next();
            app.refresh_session_log();
        }
        Msg::OperationsMenu => app.transient = Some(Transient::operations()),
        Msg::BranchMenu => app.transient = Some(Transient::branch()),
        Msg::RebaseMenu => {
            let rebasing = matches!(app.state, RepoState::Rebase);
            app.transient = Some(Transient::rebase(rebasing));
        }
        Msg::MergeMenu => app.transient = Some(Transient::merge()),
        Msg::ResetMenu => app.transient = Some(Transient::reset()),
        Msg::TagMenu => app.transient = Some(Transient::tag()),
        Msg::CherryPick => revision_prompt(app, "Cherry-pick", PromptAction::CherryPick),
        Msg::ResolveOurs => return resolve_conflict_at(app, true),
        Msg::ResolveTheirs => return resolve_conflict_at(app, false),
        Msg::BisectStartPrompt => {
            revision_prompt(app, "Bisect (bad good)", PromptAction::BisectStart);
        }
        Msg::BisectGood => return vec![bisect(app, "good")],
        Msg::BisectBad => return vec![bisect(app, "bad")],
        Msg::BisectReset => return vec![bisect(app, "reset")],
        Msg::AiReview => {
            app.busy = Some("reviewing".into());
            return vec![Effect::ReviewStaged];
        }
        Msg::Undo => {
            // Restore the previous op-log snapshot (HEAD, branch, and the whole
            // working tree), recovering uncommitted work too.
            app.busy = Some("undoing".into());
            return vec![Effect::Mutate(Mutation::Undo)];
        }
        Msg::Redo => {
            app.busy = Some("redoing".into());
            return vec![Effect::Mutate(Mutation::Redo)];
        }
        Msg::ShowRebaseTodo { base, entries } => {
            app.loading = false;
            app.rebase_todo = Some(RebaseTodo {
                base,
                entries: entries
                    .into_iter()
                    .map(|(short, subject)| TodoEntry {
                        action: 'p',
                        short,
                        subject,
                    })
                    .collect(),
                cursor: 0,
            });
        }
        Msg::RebaseTodoUp => {
            if let Some(t) = &mut app.rebase_todo {
                t.cursor = t.cursor.saturating_sub(1);
            }
        }
        Msg::RebaseTodoDown => {
            if let Some(t) = &mut app.rebase_todo {
                t.cursor = (t.cursor + 1).min(t.entries.len().saturating_sub(1));
            }
        }
        Msg::RebaseTodoMoveUp => {
            if let Some(t) = &mut app.rebase_todo {
                if t.cursor > 0 {
                    t.entries.swap(t.cursor, t.cursor - 1);
                    t.cursor -= 1;
                }
            }
        }
        Msg::RebaseTodoMoveDown => {
            if let Some(t) = &mut app.rebase_todo {
                if t.cursor + 1 < t.entries.len() {
                    t.entries.swap(t.cursor, t.cursor + 1);
                    t.cursor += 1;
                }
            }
        }
        Msg::RebaseTodoSetAction(c) => {
            if let Some(t) = &mut app.rebase_todo {
                if let Some(entry) = t.entries.get_mut(t.cursor) {
                    entry.action = c;
                }
            }
        }
        Msg::RebaseTodoCancel => app.rebase_todo = None,
        Msg::RebaseTodoRun => {
            if let Some(t) = &app.rebase_todo {
                if matches!(t.entries.first().map(|e| e.action), Some('s') | Some('f')) {
                    app.push_toast(
                        ToastKind::Error,
                        "first commit cannot be squash/fixup".into(),
                    );
                    return Vec::new();
                }
            }
            if let Some(t) = app.rebase_todo.take() {
                app.busy = Some("rebasing".into());
                return vec![Effect::RunRebaseTodo {
                    base: t.base.clone(),
                    todo: t.todo_text(),
                }];
            }
        }
        Msg::AiReviewLoaded(text) => {
            app.busy = None;
            let mut buffer = Buffer::default();
            let sections: Vec<_> = text
                .lines()
                .enumerate()
                .map(|(i, l)| {
                    rgit_model::Section::leaf(
                        format!("review/{i}"),
                        rgit_model::NodeKind::Info,
                        vec![rgit_model::Span::plain(l.to_owned())],
                    )
                })
                .collect();
            buffer.set_content(sections);
            app.push_view(ViewKind::Review, buffer);
        }
        Msg::Revert => revision_prompt(app, "Revert", PromptAction::Revert),
        Msg::TransientCancel => app.transient = None,
        Msg::TransientChar(c) => return transient_key(app, c),
        Msg::PaletteOpen => {
            app.palette = Some(Palette {
                input: String::new(),
                selected: 0,
            })
        }
        Msg::PaletteChar(c) => {
            if let Some(p) = &mut app.palette {
                p.input.push(c);
                p.selected = 0;
            }
        }
        Msg::PaletteBackspace => {
            if let Some(p) = &mut app.palette {
                p.input.pop();
                p.selected = 0;
            }
        }
        Msg::PaletteUp => {
            if let Some(p) = &mut app.palette {
                p.selected = p.selected.saturating_sub(1);
            }
        }
        Msg::PaletteDown => {
            if let Some(p) = &mut app.palette {
                let n = Palette::matches(&p.input).len();
                p.selected = (p.selected + 1).min(n.saturating_sub(1));
            }
        }
        Msg::SearchOpen => app.search = Some(String::new()),
        Msg::SearchChar(c) => {
            if let Some(s) = &mut app.search {
                s.push(c);
            }
            search_jump(app, app.buffer().cursor(), true);
        }
        Msg::SearchBackspace => {
            if let Some(s) = &mut app.search {
                s.pop();
            }
            search_jump(app, app.buffer().cursor(), true);
        }
        Msg::SearchSubmit => {
            if let Some(s) = app.search.take() {
                if !s.is_empty() {
                    app.last_search = Some(s);
                }
            }
        }
        Msg::SearchCancel => app.search = None,
        Msg::Scroll(delta) => {
            app.buffer_mut().scroll_by(delta);
            return app.after_cursor_move();
        }
        Msg::ScrollPreview(delta) => app.preview_buf.scroll_by(delta),
        Msg::ClickRow(offset) => {
            // A click in the navigator moves the cursor there and takes focus; a
            // double-click folds or expands the row under it.
            app.preview_focus = false;
            let idx = app.buffer().scroll() + offset;
            app.buffer_mut().set_cursor(idx);
            if app.is_double_click(false, idx) {
                app.buffer_mut().toggle_fold();
            }
            return app.after_cursor_move();
        }
        Msg::ClickPreview(offset) => {
            if app.preview_visible {
                app.preview_focus = true;
                let idx = app.preview_buf.scroll() + offset;
                app.preview_buf.set_cursor(idx);
                if app.is_double_click(true, idx) {
                    app.preview_buf.toggle_fold();
                }
            }
        }
        Msg::SearchNext => {
            let from = app.buffer().cursor() + 1;
            repeat_search(app, from, true);
        }
        Msg::SearchPrev => {
            let from = app.buffer().cursor().saturating_sub(1);
            repeat_search(app, from, false);
        }
        Msg::ShowCommitEditor { amend, prefill } => {
            app.commit_editor = Some(CommitEditor::new(amend, &prefill));
        }
        Msg::CommitEditorInput(key) => {
            if let Some(editor) = &mut app.commit_editor {
                editor.input(key);
            }
        }
        Msg::CommitEditorCancel => app.commit_editor = None,
        Msg::CommitEditorSubmit => {
            if let Some(editor) = &app.commit_editor {
                let message = editor.message();
                if message.is_empty() {
                    app.push_toast(ToastKind::Error, "empty commit message".into());
                } else {
                    let amend = editor.amend;
                    app.commit_editor = None;
                    if app.gpg_sign {
                        // Signing may prompt for a passphrase, which needs the
                        // real terminal; run it in the suspended-terminal console
                        // (hooks still run and show there).
                        app.busy = Some(if amend { "amending" } else { "committing" }.into());
                        let mut args = vec!["commit".to_owned(), "-S".to_owned()];
                        if amend {
                            args.push("--amend".to_owned());
                        }
                        args.push("-m".to_owned());
                        args.push(message);
                        return vec![Effect::RunGit {
                            args,
                            label: "committing".into(),
                        }];
                    }
                    // Run the commit through `git commit` and stream its hook
                    // output into the console, so hooks are visible and a hook
                    // rejection holds the commit with its errors on screen.
                    app.hook_console = Some(HookConsole::commit(amend, message.clone()));
                    return vec![Effect::CommitConsole { amend, message }];
                }
            }
        }
        Msg::CommitEditorEditor => {
            // Fall back to the external editor with whatever is typed so far.
            let amend = app.commit_editor.as_ref().map(|e| e.amend).unwrap_or(false);
            app.commit_editor = None;
            return vec![Effect::Commit { amend }];
        }
        Msg::CommitEditorGenerate => {
            if app.commit_editor.is_some() {
                app.busy = Some("generating".into());
                return vec![Effect::GenerateCommit];
            }
        }
        Msg::SetCommitEditorText(text) => {
            app.busy = None;
            if let Some(editor) = &mut app.commit_editor {
                editor.set_text(&text);
            }
        }
        Msg::HookOutput(line) => {
            if let Some(console) = &mut app.hook_console {
                console.lines.push(line);
            }
        }
        Msg::HookProgress { received, total } => {
            if let Some(console) = &mut app.hook_console {
                console.progress = Some((received, total));
            }
        }
        Msg::HookFinished { ok, summary } => {
            if let Some(console) = &mut app.hook_console {
                console.status = if ok {
                    HookStatus::Passed
                } else {
                    HookStatus::Failed
                };
            }
            if ok {
                // Committed: close the console, echo the git-style result, and
                // refresh the working tree.
                app.hook_console = None;
                if let Some(summary) = summary {
                    app.push_toast(ToastKind::Success, summary);
                }
                app.loading = true;
                return vec![Effect::Refresh];
            }
        }
        Msg::HookScroll(delta) => {
            if let Some(console) = &mut app.hook_console {
                let next = console.scroll.saturating_add_signed(delta);
                console.scroll = next.min(console.lines.len().saturating_sub(1));
                // Leaving the tail stops auto-follow; returning to it resumes.
                console.follow = console.scroll >= console.lines.len().saturating_sub(1);
            }
        }
        Msg::HookConsoleClose => app.hook_console = None,
        Msg::HookConsoleRetry => {
            if let Some(console) = app.hook_console.take() {
                match console.net {
                    // Re-run a failed network op in a fresh console.
                    Some(op) => return open_op(app, op),
                    // Reopen the commit editor with the rejected message.
                    None => {
                        app.commit_editor =
                            Some(CommitEditor::new(console.amend, &console.message));
                    }
                }
            }
        }
        Msg::PaletteCancel => app.palette = None,
        Msg::PaletteSubmit => {
            if let Some(p) = app.palette.take() {
                if let Some(entry) = Palette::matches(&p.input).get(p.selected) {
                    let msg = (entry.make)();
                    return update(app, msg);
                }
            }
        }
        Msg::Refresh => {
            // In the session-log view, refresh rebuilds the log in place.
            if app.active_kind() == ViewKind::SessionLog {
                app.refresh_session_log();
                return Vec::new();
            }
            app.loading = true;
            app.error = None;
            return vec![Effect::Refresh];
        }
        Msg::AutoRefresh => return vec![Effect::Refresh],
        Msg::Refreshed(result) => return refreshed(app, *result),
    }
    Vec::new()
}

/// `s` stages the unstaged region, file, or hunk under the cursor; on an
/// already-staged target it does nothing.
fn stage_at_cursor(app: &mut App) -> Vec<Effect> {
    if let Some(sel) = app.buffer().line_selection() {
        if !sel.staged {
            app.buffer_mut().toggle_selection();
            app.loading = true;
            return vec![Effect::Mutate(Mutation::StageLines {
                path: sel.path,
                new_start: sel.new_start,
                lines: sel.lines,
            })];
        }
    }
    let mutation = match app.buffer().target_at_cursor() {
        Some(Target::File {
            path,
            staged: false,
        }) => Mutation::StageFile(path),
        Some(Target::Hunk {
            path,
            new_start,
            staged: false,
        }) => Mutation::StageHunk { path, new_start },
        _ => return Vec::new(),
    };
    app.loading = true;
    vec![Effect::Mutate(mutation)]
}

/// `u` unstages the staged region, file, or hunk under the cursor.
fn unstage_at_cursor(app: &mut App) -> Vec<Effect> {
    if let Some(sel) = app.buffer().line_selection() {
        if sel.staged {
            app.buffer_mut().toggle_selection();
            app.loading = true;
            return vec![Effect::Mutate(Mutation::UnstageLines {
                path: sel.path,
                new_start: sel.new_start,
                lines: sel.lines,
            })];
        }
    }
    let mutation = match app.buffer().target_at_cursor() {
        Some(Target::File { path, staged: true }) => Mutation::UnstageFile(path),
        Some(Target::Hunk {
            path,
            new_start,
            staged: true,
        }) => Mutation::UnstageHunk { path, new_start },
        _ => return Vec::new(),
    };
    app.loading = true;
    vec![Effect::Mutate(mutation)]
}

/// `x` asks to discard the unstaged region, file, or hunk under the cursor,
/// arming a confirmation rather than acting immediately.
/// Apply an edit to the code-finder query, then kick off a fresh lexical search
/// (instant); an empty query just clears the results. The semantic flag resets
/// so the user knows the shown hits are lexical until they re-rank.
fn code_finder_edit(app: &mut App, edit: impl FnOnce(&mut CodeFinder)) -> Vec<Effect> {
    let Some(f) = &mut app.code_finder else {
        return Vec::new();
    };
    edit(f);
    f.selected = 0;
    f.semantic = false;
    let q = f.input.trim().to_owned();
    if q.is_empty() {
        f.hits.clear();
        f.searching = false;
        return app.sync_preview();
    }
    f.searching = true;
    vec![Effect::CodeSearchLive(q)]
}

fn discard_at_cursor(app: &mut App) {
    let (mutation, what) = if let Some(sel) = app.buffer().line_selection() {
        if sel.staged {
            return;
        }
        let what = format!("{} line(s) in {}", sel.lines.len(), sel.path);
        app.buffer_mut().toggle_selection();
        (
            Mutation::DiscardLines {
                path: sel.path,
                new_start: sel.new_start,
                lines: sel.lines,
            },
            what,
        )
    } else {
        match app.buffer().target_at_cursor() {
            Some(Target::File {
                path,
                staged: false,
            }) => (
                Mutation::DiscardFile(path.clone()),
                format!("changes to {path}"),
            ),
            Some(Target::Hunk {
                path,
                new_start,
                staged: false,
            }) => (
                Mutation::DiscardHunk {
                    path: path.clone(),
                    new_start,
                },
                format!("a hunk in {path}"),
            ),
            Some(Target::Stash { index }) => {
                (Mutation::StashDrop(index), format!("stash@{{{index}}}"))
            }
            Some(Target::Ref {
                name,
                kind: RefTarget::Local,
            }) => (
                Mutation::DeleteBranch(name.clone()),
                format!("branch {name}"),
            ),
            Some(Target::Ref {
                name,
                kind: RefTarget::Tag,
            }) => (Mutation::DeleteTag(name.clone()), format!("tag {name}")),
            _ => return,
        }
    };

    app.confirm = Some(PendingConfirm {
        prompt: format!("Discard {what}?"),
        mutation,
    });
}

fn prompt_edit(app: &mut App, edit: impl FnOnce(&mut Prompt)) {
    if let Some(prompt) = &mut app.prompt {
        edit(prompt);
    }
}

/// Char index of the start of the word before `pos` (skip spaces, then word).
fn word_start(chars: &[char], pos: usize) -> usize {
    let mut i = pos.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

/// Char index of the end of the word at or after `pos` (skip spaces, then word).
fn word_end(chars: &[char], pos: usize) -> usize {
    let mut i = pos.min(chars.len());
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    i
}

fn prompt_submit(app: &mut App) -> Vec<Effect> {
    let Some(prompt) = app.prompt.take() else {
        return Vec::new();
    };
    // A credential prompt answers the waiting network op; an empty entry cancels.
    if let PromptAction::Credential = prompt.action {
        let value = prompt.value();
        if let Some(reply) = app.pending_cred_reply.take() {
            let _ = reply.send((!value.is_empty()).then_some(value));
        }
        return Vec::new();
    }
    let value = prompt.value();
    if value.is_empty() {
        return Vec::new();
    }
    // "name url" is split into two arguments for a remote add.
    if let PromptAction::AddRemote = prompt.action {
        let mut parts = value.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some(name), Some(url)) => {
                app.loading = true;
                return vec![Effect::Mutate(Mutation::AddRemote {
                    name: name.to_owned(),
                    url: url.to_owned(),
                })];
            }
            _ => {
                app.error = Some("expected: <name> <url>".into());
                return Vec::new();
            }
        }
    }
    // "name path" is split into two arguments for a worktree add.
    if let PromptAction::AddWorktree = prompt.action {
        let mut parts = value.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some(name), Some(path)) => {
                app.loading = true;
                return vec![Effect::Mutate(Mutation::AddWorktree {
                    name: name.to_owned(),
                    path: path.to_owned(),
                })];
            }
            _ => {
                app.error = Some("expected: <name> <path>".into());
                return Vec::new();
            }
        }
    }
    // Bisect start takes a bad and a good revision.
    if let PromptAction::BisectStart = prompt.action {
        let mut parts = value.split_whitespace();
        if let (Some(bad), Some(good)) = (parts.next(), parts.next()) {
            app.busy = Some("bisecting".into());
            return vec![Effect::Mutate(Mutation::Bisect(vec![
                "start".into(),
                bad.into(),
                good.into(),
            ]))];
        }
        app.error = Some("expected: <bad> <good>".into());
        return Vec::new();
    }
    // A diff of two revisions (one rev diffs against HEAD).
    if let PromptAction::DiffRefs = prompt.action {
        let mut parts = value.split_whitespace();
        let (from, to) = match (parts.next(), parts.next()) {
            (Some(a), Some(b)) => (a.to_owned(), b.to_owned()),
            (Some(a), None) => ("HEAD".to_owned(), a.to_owned()),
            _ => return Vec::new(),
        };
        return vec![Effect::LoadDiff { from, to }];
    }
    // A log author filter loads a filtered log view, not a mutation.
    if let PromptAction::LogAuthor = prompt.action {
        let opts = LogOptions {
            all: app.pending_log_all,
            author: Some(value),
            limit: app.log_limit,
            ..LogOptions::default()
        };
        app.begin_log(opts.clone());
        return vec![Effect::LoadLog(opts)];
    }
    // Interactive rebase opens the in-app todo editor for the given base.
    if let PromptAction::RebaseInteractive = prompt.action {
        app.loading = true;
        return vec![Effect::LoadRebaseTodo { base: value }];
    }
    // Merge and rebase-onto stream their git-style output into the console.
    if let PromptAction::MergeBranch = prompt.action {
        let no_ff = std::mem::take(&mut app.pending_merge_no_ff);
        return open_op(app, ConsoleOp::Merge { rev: value, no_ff });
    }
    if let PromptAction::RebaseOnto = prompt.action {
        return open_op(app, ConsoleOp::RebaseOnto(value));
    }
    // Lane operations run and then reload the lanes view.
    match prompt.action {
        PromptAction::LaneNew => return vec![Effect::LaneOp(LaneOp::New(value))],
        PromptAction::LaneAssign => {
            if let Some(path) = app.pending_lane_path.take() {
                return vec![Effect::LaneOp(LaneOp::Assign { lane: value, path })];
            }
            return Vec::new();
        }
        PromptAction::LaneCommit => {
            if let Some(lane) = app.pending_lane.take() {
                return vec![Effect::LaneOp(LaneOp::Commit { lane, message: value })];
            }
            return Vec::new();
        }
        PromptAction::LaneRename => {
            if let Some(old) = app.pending_lane.take() {
                return vec![Effect::LaneOp(LaneOp::Rename { old, new: value })];
            }
            return Vec::new();
        }
        PromptAction::LaneStack => {
            if let Some(parent) = app.pending_lane.take() {
                return vec![Effect::LaneOp(LaneOp::Stack { name: value, parent })];
            }
            return Vec::new();
        }
        PromptAction::PushRemote => {
            let (force, force_with_lease, set_upstream) = app.pending_push.take().unwrap_or_default();
            return open_op(
                app,
                ConsoleOp::Push {
                    force,
                    force_with_lease,
                    set_upstream,
                    remote: Some(value),
                },
            );
        }
        // Reorder is two steps: pick the commit, then the target it moves before.
        PromptAction::ReorderRev => {
            if value.trim().is_empty() {
                return Vec::new();
            }
            app.pending_reorder_rev = Some(value);
            revision_prompt(app, "Move it before which commit", PromptAction::ReorderTarget);
            return Vec::new();
        }
        // Split is two steps: pick the commit, then the paths for the first part.
        PromptAction::SplitRev => {
            let rev = if value.trim().is_empty() {
                "HEAD".to_owned()
            } else {
                value
            };
            app.pending_split_rev = Some(rev);
            revision_prompt(app, "Paths for the first commit (comma-separated)", PromptAction::SplitPaths);
            return Vec::new();
        }
        // Reword is a two-step prompt: pick the commit, then its new message.
        PromptAction::RewordRev => {
            let rev = if value.trim().is_empty() {
                "HEAD".to_owned()
            } else {
                value
            };
            app.pending_reword_rev = Some(rev);
            revision_prompt(app, "New message", PromptAction::RewordMessage);
            return Vec::new();
        }
        _ => {}
    }
    // Flow/workspace ops return a status string, shown as a toast.
    let text_op = match prompt.action {
        PromptAction::FlowInit => Some(TextOp::FlowInit(value.clone())),
        PromptAction::FlowStart => Some(TextOp::FlowStart(value.clone())),
        PromptAction::WorkspaceNew => Some(TextOp::WorkspaceNew(value.clone())),
        PromptAction::StackNew => Some(TextOp::StackNew(value.clone())),
        _ => None,
    };
    if let Some(op) = text_op {
        app.busy = Some("working".into());
        return vec![Effect::RunText(op)];
    }
    let mutation = match prompt.action {
        PromptAction::CheckoutBranch => Mutation::CheckoutBranch(value),
        PromptAction::CreateBranch => Mutation::CreateBranch(value),
        // Handled above via an early return.
        PromptAction::RebaseOnto | PromptAction::MergeBranch => return Vec::new(),
        PromptAction::RebaseInteractive => return Vec::new(),
        PromptAction::ResetSoft => Mutation::Reset {
            rev: value,
            mode: ResetMode::Soft,
        },
        PromptAction::ResetMixed => Mutation::Reset {
            rev: value,
            mode: ResetMode::Mixed,
        },
        PromptAction::ResetHard => Mutation::Reset {
            rev: value,
            mode: ResetMode::Hard,
        },
        PromptAction::CherryPick => Mutation::CherryPick(value),
        PromptAction::Revert => Mutation::Revert(value),
        PromptAction::RewordMessage => match app.pending_reword_rev.take() {
            Some(rev) => Mutation::Reword {
                rev,
                message: value,
            },
            None => return Vec::new(),
        },
        PromptAction::Squash => Mutation::Squash(value),
        PromptAction::ReorderTarget => match app.pending_reorder_rev.take() {
            Some(rev) => Mutation::Reorder { rev, target: value },
            None => return Vec::new(),
        },
        PromptAction::SplitPaths => match app.pending_split_rev.take() {
            Some(rev) => Mutation::Split {
                rev,
                paths: value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            },
            None => return Vec::new(),
        },
        PromptAction::MoveFile => match app.pending_move_from.take() {
            Some(from) => Mutation::MovePath { from, to: value },
            None => return Vec::new(),
        },
        PromptAction::CreateTag => Mutation::CreateTag(value),
        PromptAction::DeleteTag => Mutation::DeleteTag(value),
        PromptAction::StashMessage => Mutation::StashPushMessage(value),
        PromptAction::RemoveRemote => Mutation::RemoveRemote(value),
        PromptAction::RemoveWorktree => Mutation::RemoveWorktree(value),
        // Handled above via an early return.
        PromptAction::AddRemote
        | PromptAction::AddWorktree
        | PromptAction::LogAuthor
        | PromptAction::DiffRefs
        | PromptAction::BisectStart
        | PromptAction::FlowInit
        | PromptAction::FlowStart
        | PromptAction::WorkspaceNew
        | PromptAction::StackNew
        | PromptAction::LaneNew
        | PromptAction::LaneAssign
        | PromptAction::LaneCommit
        | PromptAction::LaneRename
        | PromptAction::LaneStack
        | PromptAction::RewordRev
        | PromptAction::PushRemote
        | PromptAction::ReorderRev
        | PromptAction::SplitRev
        | PromptAction::Credential => {
            return Vec::new();
        }
        PromptAction::DeleteBranch => Mutation::DeleteBranch(value),
        PromptAction::RenameBranch => {
            let old = app
                .head
                .as_ref()
                .and_then(|h| h.branch.clone())
                .unwrap_or_default();
            Mutation::RenameBranch { old, new: value }
        }
    };
    app.loading = true;
    vec![Effect::Mutate(mutation)]
}

/// A key in an open transient: toggle a matching argument, or fire a matching
/// action with the accumulated arguments.
fn transient_key(app: &mut App, c: char) -> Vec<Effect> {
    if let Some(t) = app.transient.as_mut() {
        if let Some(arg) = t.args.iter_mut().find(|a| a.key == c) {
            arg.on = !arg.on;
            return Vec::new();
        }
    }
    let Some(t) = app.transient.as_ref() else {
        return Vec::new();
    };
    let Some(action) = t.actions.iter().find(|a| a.key == c) else {
        return Vec::new();
    };
    let kind = action.kind;
    let t_lease = t.arg_on('f');
    let t_force = t.arg_on('F');
    let set_upstream = t.arg_on('u');
    let t_all = t.arg_on('a');
    let t_no_ff = t.arg_on('n');
    app.transient = None;

    match kind {
        ActionKind::Push => open_op(
            app,
            ConsoleOp::Push {
                force: t_force,
                force_with_lease: t_lease,
                set_upstream,
                remote: None,
            },
        ),
        ActionKind::PushElsewhere => {
            // Stash the toggles; the remote is chosen in the next prompt.
            app.pending_push = Some((t_force, t_lease, set_upstream));
            vec![Effect::LoadPushRemotes]
        }
        ActionKind::Commit => vec![Effect::OpenCommitEditor { amend: false }],
        ActionKind::Amend => vec![Effect::OpenCommitEditor { amend: true }],
        ActionKind::Extend => {
            app.loading = true;
            vec![Effect::Mutate(Mutation::Extend)]
        }
        ActionKind::RebaseUpstream => open_op(app, ConsoleOp::RebaseOnto("@{upstream}".into())),
        ActionKind::RebaseAbort => {
            app.loading = true;
            vec![Effect::Mutate(Mutation::RebaseAbort)]
        }
        ActionKind::RebaseElsewhere => {
            revision_prompt(app, "Rebase onto", PromptAction::RebaseOnto);
            Vec::new()
        }
        ActionKind::RebaseInteractive => {
            app.transient = None;
            // Pointing at a commit already answers "from where"; open its todo
            // directly (that commit and everything after it become editable).
            // Otherwise ask for the starting point - not an "onto" target; an
            // interactive rebase replays in place, it does not relocate.
            if let Some(id) = app.cursor_commit_id() {
                app.loading = true;
                vec![Effect::LoadRebaseTodo {
                    base: format!("{id}~1"),
                }]
            } else {
                revision_prompt(
                    app,
                    "Interactive rebase from",
                    PromptAction::RebaseInteractive,
                );
                Vec::new()
            }
        }
        ActionKind::RebaseContinue => {
            app.transient = None;
            app.busy = Some("rebasing".into());
            vec![Effect::Mutate(Mutation::RebaseContinue)]
        }
        ActionKind::RebaseSkip => {
            app.transient = None;
            app.busy = Some("rebasing".into());
            vec![Effect::Mutate(Mutation::RebaseSkip)]
        }
        ActionKind::ResetSoft => {
            revision_prompt(app, "Reset soft to", PromptAction::ResetSoft);
            Vec::new()
        }
        ActionKind::ResetMixed => {
            revision_prompt(app, "Reset mixed to", PromptAction::ResetMixed);
            Vec::new()
        }
        ActionKind::ResetHard => {
            revision_prompt(app, "Reset HARD to", PromptAction::ResetHard);
            Vec::new()
        }
        ActionKind::MergeBranch => {
            app.pending_merge_no_ff = t_no_ff;
            revision_prompt(app, "Merge", PromptAction::MergeBranch);
            Vec::new()
        }
        ActionKind::TagCreate => {
            revision_prompt(app, "Tag name", PromptAction::CreateTag);
            Vec::new()
        }
        ActionKind::TagDelete => {
            revision_prompt(app, "Delete tag", PromptAction::DeleteTag);
            Vec::new()
        }
        ActionKind::StashPush => {
            app.transient = None;
            app.loading = true;
            vec![Effect::Mutate(Mutation::StashPush)]
        }
        ActionKind::StashMessage => {
            revision_prompt(app, "Stash message", PromptAction::StashMessage);
            Vec::new()
        }
        ActionKind::StashPopAt => stash_at_point(app, Mutation::StashPop),
        ActionKind::StashApplyAt => stash_at_point(app, Mutation::StashApply),
        ActionKind::StashDropAt => stash_at_point(app, Mutation::StashDrop),
        ActionKind::OpUndo => update(app, Msg::Undo),
        ActionKind::OpRedo => update(app, Msg::Redo),
        ActionKind::OpOplog => update(app, Msg::OpenOplog),
        ActionKind::OpSmartlog => update(app, Msg::OpenSmartlog),
        ActionKind::OpStack => update(app, Msg::OpenStack),
        ActionKind::OpStackNew => update(app, Msg::StackNewPrompt),
        ActionKind::OpRestack => update(app, Msg::Restack),
        ActionKind::OpAbsorb => update(app, Msg::Absorb),
        ActionKind::Reword => {
            revision_prompt(app, "Reword commit (empty = HEAD)", PromptAction::RewordRev);
            Vec::new()
        }
        ActionKind::Squash => {
            revision_prompt(app, "Squash into parent (commit)", PromptAction::Squash);
            Vec::new()
        }
        ActionKind::Uncommit => {
            app.transient = None;
            app.loading = true;
            vec![Effect::Mutate(Mutation::Uncommit(1))]
        }
        ActionKind::Reorder => {
            revision_prompt(app, "Move which commit", PromptAction::ReorderRev);
            Vec::new()
        }
        ActionKind::Split => {
            revision_prompt(app, "Split which commit (empty = HEAD)", PromptAction::SplitRev);
            Vec::new()
        }
        ActionKind::OpSync => open_op(app, ConsoleOp::Sync),
        ActionKind::OpSubmit => open_op(app, ConsoleOp::Submit),
        ActionKind::OpStackNext => {
            app.transient = None;
            app.loading = true;
            vec![Effect::Mutate(Mutation::StackNext)]
        }
        ActionKind::OpStackPrev => {
            app.transient = None;
            app.loading = true;
            vec![Effect::Mutate(Mutation::StackPrev)]
        }
        ActionKind::OpPruneMerged => {
            app.transient = None;
            app.busy = Some("pruning".into());
            vec![Effect::RunText(TextOp::PruneMerged("HEAD".into()))]
        }
        ActionKind::OpClean => {
            app.transient = None;
            app.confirm = Some(PendingConfirm {
                prompt: "Remove all untracked files and directories?".into(),
                mutation: Mutation::Clean,
            });
            Vec::new()
        }
        ActionKind::OpFlowStatus => update(app, Msg::OpenFlowStatus),
        ActionKind::OpWorkspaces => update(app, Msg::OpenWorkspaces),
        ActionKind::OpLanes => update(app, Msg::OpenLanes),
        ActionKind::BranchCheckout => {
            app.transient = None;
            vec![Effect::LoadBranches]
        }
        ActionKind::BranchCreateNew => {
            revision_prompt(app, "Create branch", PromptAction::CreateBranch);
            Vec::new()
        }
        ActionKind::BranchRename => {
            match app.head.as_ref().and_then(|h| h.branch.clone()) {
                Some(_) => revision_prompt(app, "Rename branch to", PromptAction::RenameBranch),
                None => app.error = Some("HEAD is detached; no branch to rename".into()),
            }
            Vec::new()
        }
        ActionKind::BranchDelete => {
            revision_prompt(app, "Delete branch", PromptAction::DeleteBranch);
            Vec::new()
        }
        ActionKind::RemoteList => {
            app.transient = None;
            vec![Effect::LoadRemotes]
        }
        ActionKind::RemoteAdd => {
            revision_prompt(app, "Add remote (name url)", PromptAction::AddRemote);
            Vec::new()
        }
        ActionKind::RemoteRemove => {
            revision_prompt(app, "Remove remote", PromptAction::RemoveRemote);
            Vec::new()
        }
        ActionKind::WorktreeList => {
            app.transient = None;
            vec![Effect::LoadWorktrees]
        }
        ActionKind::WorktreeAdd => {
            revision_prompt(app, "Add worktree (name path)", PromptAction::AddWorktree);
            Vec::new()
        }
        ActionKind::WorktreeRemove => {
            revision_prompt(app, "Remove worktree", PromptAction::RemoveWorktree);
            Vec::new()
        }
        ActionKind::LogShow => {
            let opts = LogOptions {
                all: t_all,
                limit: app.log_limit,
                ..LogOptions::default()
            };
            app.begin_log(opts.clone());
            vec![Effect::LoadLog(opts)]
        }
        ActionKind::LogAuthor => {
            app.pending_log_all = t_all;
            revision_prompt(app, "Log by author", PromptAction::LogAuthor);
            Vec::new()
        }
    }
}

/// Resolve the stash under the cursor and fire `make` with its index.
fn stash_at_point(app: &mut App, make: impl FnOnce(usize) -> Mutation) -> Vec<Effect> {
    app.transient = None;
    if let Some(Target::Stash { index }) = app.buffer().target_at_cursor() {
        app.loading = true;
        vec![Effect::Mutate(make(index))]
    } else {
        app.error = Some("point is not on a stash".into());
        Vec::new()
    }
}

/// Start a network operation, labeling the busy state so the status bar reflects
/// it until the follow-up refresh clears it.
/// Build the session-log view: one colored row per captured line under the
/// filter, newest last.
fn build_session_log(lines: &[crate::session_log::LogLine], filter: LogFilter) -> Vec<Section> {
    use crate::session_log::LogLevel;
    use rgit_model::{NodeKind, Section, Span, Style};

    let rows: Vec<Section> = lines
        .iter()
        .filter(|l| filter.admits(l.level))
        .enumerate()
        .map(|(i, l)| {
            let (tag, level_style) = match l.level {
                LogLevel::Error => ("error", Style::Deleted),
                LogLevel::Warn => ("warn ", Style::Modified),
                LogLevel::Info => ("info ", Style::Dim),
                LogLevel::Debug => ("debug", Style::Dim),
                LogLevel::Trace => ("trace", Style::Dim),
            };
            // Warnings and errors color the message; everything else reads plain.
            let msg_style = match l.level {
                LogLevel::Error => Style::Deleted,
                LogLevel::Warn => Style::Modified,
                _ => Style::Plain,
            };
            let spans = vec![
                Span::new(format!("{}  ", l.time), Style::Dim),
                Span::new(format!("{tag} "), level_style),
                Span::new(format!("{:<8} ", l.target), Style::Hash),
                Span::new(l.message.clone(), msg_style),
            ];
            Section::leaf(format!("log/{i}"), NodeKind::Info, spans)
        })
        .collect();

    if rows.is_empty() {
        return vec![Section::leaf(
            "log/empty",
            NodeKind::Info,
            vec![Span::new(
                "no log lines for this filter".to_owned(),
                Style::Dim,
            )],
        )];
    }
    rows
}

fn build_smartlog(entries: &[SmartlogEntry]) -> Vec<Section> {
    use rgit_model::{NodeKind, Section, Span, Style};
    if entries.is_empty() {
        return vec![Section::leaf(
            "smartlog/empty",
            NodeKind::Info,
            vec![Span::new("no commits".to_owned(), Style::Dim)],
        )];
    }
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let (marker, mstyle) = if e.is_head {
                ("*", Style::Added)
            } else if e.is_trunk {
                ("=", Style::Branch)
            } else {
                ("o", Style::Dim)
            };
            let mut spans = vec![
                Span::new(format!("{marker} "), mstyle),
                Span::new(format!("{} ", e.short_id), Style::Hash),
            ];
            if !e.refs.is_empty() {
                spans.push(Span::new(
                    format!("({}) ", e.refs.join(", ")),
                    Style::Branch,
                ));
            }
            spans.push(Span::new(e.summary.clone(), Style::Plain));
            if let Some(id) = &e.change_id {
                let short: String = id.chars().take(9).collect();
                spans.push(Span::new(format!("  {short}"), Style::Dim));
            }
            spans.push(Span::new(format!("  {}", e.when), Style::Dim));
            Section::leaf(format!("smartlog/{i}"), NodeKind::Info, spans)
        })
        .collect()
}

fn build_oplog(entries: &[OpLogEntry]) -> Vec<Section> {
    use rgit_model::{NodeKind, Section, Span, Style};
    if entries.is_empty() {
        return vec![Section::leaf(
            "oplog/empty",
            NodeKind::Info,
            vec![Span::new("op-log is empty".to_owned(), Style::Dim)],
        )];
    }
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let spans = vec![
                Span::new(format!("{} ", e.short_id), Style::Hash),
                Span::new(format!("{} ", e.label), Style::Plain),
                Span::new(format!("({}, {})", e.head, e.when), Style::Dim),
            ];
            Section::leaf(format!("oplog/{i}"), NodeKind::Info, spans)
        })
        .collect()
}

/// The lanes view: a foldable section per lane listing the files and hunks it
/// owns. Section ids encode the lane and path so the view's keys can act on the
/// item under the cursor (`lanes/lane/<name>`, `lanes/file/<name>/<path>`).
fn build_lanes(state: &LanesState) -> Vec<Section> {
    use rgit_model::{NodeKind, Section, Span, Style};
    state
        .lanes
        .iter()
        .map(|lane| {
            let header = vec![
                Span::new(format!("{} ", lane.name), Style::Branch),
                Span::new(format!("[{}]", lane.branch), Style::Dim),
            ];
            let mut children: Vec<Section> = Vec::new();
            for (short, summary) in &lane.commits {
                children.push(Section::leaf(
                    format!("lanes/commit/{}/{}", lane.name, short),
                    NodeKind::Info,
                    vec![
                        Span::new(format!("{short} "), Style::Hash),
                        Span::new(summary.clone(), Style::Dim),
                    ],
                ));
            }
            for path in &lane.paths {
                children.push(Section::leaf(
                    format!("lanes/file/{}/{}", lane.name, path),
                    NodeKind::Info,
                    vec![Span::new(path.clone(), Style::Plain)],
                ));
            }
            for h in &lane.hunks {
                let short: String = h.anchor.chars().take(7).collect();
                children.push(Section::leaf(
                    format!("lanes/hunk/{}/{}", lane.name, h.path),
                    NodeKind::Info,
                    vec![
                        Span::new(h.path.clone(), Style::Plain),
                        Span::new(format!("  (hunk {short})"), Style::Dim),
                    ],
                ));
            }
            if children.is_empty() {
                children.push(Section::leaf(
                    format!("lanes/empty/{}", lane.name),
                    NodeKind::Info,
                    vec![Span::new("(no files)".to_owned(), Style::Dim)],
                ));
            }
            Section::branch(
                format!("lanes/lane/{}", lane.name),
                NodeKind::Info,
                header,
                children,
            )
        })
        .collect()
}

/// The lane name for the row under the cursor, from its `lanes/.../<name>/...`
/// section id (lane header, file, or hunk row).
fn lane_at_cursor(app: &App) -> Option<String> {
    let id = app.buffer().cursor_id()?;
    lane_of_id(&id).map(str::to_owned)
}

/// The `(lane, path)` for a file row under the cursor, if the cursor is on one.
fn file_at_cursor(app: &App) -> Option<(String, String)> {
    let id = app.buffer().cursor_id()?;
    let (lane, path) = file_of_id(&id)?;
    Some((lane.to_owned(), path.to_owned()))
}

/// The lane name in a `lanes/<kind>/<name>[/...]` section id.
fn lane_of_id(id: &str) -> Option<&str> {
    let mut parts = id.split('/');
    (parts.next()? == "lanes").then_some(())?;
    let _kind = parts.next()?;
    parts.next()
}

/// The `(lane, path)` in a `lanes/file/<name>/<path>` section id.
fn file_of_id(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix("lanes/file/")?.split_once('/')
}

fn build_stack(parents: &[(String, Option<String>)], current: Option<&str>) -> Vec<Section> {
    use rgit_model::{NodeKind, Section, Span, Style};
    use std::collections::HashMap;
    let map: HashMap<&str, &str> = parents
        .iter()
        .filter_map(|(b, p)| p.as_deref().map(|p| (b.as_str(), p)))
        .collect();

    // Walk down from the current branch through its parents.
    let mut chain = Vec::new();
    let mut cursor = current.map(str::to_owned);
    while let Some(branch) = cursor {
        let parent = map.get(branch.as_str()).map(|s| s.to_string());
        chain.push((branch.clone(), parent.clone()));
        cursor = parent;
    }
    if chain.len() <= 1 && map.is_empty() {
        return vec![Section::leaf(
            "stack/empty",
            NodeKind::Info,
            vec![Span::new(
                "no stacked branches (palette: Stack: new branch)".to_owned(),
                Style::Dim,
            )],
        )];
    }
    chain
        .iter()
        .enumerate()
        .map(|(i, (branch, parent))| {
            let is_current = current == Some(branch.as_str());
            let mark = if is_current { "*" } else { " " };
            let mut spans = vec![
                Span::new(format!("{mark} "), Style::Added),
                Span::new(
                    branch.clone(),
                    if is_current {
                        Style::Added
                    } else {
                        Style::Branch
                    },
                ),
            ];
            match parent {
                Some(p) => spans.push(Span::new(format!("  (on {p})"), Style::Dim)),
                None => spans.push(Span::new("  (base)".to_owned(), Style::Dim)),
            }
            Section::leaf(format!("stack/{i}"), NodeKind::Info, spans)
        })
        .collect()
}

fn build_info(title: &str, text: &Result<String, String>) -> Vec<Section> {
    use rgit_model::{NodeKind, Section, Span, Style};
    let (body, style) = match text {
        Ok(t) => (t.clone(), Style::Plain),
        Err(e) => (e.clone(), Style::Deleted),
    };
    let mut rows = vec![Section::leaf(
        "info/title",
        NodeKind::Info,
        vec![Span::new(title.to_owned(), Style::SectionHeader)],
    )];
    for (i, line) in body.lines().enumerate() {
        rows.push(Section::leaf(
            format!("info/{i}"),
            NodeKind::Info,
            vec![Span::new(line.to_owned(), style)],
        ));
    }
    rows
}

/// Open the operation console for a network op and start it streaming.
fn open_op(app: &mut App, op: ConsoleOp) -> Vec<Effect> {
    app.error = None;
    app.hook_console = Some(HookConsole::net(op.clone()));
    vec![Effect::OpConsole(op)]
}

/// Jump the cursor to the next match of the live search input from `from`.
fn search_jump(app: &mut App, from: usize, forward: bool) {
    let Some(query) = app.search.clone() else {
        return;
    };
    if let Some(idx) = app.buffer().search(&query, from, forward) {
        app.buffer_mut().set_cursor(idx);
    }
}

/// Jump to the next/previous match of the last confirmed query (`n` / `N`).
fn repeat_search(app: &mut App, from: usize, forward: bool) {
    let Some(query) = app.last_search.clone() else {
        return;
    };
    if let Some(idx) = app.buffer().search(&query, from, forward) {
        app.buffer_mut().set_cursor(idx);
    }
}

fn bisect(app: &mut App, sub: &str) -> Effect {
    app.busy = Some("bisecting".into());
    Effect::Mutate(Mutation::Bisect(vec![sub.into()]))
}

fn resolve_conflict_at(app: &mut App, ours: bool) -> Vec<Effect> {
    if let Some(Target::File { path, .. }) = app.buffer().target_at_cursor() {
        app.loading = true;
        return vec![Effect::Mutate(Mutation::ResolveConflict { path, ours })];
    }
    Vec::new()
}

fn refreshed(app: &mut App, result: RefreshResult) -> Vec<Effect> {
    app.loading = false;
    // A labeled operation that just finished reports its outcome as a toast.
    let finished = app.busy.take();
    match result {
        Ok(status) => {
            app.state = status.state;
            app.changed = status.entries.len();
            app.snapshot = Some(status.clone());
            app.head = Some(status.head);
            rebuild_status(app);
            app.error = None;
            // The working tree changed, so cached file diffs are stale; commit
            // and stash diffs are immutable and stay cached. Force the current
            // preview to rebuild from the new snapshot.
            app.preview_cache
                .retain(|k, _| !matches!(k, PreviewKey::File { .. }));
            if matches!(app.preview_key, Some(PreviewKey::File { .. })) {
                app.preview_key = None;
            }
            if let Some(label) = finished {
                app.push_toast(ToastKind::Success, format!("{label} done"));
            }
            let mut effects = app.sync_preview();
            // Refresh the forge PR/CI status, throttled so frequent worktree
            // refreshes do not spam the network.
            let stale = app
                .last_remote_fetch
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(20));
            if stale {
                app.last_remote_fetch = Some(std::time::Instant::now());
                effects.push(Effect::LoadRemoteStatus);
            }
            return effects;
        }
        Err(e) => app.push_toast(ToastKind::Error, e.to_string()),
    }
    Vec::new()
}

/// Rebuild the status buffer from the last snapshot plus the REMOTE section,
/// preserving fold state and cursor.
fn rebuild_status(app: &mut App) {
    let Some(status) = app.snapshot.clone() else {
        return;
    };
    let mut sections = build(&status);
    if let Some(section) = build_remote_section(app.remote_status.as_ref(), app.head.as_ref()) {
        sections.push(section);
    }
    app.status_buffer_mut().set_content(sections);
}

/// The REMOTE section: per-remote ahead/behind for the current branch, plus its
/// open PR and CI rollup once loaded. `None` when there is nothing to show (no
/// remotes and no loaded forge status).
fn build_remote_section(remote: Option<&RemoteSummary>, head: Option<&Head>) -> Option<Section> {
    use rgit_model::{NodeKind, Section, Span, Style};
    let mut children: Vec<Section> = Vec::new();

    // One line per remote that has this branch: `origin/main  up 2 down 0`.
    for (r, (name, ahead, behind)) in head.map(|h| &h.remotes).into_iter().flatten().enumerate() {
        let mut spans = vec![Span::new(name.clone(), Style::Branch)];
        if *ahead > 0 {
            spans.push(Span::new(format!("  \u{2191}{ahead}"), Style::Added));
        }
        if *behind > 0 {
            spans.push(Span::new(format!("  \u{2193}{behind}"), Style::Deleted));
        }
        if *ahead == 0 && *behind == 0 {
            spans.push(Span::new("  up to date".to_owned(), Style::Dim));
        }
        children.push(Section::leaf(format!("remote/{r}"), NodeKind::Info, spans));
    }

    // The current branch's PR and CI, once loaded.
    if let Some(remote) = remote {
        let mut spans = Vec::new();
        match &remote.pr {
            Some((num, state)) => {
                spans.push(Span::new(format!("PR #{num} "), Style::Hash));
                let style = if state.eq_ignore_ascii_case("open")
                    || state.eq_ignore_ascii_case("opened")
                {
                    Style::Added
                } else {
                    Style::Dim
                };
                spans.push(Span::new(state.clone(), style));
            }
            None => spans.push(Span::new("no open pull request".to_owned(), Style::Dim)),
        }
        if let Some(checks) = &remote.checks {
            let (sym, style) = match checks.as_str() {
                "passing" => ("\u{2714}", Style::Added),
                "failing" => ("\u{2718}", Style::Deleted),
                _ => ("\u{2022}", Style::Dim),
            };
            spans.push(Span::new(format!("   {sym} {checks}"), style));
        }
        children.push(Section::leaf("remote/pr", NodeKind::Info, spans));
    }

    if children.is_empty() {
        return None;
    }
    Some(Section::branch(
        "remote",
        NodeKind::Section,
        vec![Span::new("REMOTE".to_owned(), Style::Dim)],
        children,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_menu_exposes_the_parity_ops_with_unique_keys() {
        let t = Transient::operations();
        let kinds: Vec<ActionKind> = t.actions.iter().map(|a| a.kind).collect();
        for k in [
            ActionKind::OpSync,
            ActionKind::OpSubmit,
            ActionKind::OpStackNext,
            ActionKind::OpStackPrev,
            ActionKind::OpPruneMerged,
            ActionKind::OpClean,
        ] {
            assert!(kinds.contains(&k), "operations menu missing {k:?}");
        }
        // Every action key in the menu is distinct, or one shadows another.
        let mut keys: Vec<char> = t.actions.iter().map(|a| a.key).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "duplicate action keys in the operations menu");
    }

    #[test]
    fn remote_section_appears_only_when_loaded() {
        assert!(build_remote_section(None, None).is_none());
        let summary = RemoteSummary {
            pr: Some((7, "open".into())),
            checks: Some("passing".into()),
        };
        let mut buffer = Buffer::default();
        buffer.set_content(vec![build_remote_section(Some(&summary), None).unwrap()]);
        let ids: Vec<String> = buffer.rows().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&"remote".to_owned()));
        assert!(ids.contains(&"remote/pr".to_owned()));
    }

    #[test]
    fn build_lanes_encodes_lane_and_path_in_section_ids() {
        use rgit_git::{HunkRef, Lane};
        let state = LanesState {
            base: "abc".into(),
            lanes: vec![
                Lane {
                    name: "default".into(),
                    branch: "main".into(),
                    paths: vec!["src/a.rs".into()],
                    hunks: vec![],
                    commits: vec![],
                    parent: None,
                },
                Lane {
                    name: "feat".into(),
                    branch: "feat".into(),
                    paths: vec![],
                    hunks: vec![HunkRef {
                        path: "b.rs".into(),
                        anchor: "deadbeef".into(),
                    }],
                    commits: vec![("abc1234".into(), "did a thing".into())],
                    parent: None,
                },
            ],
        };
        let mut buffer = Buffer::default();
        buffer.set_content(build_lanes(&state));
        let ids: Vec<String> = buffer.rows().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&"lanes/lane/default".to_owned()));
        assert!(ids.contains(&"lanes/file/default/src/a.rs".to_owned()));
        assert!(ids.contains(&"lanes/lane/feat".to_owned()));
        assert!(ids.contains(&"lanes/hunk/feat/b.rs".to_owned()));
    }

    #[test]
    fn lane_and_file_ids_parse_back() {
        assert_eq!(lane_of_id("lanes/lane/default"), Some("default"));
        assert_eq!(lane_of_id("lanes/file/feat/src/a.rs"), Some("feat"));
        assert_eq!(lane_of_id("lanes/hunk/feat/b.rs"), Some("feat"));
        assert_eq!(lane_of_id("status/0"), None);
        assert_eq!(
            file_of_id("lanes/file/feat/src/deep/a.rs"),
            Some(("feat", "src/deep/a.rs"))
        );
        assert_eq!(file_of_id("lanes/lane/feat"), None);
    }

    #[test]
    fn palette_fuzzy_matches_as_subsequence() {
        let all = Palette::matches("");
        assert_eq!(all.len(), PALETTE.len());

        let rb = Palette::matches("rb");
        assert!(rb.iter().any(|e| e.label == "Rebase"));
        assert!(rb.iter().all(|e| e.label != "Commit"));

        // case-insensitive, and the empty result is fine
        assert!(Palette::matches("PUSH").iter().any(|e| e.label == "Push"));
        assert!(Palette::matches("zzzq").is_empty());
    }

    #[test]
    fn commit_editor_edits_text() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut ed = CommitEditor::new(false, "");
        let ch = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        for c in "feat: x".chars() {
            ed.input(ch(c));
        }
        assert_eq!(ed.subject_len(), 7);
        ed.input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ed.input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for c in "body".chars() {
            ed.input(ch(c));
        }
        assert_eq!(ed.message(), "feat: x\n\nbody");
        // backspace at line start merges into the previous line
        ed.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        ed.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        ed.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        ed.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        ed.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ed.message(), "feat: x");
    }

    #[test]
    fn commit_editor_readline_motions() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut ed = CommitEditor::new(false, "");
        let ch = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        for c in "hello world".chars() {
            ed.input(ch(c));
        }
        ed.input(ctrl('a')); // start of line
        ed.input(ch('Z'));
        assert_eq!(ed.message(), "Zhello world");
        ed.input(ctrl('e')); // end of line
        ed.input(ctrl('w')); // kill previous word
        assert_eq!(ed.message(), "Zhello");
        ed.input(ctrl('u')); // kill to start
        assert_eq!(ed.message(), "");
    }

    #[test]
    fn prompt_readline_edits_mid_string() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut p = Prompt {
            label: "x".into(),
            input: String::new(),
            cursor: 0,
            candidates: Vec::new(),
            selected: 0,
            action: PromptAction::CreateBranch,
            masked: false,
        };
        let ch = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        for c in "featureX".chars() {
            p.apply(ch(c));
        }
        p.apply(ctrl('a')); // to start
        p.apply(ctrl('f')); // one right
        p.apply(ch('-')); // insert after first char
        assert_eq!(p.input, "f-eatureX");
        p.apply(ctrl('e')); // to end
        p.apply(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(p.input, "f-eature");
        p.apply(ctrl('u')); // kill to start
        assert_eq!(p.input, "");
    }

    #[test]
    fn hook_console_flow() {
        let mut app = App::new(
            Arc::new(rgit_git::Git2Backend::discover(".").unwrap()),
            crate::config::Config::default(),
        );

        // Submitting the editor opens the hook console and asks to run the commit.
        app.commit_editor = Some(CommitEditor::new(false, "my message"));
        let fx = update(&mut app, Msg::CommitEditorSubmit);
        assert!(app.commit_editor.is_none());
        assert!(app.hook_console.is_some());
        assert!(matches!(fx.as_slice(), [Effect::CommitConsole { .. }]));

        // Streamed lines accumulate in the console.
        update(&mut app, Msg::HookOutput("running black... failed".into()));
        assert_eq!(app.hook_console.as_ref().unwrap().lines.len(), 1);

        // A hook rejection holds the console open with a failed status.
        update(
            &mut app,
            Msg::HookFinished {
                ok: false,
                summary: None,
            },
        );
        assert_eq!(
            app.hook_console.as_ref().unwrap().status,
            HookStatus::Failed
        );

        // Retry reopens the editor with the message preserved for a fix.
        update(&mut app, Msg::HookConsoleRetry);
        assert!(app.hook_console.is_none());
        assert_eq!(app.commit_editor.as_ref().unwrap().message(), "my message");

        // A passing run closes the console and refreshes.
        app.hook_console = Some(HookConsole::commit(false, "m".into()));
        let fx = update(
            &mut app,
            Msg::HookFinished {
                ok: true,
                summary: Some("[main abc] x".into()),
            },
        );
        assert!(app.hook_console.is_none());
        assert!(matches!(fx.as_slice(), [Effect::Refresh]));
    }

    #[test]
    fn toasts_age_out_on_tick() {
        // The tests run inside a git repo, so a real backend discovers cleanly.
        let mut app = App::new(
            Arc::new(rgit_git::Git2Backend::discover(".").unwrap()),
            crate::config::Config::default(),
        );
        app.push_toast(ToastKind::Success, "pushed".into());
        assert_eq!(app.toasts.len(), 1);
        let start = app.toasts[0].ticks;
        for _ in 0..start {
            app.tick();
        }
        assert!(app.toasts.is_empty(), "toast expires after its ticks");
    }
}
