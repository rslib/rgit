//! The command-line surface. With no subcommand rgit launches the TUI; each
//! subcommand drives the same `GitBackend` and prints compact, agent-friendly output.
//!
//! When a required argument is missing and stdout is a real terminal, the
//! missing value is prompted for (our own widgets); with `--no-input` or a non-TTY
//! (an agent, a pipe, CI) it errors instead, so scripted use stays predictable.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand, ValueEnum};
use rgit_git::{GitBackend, GitError, LogOptions, OpProgress, ResetMode};

use crate::render;

#[derive(Parser)]
#[command(
    name = "rgit",
    version,
    about = "A magit-style git TUI, also usable as a CLI and an MCP server.",
    long_about = "Run with no subcommand to open the TUI. Subcommands drive the same in-process \
                  git backend and print compact, agent-friendly output."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Never prompt; error if a required argument is missing (for scripts/agents).
    #[arg(long, global = true)]
    pub no_input: bool,

    /// Print a JSON result envelope.
    #[arg(long, global = true, conflicts_with_all = ["toon", "human"])]
    pub json: bool,

    /// Print TOON for agents: no color, spinners, or prompts. The default when
    /// stdout is not a terminal.
    #[arg(long, visible_alias = "axi", global = true, conflicts_with = "human")]
    pub toon: bool,

    /// Force human text output, even when stdout is not a terminal.
    #[arg(long, alias = "text", global = true)]
    pub human: bool,

    /// Disable ANSI color even on a terminal.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Extra table columns to print in agent output (comma-separated).
    #[arg(long, global = true, value_delimiter = ',', value_name = "FIELD,...")]
    pub fields: Vec<String>,

    /// Print long text (patches, commit bodies) without truncation.
    #[arg(long, global = true)]
    pub full: bool,

    /// Open the TUI without the side preview pane (single column). Handy when
    /// embedding rgit in a narrow editor split. Overrides `ui.preview`.
    #[arg(long)]
    pub no_preview: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Text,
    Porcelain,
    Json,
}

impl Cli {
    pub fn output_mode(&self, stdout_is_terminal: bool) -> OutputMode {
        if self.json {
            OutputMode::Json
        } else if self.toon || !stdout_is_terminal && !self.human {
            OutputMode::Porcelain
        } else {
            OutputMode::Text
        }
    }
}

/// How `log`, `diff` and `show` print the changes.
#[derive(clap::Args, Clone, Copy, Default)]
pub struct DiffFormat {
    /// Print the full unified patch (git's -p).
    #[arg(short, long)]
    pub patch: bool,
    /// Print a diffstat.
    #[arg(long)]
    pub stat: bool,
    /// List only the names of changed files.
    #[arg(long)]
    pub name_only: bool,
    /// List changed files with a status letter (A, M, D, R...).
    #[arg(long)]
    pub name_status: bool,
    /// Added and removed line counts per file, tab-separated.
    #[arg(long)]
    pub numstat: bool,
}

impl DiffFormat {
    pub(crate) fn any(self) -> bool {
        self.patch || self.stat || self.name_only || self.name_status || self.numstat
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Compact working-tree status. Agents: add `--toon` for a structured table.
    /// `--porcelain`, `--short`, `--branch`, and `-z` print git's raw formats,
    /// exactly as `git status` does, for scripts.
    Status {
        /// git's raw script format (`git status --porcelain`), v1 by default.
        /// Agents should prefer `--toon`.
        #[arg(
            long,
            value_name = "VERSION",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "v1"
        )]
        porcelain: Option<String>,
        /// git's short format (`git status --short`).
        #[arg(short, long)]
        short: bool,
        /// Add branch and tracking info (`git status --branch`).
        #[arg(short, long)]
        branch: bool,
        /// Terminate entries with NUL (`git status -z`).
        #[arg(short = 'z')]
        z: bool,
        /// Show untracked files: `no`, `normal` or `all` (git's -u, default all).
        #[arg(
            short = 'u',
            long = "untracked-files",
            value_name = "MODE",
            num_args = 0..=1,
            value_parser = ["no", "normal", "all"],
            default_missing_value = "all"
        )]
        untracked: Option<String>,
        /// Also list ignored files (git's --ignored).
        #[arg(long)]
        ignored: bool,
        /// Limit to these paths: files, folders or globs.
        paths: Vec<String>,
    },
    /// Recent commits as `sha subject` lines.
    Log {
        /// Maximum number of commits to show (git's -n).
        #[arg(
            short = 'n',
            short_alias = 'l',
            long = "max-count",
            visible_alias = "limit",
            default_value_t = 20
        )]
        limit: usize,
        /// Skip this many commits before showing any.
        #[arg(long, value_name = "N", default_value_t = 0)]
        skip: usize,
        /// Walk every ref, not just HEAD.
        #[arg(long)]
        all: bool,
        /// Keep only commits whose author name/email contains this.
        #[arg(long)]
        author: Option<String>,
        /// Only commits at or after this date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS).
        #[arg(long, visible_alias = "after")]
        since: Option<String>,
        /// Only commits at or before this date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS).
        #[arg(long, visible_alias = "before")]
        until: Option<String>,
        /// Accepted for git compatibility (rgit's log is already one line each).
        #[arg(long)]
        oneline: bool,
        /// Keep only commits whose message matches this regex (repeat for any of several).
        #[arg(long, value_name = "REGEX")]
        grep: Vec<String>,
        /// Match --grep case-insensitively.
        #[arg(short = 'i', long = "regexp-ignore-case")]
        ignore_case: bool,
        /// Follow only the first parent of merge commits.
        #[arg(long)]
        first_parent: bool,
        /// Show only merge commits.
        #[arg(long, conflicts_with = "no_merges")]
        merges: bool,
        /// Leave out merge commits.
        #[arg(long)]
        no_merges: bool,
        /// Oldest first.
        #[arg(long)]
        reverse: bool,
        /// Follow one file's history across renames.
        #[arg(long)]
        follow: bool,
        #[command(flatten)]
        format: DiffFormat,
        /// Revisions to walk (`main`, `^main`, `A..B`, `A...B`; default HEAD),
        /// then paths: `rgit log <rev>...` or `rgit log <rev> -- <path>...`.
        #[arg(value_name = "REV_OR_PATH")]
        revs: Vec<String>,
        /// Limit to commits touching these paths (after `--`).
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Diffstat of unstaged changes (`--cached` for staged), against a revision,
    /// or between two revisions (`A B`, `A..B`, `A...B`).
    Diff {
        /// Revisions (none: the working tree against the index; one: it against
        /// the working tree; `A B`, `A..B`, or `A...B` from their merge base),
        /// then paths.
        #[arg(value_name = "REV_OR_PATH")]
        revs: Vec<String>,
        /// Limit to these paths (after `--`).
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
        /// Diff the staged changes (index vs HEAD), like git's --cached.
        #[arg(long, visible_alias = "staged")]
        cached: bool,
        #[command(flatten)]
        format: DiffFormat,
        /// Lines of context around each change (git's -U, default 3).
        #[arg(short = 'U', long = "unified", value_name = "N")]
        unified: Option<u32>,
        /// Ignore whitespace when comparing lines.
        #[arg(short = 'w', long = "ignore-all-space")]
        ignore_all_space: bool,
        /// Ignore changes in the amount of whitespace.
        #[arg(short = 'b', long = "ignore-space-change")]
        ignore_space_change: bool,
    },
    /// A commit's header and diffstat, or a file (`rev:path`) or folder at a revision.
    Show {
        /// The commits to show (branch, tag, or sha; default HEAD), or `rev:path`
        /// to print a file as it is at `rev` (`:path` for the staged version).
        #[arg(value_name = "REV")]
        revs: Vec<String>,
        /// Limit the listed changes to these paths (after `--`).
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
        #[command(flatten)]
        format: DiffFormat,
        /// Only the header, no changed files (git's -s).
        #[arg(short = 's', long = "no-patch")]
        no_patch: bool,
    },
    /// Blame a file: `sha author line` per line.
    Blame {
        /// `[REV] PATH`: the file to annotate, as it is in the working tree or
        /// at REV (`rgit blame <rev> -- <path>` also works).
        #[arg(value_name = "REV_OR_PATH", required = true, num_args = 1..=2)]
        args: Vec<String>,
        /// Limit to a 1-based line range `START,END` or `START,+COUNT` (git's -L).
        #[arg(short = 'L', value_name = "START,END")]
        lines: Option<String>,
    },
    /// All refs (local branches, remotes, tags).
    Refs,
    /// Stage paths, some hunks of one path, or specific lines of one hunk.
    Stage {
        /// The paths to stage: files, folders or globs.
        #[arg(required = true)]
        paths: Vec<String>,
        /// Stage only the hunks at these new-side start lines (comma-separated).
        #[arg(long, value_delimiter = ',')]
        hunk: Vec<u32>,
        /// Stage only these line indices within one --hunk (comma-separated).
        #[arg(long, value_delimiter = ',', requires = "hunk")]
        lines: Vec<usize>,
    },
    /// Unstage paths, some hunks of one path, or specific lines of one hunk.
    Unstage {
        /// The paths to unstage: files, folders or globs.
        #[arg(required = true)]
        paths: Vec<String>,
        /// Unstage only the hunks at these new-side start lines (comma-separated).
        #[arg(long, value_delimiter = ',')]
        hunk: Vec<u32>,
        /// Unstage only these line indices within one --hunk (comma-separated).
        #[arg(long, value_delimiter = ',', requires = "hunk")]
        lines: Vec<usize>,
    },
    /// Stage every change.
    StageAll,
    /// Unstage everything.
    UnstageAll,
    /// Discard unstaged changes to paths, some hunks of one path, or specific lines.
    Discard {
        /// The paths to discard: files, folders or globs (prompted for if
        /// omitted on a terminal).
        paths: Vec<String>,
        /// Discard only the hunks at these new-side start lines (comma-separated).
        #[arg(long, value_delimiter = ',')]
        hunk: Vec<u32>,
        /// Discard only these line indices within one --hunk (comma-separated).
        #[arg(long, value_delimiter = ',', requires = "hunk")]
        lines: Vec<usize>,
    },
    /// Resolve a conflicted path by taking ours or theirs.
    Resolve {
        /// The conflicted path.
        path: String,
        /// Take our side of the conflict.
        #[arg(long, conflicts_with = "theirs")]
        ours: bool,
        /// Take their side of the conflict.
        #[arg(long)]
        theirs: bool,
    },
    /// Commit the staged changes (runs hooks).
    Commit {
        /// The commit message (prompted for if omitted on a terminal).
        #[arg(short, long)]
        message: Option<String>,
        /// Amend the previous commit instead of creating a new one.
        #[arg(long)]
        amend: bool,
        /// Stage all tracked, modified files before committing (git's -a).
        #[arg(short = 'a', long = "all")]
        all: bool,
        /// Skip the pre-commit and commit-msg hooks (git's --no-verify).
        #[arg(short = 'n', long = "no-verify")]
        no_verify: bool,
    },
    /// Amend HEAD with the staged changes, keeping its message (no editor).
    Extend,
    /// Change any commit's message and restack its descendants (default HEAD).
    Reword {
        /// The new message.
        #[arg(short, long)]
        message: String,
        /// The commit to reword (default HEAD).
        #[arg(default_value = "HEAD")]
        rev: String,
    },
    /// Undo the last commit(s), keeping the changes staged (default 1).
    Uncommit {
        /// How many commits to undo.
        #[arg(default_value_t = 1)]
        n: usize,
    },
    /// Fold a commit into its parent (default HEAD), or a whole range with
    /// `--from` (fold every commit after <rev> up to HEAD into one).
    Squash {
        /// The commit to squash into its parent (default HEAD).
        #[arg(default_value = "HEAD")]
        rev: String,
        /// Fold every commit after this one, up to HEAD, into a single commit.
        #[arg(long)]
        from: Option<String>,
    },
    /// Move a commit before or after another in the current branch's history.
    #[command(name = "move")]
    Move {
        /// The commit to move.
        rev: String,
        /// Move it to just before this commit.
        #[arg(long, conflicts_with = "after")]
        before: Option<String>,
        /// Move it to just after this commit.
        #[arg(long)]
        after: Option<String>,
    },
    /// Split a commit into two by path (given paths first, the rest second).
    Split {
        /// The commit to split (default HEAD).
        #[arg(long, default_value = "HEAD")]
        rev: String,
        /// Paths whose changes go into the first commit.
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Prune unreachable objects (git's `prune`). For deleting merged branches,
    /// use `branch prune`.
    Prune {
        /// List what would be removed without deleting (git's -n).
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
    },
    /// Check out the branch stacked on this one (move up the stack).
    Next,
    /// Check out this branch's stack parent (move down the stack).
    Prev,
    /// Fetch the current branch's remote, or `<repository> [<refspec>...]`.
    Fetch {
        /// The remote to fetch (defaults to the branch's upstream remote).
        repository: Option<String>,
        /// Refs to fetch, as in git (`main`, `src:dst`); defaults to the remote's.
        refspecs: Vec<String>,
        /// Fetch from every remote (git's --all).
        #[arg(long)]
        all: bool,
        /// Delete remote-tracking refs that no longer exist upstream (--prune).
        #[arg(short = 'p', long)]
        prune: bool,
        /// Fetch this named remote instead of the branch's upstream.
        #[arg(long)]
        remote: Option<String>,
        /// Fetch every tag too (git's --tags).
        #[arg(short = 't', long)]
        tags: bool,
        /// Limit history to this many commits (git's --depth).
        #[arg(long, default_value_t = 0)]
        depth: i32,
        /// Show what would be fetched without changing any ref.
        #[arg(long)]
        dry_run: bool,
    },
    /// Fetch and integrate the current branch's upstream (merges when it has
    /// diverged, unless `pull.rebase` says otherwise).
    Pull {
        /// The remote to pull from (defaults to the upstream's remote).
        repository: Option<String>,
        /// The remote branch to integrate (defaults to the upstream branch).
        branch: Option<String>,
        /// Rebase local commits onto the upstream instead of merging.
        #[arg(short = 'r', long)]
        rebase: bool,
        /// Merge even if `pull.rebase` is set (git's --no-rebase).
        #[arg(long, conflicts_with = "rebase")]
        no_rebase: bool,
        /// Refuse unless the upstream fast-forwards the branch (git's --ff-only).
        #[arg(long)]
        ff_only: bool,
    },
    /// Fetch, fast-forward branches to their upstreams, and restack the stack.
    Sync,
    /// Push every branch in the stack and open a pull request per branch.
    Submit,
    /// Push the current branch to its upstream, or `<repository> [<refspec>...]`.
    Push {
        /// The remote to push to (defaults to the branch's upstream remote).
        repository: Option<String>,
        /// Refs to push, as in git: `branch`, `src:dst`, `:branch` (delete),
        /// `+src:dst` (force).
        refspecs: Vec<String>,
        /// Overwrite the remote branch unconditionally (dangerous).
        #[arg(short = 'f', long, conflicts_with = "force_with_lease")]
        force: bool,
        /// Overwrite the remote branch only if it still matches our tracking ref.
        #[arg(long)]
        force_with_lease: bool,
        /// Record the pushed branch as the upstream.
        #[arg(short = 'u', long)]
        set_upstream: bool,
        /// Push to this named remote instead of the branch's upstream.
        #[arg(long)]
        remote: Option<String>,
        /// Push all local tags (git's --tags).
        #[arg(long)]
        tags: bool,
        /// Push every local branch (git's --all).
        #[arg(long)]
        all: bool,
        /// Delete the named branches on the remote (git's --delete).
        #[arg(short = 'd', long)]
        delete: bool,
        /// Show what would be pushed without sending anything.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Check out a branch or, for any other revision, a detached HEAD.
    Checkout {
        /// The branch or revision (prompted for if omitted on a terminal).
        rev: Option<String>,
        /// Create a new branch and switch to it (git's -b), from `rev` or HEAD.
        #[arg(short = 'b', value_name = "NEW_BRANCH")]
        branch: Option<String>,
    },
    /// Merge revisions into the current branch (several make an octopus merge).
    Merge {
        /// The branches or revisions to merge (prompted for if omitted).
        revs: Vec<String>,
        /// Always create a merge commit, even if a fast-forward is possible.
        #[arg(long = "no-ff", conflicts_with = "ff_only")]
        no_ff: bool,
        /// Refuse to merge unless it can fast-forward (git's --ff-only).
        #[arg(long = "ff-only")]
        ff_only: bool,
        /// Stage the merged changes as one ordinary change, without a merge commit.
        #[arg(long)]
        squash: bool,
        /// Merge but stop before committing; finish with `merge --continue`.
        #[arg(long = "no-commit")]
        no_commit: bool,
        /// The merge commit message.
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
        /// Take this side on conflicting hunks (git's -X).
        #[arg(short = 'X', long = "strategy-option", value_parser = ["ours", "theirs"])]
        strategy_option: Option<String>,
        /// Accepted for git compatibility; rgit never opens an editor.
        #[arg(long = "no-edit", hide = true)]
        no_edit: bool,
        /// Commit a merge whose conflicts are resolved.
        #[arg(long = "continue")]
        cont: bool,
        /// Abort an in-progress (conflicted) merge, restoring HEAD.
        #[arg(long)]
        abort: bool,
    },
    /// Rebase onto a revision, or continue/skip/abort an in-progress rebase.
    Rebase {
        /// The upstream to rebase onto (prompted for if omitted). With --onto,
        /// this is the upstream whose commits after it are replayed.
        onto: Option<String>,
        /// Replay commits after <upstream> onto this new base (git's --onto).
        #[arg(long = "onto", value_name = "NEWBASE")]
        onto_new: Option<String>,
        /// Interactive rebase: opens the todo editor (needs a terminal).
        #[arg(short = 'i', long = "interactive")]
        edit: bool,
        /// Rebase every commit down to the root commit.
        #[arg(long)]
        root: bool,
        /// Move `fixup!`/`squash!` commits after their targets and fold them in.
        #[arg(long)]
        autosquash: bool,
        /// Run this shell command after each rebased commit (repeatable).
        #[arg(short = 'x', long = "exec", value_name = "CMD")]
        exec: Vec<String>,
        /// Also move branches that point into the rebased commits.
        #[arg(long = "update-refs")]
        update_refs: bool,
        /// Take this side on conflicting hunks (git's -X).
        #[arg(short = 'X', long = "strategy-option", value_parser = ["ours", "theirs"])]
        strategy_option: Option<String>,
        /// Continue after resolving conflicts.
        #[arg(long = "continue")]
        cont: bool,
        /// Skip the current commit.
        #[arg(long)]
        skip: bool,
        /// Abort the in-progress rebase.
        #[arg(long)]
        abort: bool,
    },
    /// Undo the last operation from the op-log, restoring HEAD and the working
    /// tree (recovers uncommitted work). Set RGIT_OPLOG=0 to disable the op-log.
    Undo,
    /// Redo the operation most recently undone.
    Redo,
    /// Show the operation log (the undo stack), newest first.
    Oplog,
    /// Smartlog: your local/draft commits and the trunk they branch from.
    #[command(visible_alias = "sl")]
    Smartlog,
    /// Run a git bisect subcommand: `start <bad> <good>`, `good`, `bad`, `reset`.
    Bisect {
        /// Arguments passed to `git bisect`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Reset HEAD to a revision (default mixed).
    Reset {
        /// The revision to reset to (prompted for if omitted on a terminal).
        rev: Option<String>,
        /// Move HEAD only, keep the index and working tree.
        #[arg(long, conflicts_with = "hard")]
        soft: bool,
        /// Reset the index and working tree too (discards changes).
        #[arg(long)]
        hard: bool,
        /// Unstage these paths instead of moving HEAD (git's `reset -- <paths>`).
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Cherry-pick commits onto HEAD, or continue/skip/abort a stopped one.
    CherryPick {
        /// Commits or ranges `A..B` to apply in order (prompted for if omitted on a terminal).
        revs: Vec<String>,
        /// Apply the change without committing (git's -n/--no-commit).
        #[arg(short = 'n', long = "no-commit")]
        no_commit: bool,
        /// Append "(cherry picked from commit ...)" to each message (git's -x).
        #[arg(short = 'x')]
        record_origin: bool,
        /// For a merge commit, the parent number (from 1) to diff against.
        #[arg(short = 'm', long = "mainline", value_name = "PARENT")]
        mainline: Option<u32>,
        /// Take this side on conflicting hunks (git's -X).
        #[arg(short = 'X', long = "strategy-option", value_parser = ["ours", "theirs"])]
        strategy_option: Option<String>,
        /// Accepted for git compatibility; rgit keeps the original message.
        #[arg(long = "no-edit", hide = true)]
        no_edit: bool,
        /// Commit the resolved commit and apply the rest.
        #[arg(long = "continue")]
        cont: bool,
        /// Drop the current commit and apply the rest.
        #[arg(long)]
        skip: bool,
        /// Cancel and return to where the cherry-pick started.
        #[arg(long)]
        abort: bool,
    },
    /// Revert commits on HEAD, or continue/skip/abort a stopped revert.
    Revert {
        /// Commits or ranges `A..B` to revert, newest first (prompted for if omitted on a terminal).
        revs: Vec<String>,
        /// Apply the inverse without committing (git's -n/--no-commit).
        #[arg(short = 'n', long = "no-commit")]
        no_commit: bool,
        /// For a merge commit, the parent number (from 1) to revert to.
        #[arg(short = 'm', long = "mainline", value_name = "PARENT")]
        mainline: Option<u32>,
        /// Take this side on conflicting hunks (git's -X).
        #[arg(short = 'X', long = "strategy-option", value_parser = ["ours", "theirs"])]
        strategy_option: Option<String>,
        /// Accepted for git compatibility; rgit writes git's revert message.
        #[arg(long = "no-edit", hide = true)]
        no_edit: bool,
        /// Commit the resolved revert and apply the rest.
        #[arg(long = "continue")]
        cont: bool,
        /// Drop the current commit and apply the rest.
        #[arg(long)]
        skip: bool,
        /// Cancel and return to where the revert started.
        #[arg(long)]
        abort: bool,
    },
    /// Branch management (no subcommand lists local branches).
    Branch {
        #[command(subcommand)]
        cmd: Option<BranchCmd>,
        /// List remote-tracking branches too (git's -a).
        #[arg(short = 'a', long)]
        all: bool,
        /// List only remote-tracking branches (git's -r).
        #[arg(short = 'r', long)]
        remotes: bool,
    },
    /// Stash management (no subcommand stashes the working tree).
    Stash {
        #[command(subcommand)]
        cmd: Option<StashCmd>,
    },
    /// Tag management: `tag` lists, `tag <name>` creates, `tag -d <name>` deletes.
    Tag {
        /// The tag to create (annotated when -m/-a is given).
        name: Option<String>,
        /// Annotation message (implies an annotated tag).
        #[arg(short, long)]
        message: Option<String>,
        /// Make an annotated tag even without a message.
        #[arg(short, long)]
        annotate: bool,
        /// Replace an existing tag of the same name (git's -f).
        #[arg(short, long)]
        force: bool,
        /// Delete this tag (git's -d).
        #[arg(short = 'd', long, value_name = "NAME")]
        delete: Option<String>,
        /// List tags (the default with no name); accepted for git compatibility.
        #[arg(short, long)]
        list: bool,
    },
    /// Remote management (no subcommand lists remotes).
    Remote {
        #[command(subcommand)]
        cmd: Option<RemoteCmd>,
    },
    /// Worktree management (no subcommand lists worktrees).
    Worktree {
        #[command(subcommand)]
        cmd: Option<WorktreeCmd>,
    },
    /// Copy-on-write workspaces: instant, isolated, block-sharing clones of the
    /// whole repo (code, build, .git) for parallel work (no subcommand lists).
    Workspace {
        #[command(subcommand)]
        cmd: Option<WorkspaceCmd>,
    },
    /// Branching workflows: pick a preset (gitflow, github, gitlab, trunk,
    /// release-flow); start/finish/release then follow its rules.
    Flow {
        #[command(subcommand)]
        cmd: FlowCmd,
    },
    /// Manage GitHub repositories, branches, and pull requests without gh.
    Forge {
        /// Named forge profile from the rgit config file.
        #[arg(long)]
        profile: Option<String>,
        /// Credential account label for forge API operations.
        #[arg(long)]
        account: Option<String>,
        /// Forge host override for GitLab API operations.
        #[arg(long)]
        host: Option<String>,
        #[command(subcommand)]
        cmd: ForgeCmd,
    },
    /// Stacked branches: chain branches and restack descendants after edits
    /// (no subcommand lists the current stack).
    Stack {
        #[command(subcommand)]
        cmd: Option<StackCmd>,
    },
    /// Lanes: several lines of work in one worktree. Assign uncommitted files to
    /// lanes and commit each to its own branch (no subcommand lists the lanes).
    Lanes {
        #[command(subcommand)]
        cmd: Option<LanesCmd>,
    },
    /// Fold each pending change into the stacked commit that last touched those
    /// lines (blame-routed fixups + autosquash).
    Absorb,
    /// Get, set, unset or list config values: `config <key>` reads,
    /// `config <key> <value>` writes.
    Config {
        /// The key (`section.name`).
        key: Option<String>,
        /// The value to set.
        value: Option<String>,
        /// Use the user's global config (~/.gitconfig or $GIT_CONFIG_GLOBAL).
        #[arg(long)]
        global: bool,
        /// Use only the repository's config.
        #[arg(long, conflicts_with = "global")]
        local: bool,
        /// Print the key's value (the default with just a key).
        #[arg(long)]
        get: bool,
        /// Print every value of a multi-valued key.
        #[arg(long)]
        get_all: bool,
        /// Remove the key.
        #[arg(long)]
        unset: bool,
        /// Remove every value of a multi-valued key.
        #[arg(long)]
        unset_all: bool,
        /// List every `key=value`.
        #[arg(short = 'l', long)]
        list: bool,
        /// Add a value to a multi-valued key instead of replacing it.
        #[arg(long)]
        add: bool,
        /// Read and write the value as a boolean (true/false).
        #[arg(long = "bool")]
        as_bool: bool,
        /// Read and write the value as an integer (k/m/g suffixes allowed).
        #[arg(long = "int", conflicts_with = "as_bool")]
        as_int: bool,
    },
    /// Apply a patch to the working tree, the index, or both.
    Apply {
        /// Patch files (reads stdin when none or `-`).
        patches: Vec<String>,
        /// Apply to the index only, leaving the working tree as it is.
        #[arg(long)]
        cached: bool,
        /// Apply to both the index and the working tree.
        #[arg(long, conflicts_with = "cached")]
        index: bool,
        /// Only check that the patch applies.
        #[arg(long)]
        check: bool,
        /// Undo the patch (apply it in reverse).
        #[arg(short = 'R', long)]
        reverse: bool,
        /// Print the patch's diffstat instead of applying it.
        #[arg(long)]
        stat: bool,
    },
    /// Notes attached to commits (no subcommand lists them).
    Notes {
        /// The notes ref (default refs/notes/commits).
        #[arg(long = "ref", value_name = "REF")]
        notes_ref: Option<String>,
        #[command(subcommand)]
        cmd: Option<NotesCmd>,
    },
    /// Point a ref at a commit, or delete it, optionally only if it holds an
    /// expected value.
    UpdateRef {
        /// The ref (e.g. refs/heads/main).
        name: String,
        /// The new value (with -d: the expected old value).
        new: Option<String>,
        /// Only update if the ref holds this value (all zeros: does not exist).
        old: Option<String>,
        /// Delete the ref.
        #[arg(short = 'd')]
        delete: bool,
        /// Update a symbolic ref itself instead of the ref it points to.
        #[arg(long)]
        no_deref: bool,
        /// The reflog message.
        #[arg(short = 'm', value_name = "REASON")]
        message: Option<String>,
    },
    /// Print the object id of files or stdin; -w stores them.
    HashObject {
        /// Files to hash.
        paths: Vec<String>,
        /// Store the objects in the repository.
        #[arg(short = 'w')]
        write: bool,
        /// Hash stdin (before any files).
        #[arg(long)]
        stdin: bool,
        /// The object type.
        #[arg(short = 't', default_value = "blob", value_name = "TYPE")]
        kind: String,
    },
    /// Write commits as mbox patch files (`-<n>`, `<since>` or `<a>..<b>`).
    FormatPatch {
        /// `-<n>` for the newest n commits, `<rev>` for the commits after it
        /// up to HEAD, or a range `<a>..<b>`.
        #[arg(allow_negative_numbers = true)]
        revs: Vec<String>,
        /// Write the files to this folder.
        #[arg(short = 'o', long = "output-directory", value_name = "DIR")]
        output_dir: Option<String>,
        /// Print the patches instead of writing files.
        #[arg(long)]
        stdout: bool,
    },
    /// Apply mbox patches (from format-patch) as commits.
    Am {
        /// mbox files (reads stdin when none).
        mbox: Vec<String>,
        /// Give up and restore the branch as it was.
        #[arg(long, conflicts_with_all = ["cont", "skip"])]
        abort: bool,
        /// Commit the resolved patch and go on.
        #[arg(long = "continue", conflicts_with = "skip")]
        cont: bool,
        /// Skip the current patch.
        #[arg(long)]
        skip: bool,
        /// Fall back to a three-way merge.
        #[arg(short = '3', long)]
        three_way: bool,
        /// Add a Signed-off-by trailer.
        #[arg(short = 's', long)]
        signoff: bool,
    },
    /// Write a tar or zip of a revision's files.
    Archive {
        /// The revision (default HEAD).
        rev: Option<String>,
        /// Limit to these paths.
        paths: Vec<String>,
        /// tar, tgz (tar.gz) or zip (default: from -o's extension, else tar).
        #[arg(long, value_parser = ["tar", "tgz", "tar.gz", "zip"])]
        format: Option<String>,
        /// Write to this file instead of stdout.
        #[arg(short = 'o', long)]
        output: Option<String>,
        /// Put every entry under this folder (e.g. `project/`).
        #[arg(long)]
        prefix: Option<String>,
    },
    /// Pack the object database and prune unreachable objects.
    Gc {
        /// Prune loose objects older than this date (default 2 weeks ago).
        #[arg(long, value_name = "DATE", num_args = 0..=1, require_equals = true,
              default_missing_value = "")]
        prune: Option<String>,
        /// Repack more thoroughly (slow).
        #[arg(long)]
        aggressive: bool,
        /// Only run when enough loose objects have piled up.
        #[arg(long)]
        auto: bool,
    },
    /// Check the object database for corruption and dangling objects.
    Fsck {
        /// Also check packed objects.
        #[arg(long)]
        full: bool,
        /// Strict checking.
        #[arg(long)]
        strict: bool,
        /// List unreachable objects.
        #[arg(long)]
        unreachable: bool,
        /// Do not report dangling objects.
        #[arg(long)]
        no_dangling: bool,
        /// Check only that objects are connected.
        #[arg(long)]
        connectivity_only: bool,
    },
    /// Remove untracked files and directories.
    Clean {
        /// List what would be removed without deleting (git's -n).
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
        /// Also remove ignored files (git's -x).
        #[arg(short = 'x')]
        ignored_too: bool,
        /// Remove only ignored files (git's -X).
        #[arg(short = 'X', conflicts_with = "ignored_too")]
        only_ignored: bool,
        /// Keep files matching this pattern (git's -e).
        #[arg(short = 'e', long = "exclude", value_name = "PATTERN")]
        exclude: Vec<String>,
        /// Accepted for git compatibility: rgit always removes folders.
        #[arg(short = 'd', hide = true)]
        dirs: bool,
        /// Accepted for git compatibility: rgit needs no -f.
        #[arg(short = 'f', long, hide = true)]
        force: bool,
        /// Limit to these paths.
        paths: Vec<String>,
    },
    /// Remove tracked paths from the index and working tree.
    Rm {
        /// The paths to remove: files, folders (with -r) or globs (prompted for
        /// if omitted on a terminal).
        paths: Vec<String>,
        /// Remove only from the index, keeping the working-tree file (--cached).
        #[arg(long)]
        cached: bool,
        /// Remove folders recursively (git's -r).
        #[arg(short = 'r')]
        recursive: bool,
        /// Accepted for git compatibility.
        #[arg(short = 'f', long, hide = true)]
        force: bool,
    },
    /// Rename/move tracked files or folders.
    Mv {
        /// The paths to move, then the destination (a folder when moving several).
        #[arg(required = true, num_args = 2.., value_name = "PATH")]
        paths: Vec<String>,
        /// Overwrite the destination if it exists (git's -f).
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Describe a revision relative to the nearest tag (default HEAD).
    Describe {
        /// The revision to describe (defaults to HEAD).
        rev: Option<String>,
        /// Use lightweight tags too, not just annotated ones (git's --tags).
        #[arg(long)]
        tags: bool,
        /// Append -dirty when the working tree has uncommitted changes.
        #[arg(long)]
        dirty: bool,
        /// Always use the long format (tag-count-oid), even on a tag.
        #[arg(long)]
        long: bool,
        /// Number of hex digits for the abbreviated commit oid (0: the tag only).
        #[arg(long, value_name = "N")]
        abbrev: Option<u32>,
        /// Show the abbreviated commit oid when no tag is found (rgit always does).
        #[arg(long)]
        always: bool,
        /// Only consider tags matching this glob.
        #[arg(long = "match", value_name = "GLOB")]
        pattern: Option<String>,
        /// Print the tag only when it points at the revision itself; else fail.
        #[arg(long)]
        exact_match: bool,
    },
    /// Create a new repository in the current directory (or PATH).
    Init {
        /// Where to create the repository (defaults to the current directory).
        path: Option<String>,
        /// Name of the initial branch (git's -b/--initial-branch).
        #[arg(short = 'b', long = "initial-branch")]
        initial_branch: Option<String>,
        /// Create a bare repository (git's --bare).
        #[arg(long)]
        bare: bool,
    },
    /// Clone a repository into a new directory.
    Clone {
        /// The repository URL to clone.
        url: String,
        /// The target directory (defaults to the repository name).
        dir: Option<String>,
        /// Check out this branch instead of the remote's default (git's -b).
        #[arg(short = 'b', long)]
        branch: Option<String>,
        /// Shallow-clone this many commits of history (git's --depth).
        #[arg(long, default_value_t = 0)]
        depth: i32,
        /// Make a bare repository (git's --bare).
        #[arg(long)]
        bare: bool,
        /// Name the remote this instead of `origin` (git's -o).
        #[arg(short = 'o', long)]
        origin: Option<String>,
        /// Clone the submodules too (git's --recurse-submodules).
        #[arg(long)]
        recurse_submodules: bool,
    },
    /// Submodule management: forwards to `git submodule <args>`.
    Submodule {
        /// Arguments passed to `git submodule`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Escape hatch: run any `git` subcommand and print its output.
    Git {
        /// The git subcommand and its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the Model Context Protocol server over stdio.
    Mcp,
    /// Install session hooks so Claude Code, Codex, and OpenCode start with rgit context.
    Hooks {
        #[command(subcommand)]
        cmd: HooksCmd,
    },
    /// Install the rgit Agent Skill for Claude Code, Codex, and other agents.
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
    },
    /// Semantic code search: build the on-disk vector index or query it.
    Index {
        #[command(subcommand)]
        action: IndexCmd,
    },
    /// Serve the web viewer for this repository, or a directory of repositories.
    Serve {
        /// Address to bind.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to listen on.
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Serve every git repository under this directory, addressed by name,
        /// instead of the current repository.
        #[arg(long)]
        root: Option<String>,
        /// Public clone base (e.g. https://git.example.dev). When set, the shown
        /// clone URL is <base>/<repo>.git instead of the repo's own remotes.
        #[arg(long)]
        clone_base: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum HooksCmd {
    /// Install or repair the session-start hook (project scope by default).
    Install {
        /// Install into the user's home config instead of this project.
        #[arg(long)]
        user: bool,
        /// Which agent app to configure.
        #[arg(long, value_enum, default_value_t = crate::setup::HookApp::All)]
        app: crate::setup::HookApp,
    },
    /// Show which agent apps have the rgit hook and whether it is current.
    Status,
}

#[derive(Subcommand)]
pub enum SkillsCmd {
    /// List embedded skills and install targets.
    List,
    /// Print the rgit SKILL.md, or its command reference.
    Show {
        /// Print references/commands.md instead of SKILL.md.
        #[arg(long)]
        reference: bool,
    },
    /// Install embedded skills into user or project skill directories.
    Install {
        /// Install into this project.
        #[arg(long)]
        project: bool,
        /// Install into the current user's home directory.
        #[arg(long)]
        user: bool,
        /// Which client layout to write.
        #[arg(long, value_enum, default_value_t = SkillTarget::All)]
        target: SkillTarget,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SkillTarget {
    /// Portable Agent Skills path used by Pi, Codex, and other clients.
    #[value(alias = "pi", alias = "codex")]
    Agents,
    /// Claude Code native skill path.
    Claude,
    /// Both portable and Claude Code paths.
    All,
}

impl std::fmt::Display for SkillTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SkillTarget::Agents => "agents",
            SkillTarget::Claude => "claude",
            SkillTarget::All => "all",
        })
    }
}

#[derive(Subcommand)]
pub enum IndexCmd {
    /// Build (or incrementally rebuild) the index. Local by default; `--root DIR`
    /// builds every repo under a directory.
    Build {
        /// Build every git repo directly under this directory (global).
        #[arg(long)]
        root: Option<String>,
    },
    /// Search the semantic index by meaning (local, or global with `--root`).
    /// Results are re-ranked by git history, so recently and frequently changed
    /// files surface above cold ones at equal relevance.
    Search {
        /// The natural-language or code query.
        query: String,
        /// Maximum results.
        #[arg(short, long, default_value_t = 8)]
        limit: usize,
        /// Search every git repo directly under this directory (global).
        #[arg(long)]
        root: Option<String>,
    },
    /// Report whether an index exists and how many chunks it holds.
    Status,
    /// Hybrid search: fuse literal grep and semantic ranking, then re-rank by
    /// git history (churn and recency) (local, or global with `--root`).
    Code {
        /// The query (literal terms help the lexical side; prose helps semantic).
        query: String,
        /// Maximum results.
        #[arg(short, long, default_value_t = 8)]
        limit: usize,
        /// Search every git repo directly under this directory (global).
        #[arg(long)]
        root: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum BranchCmd {
    /// Create a branch and switch to it.
    Create {
        /// The new branch name.
        name: String,
    },
    /// Check out an existing branch.
    Checkout {
        /// The branch to check out.
        name: String,
    },
    /// Delete a branch (multiselect prompt if no name on a terminal).
    Delete {
        /// The branch to delete.
        name: Option<String>,
        /// Delete even if not fully merged (git's -D).
        #[arg(short = 'D', long = "force")]
        force: bool,
    },
    /// Rename a branch.
    Rename {
        /// The current branch name.
        old: String,
        /// The new branch name.
        new: String,
    },
    /// Delete every local branch already merged into a base (default HEAD).
    Prune {
        /// Delete branches merged into this revision.
        #[arg(default_value = "HEAD")]
        base: String,
    },
}

#[derive(Subcommand)]
pub enum StashCmd {
    /// Stash the working tree, with an optional message.
    Push {
        /// A description for the stash.
        message: Option<String>,
        /// Also stash untracked files (git's -u).
        #[arg(short = 'u', long = "include-untracked")]
        include_untracked: bool,
    },
    /// Apply a stash and drop it (prompted for if no index on a terminal).
    Pop {
        /// The stash index (defaults to the most recent).
        index: Option<usize>,
    },
    /// Apply a stash without dropping it.
    Apply {
        /// The stash index (defaults to the most recent).
        index: Option<usize>,
    },
    /// Drop a stash.
    Drop {
        /// The stash index (defaults to the most recent).
        index: Option<usize>,
    },
    /// List the stashes.
    List,
}

#[derive(Subcommand)]
pub enum NotesCmd {
    /// List notes as `<note id> <object id>`, or the note id of one object.
    List {
        /// The annotated object.
        rev: Option<String>,
    },
    /// Print an object's note (default HEAD).
    Show {
        /// The annotated object.
        rev: Option<String>,
    },
    /// Attach a note to an object (default HEAD).
    Add {
        /// The annotated object.
        rev: Option<String>,
        /// The note text; several -m become paragraphs.
        #[arg(short, long, required = true)]
        message: Vec<String>,
        /// Replace an existing note.
        #[arg(short, long)]
        force: bool,
    },
    /// Add a paragraph to an object's note, creating it if needed.
    Append {
        /// The annotated object.
        rev: Option<String>,
        /// The text to add; several -m become paragraphs.
        #[arg(short, long, required = true)]
        message: Vec<String>,
    },
    /// Remove an object's note (default HEAD).
    Remove {
        /// The annotated object.
        rev: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RemoteCmd {
    /// Add a remote.
    Add {
        /// The remote name.
        name: String,
        /// The remote URL.
        url: String,
    },
    /// Remove a remote.
    Remove {
        /// The remote name.
        name: String,
    },
    /// Change a remote's URL.
    SetUrl {
        /// The remote name.
        name: String,
        /// The new URL.
        url: String,
    },
    /// Rename a remote.
    Rename {
        /// The current remote name.
        old: String,
        /// The new remote name.
        new: String,
    },
}

#[derive(Subcommand)]
pub enum WorktreeCmd {
    /// Add a linked worktree.
    Add {
        /// The worktree name (branch).
        name: String,
        /// The path for the new worktree.
        path: String,
    },
    /// Remove a linked worktree.
    Remove {
        /// The worktree name.
        name: String,
        /// Remove even if locked (git's -f/--force).
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Prune worktree entries whose working tree is gone (git's `worktree prune`).
    Prune,
}

/// Subcommands for stacked branches.
#[derive(clap::Subcommand)]
pub enum StackCmd {
    /// Create a new branch stacked on the current one.
    New {
        /// The new branch name.
        name: String,
    },
    /// List the stack containing the current branch.
    List,
    /// Rebase every descendant onto its parent's new tip.
    Restack,
}

/// Subcommands for lanes.
#[derive(clap::Subcommand)]
pub enum LanesCmd {
    /// Enter lanes mode: record the fork point and a default lane.
    Init,
    /// Leave lanes mode (lane branches are kept).
    Off,
    /// List the lanes and their owned files (the default when no subcommand).
    List,
    /// Create a new lane committing to a same-named branch.
    New {
        /// The lane (and branch) name.
        name: String,
    },
    /// Create a new lane stacked on another (its commits build on that lane).
    Stack {
        /// The new lane name.
        name: String,
        /// The parent lane to stack on.
        #[arg(long)]
        on: String,
    },
    /// Assign a worktree path to a lane, or a single hunk with `--hunk`.
    Assign {
        /// The lane to assign to.
        lane: String,
        /// The path to assign.
        path: String,
        /// Assign only the hunk starting at this new-file line, not the file.
        #[arg(long)]
        hunk: Option<u32>,
    },
    /// Return a path to the default lane.
    Unassign {
        /// The path to unassign.
        path: String,
    },
    /// Commit a lane's owned changes to its branch.
    Commit {
        /// The lane to commit.
        lane: String,
        /// The commit message.
        #[arg(short, long)]
        message: String,
    },
    /// Rename a lane and its branch.
    Rename {
        /// The lane to rename.
        old: String,
        /// The new name.
        new: String,
    },
    /// Delete a lane (its changes return to default; its branch is kept).
    Delete {
        /// The lane to delete.
        name: String,
    },
    /// Push a lane's branch to the remote.
    Push {
        /// The lane to push.
        lane: String,
    },
    /// Push a lane's branch and open a pull request (via gh/glab).
    Pr {
        /// The lane to open a PR for.
        lane: String,
    },
    /// Move each stacked lane onto its parent lane's new tip (in the odb; the
    /// worktree is not touched).
    Restack,
}

/// Subcommands for branching workflows.
#[derive(clap::Subcommand)]
pub enum FlowCmd {
    /// Set the active workflow: gitflow, github, gitlab, trunk, release-flow.
    Init {
        /// The workflow preset name.
        preset: String,
    },
    /// Start a feature branch per the active workflow.
    Start {
        /// The feature name.
        name: String,
    },
    /// Finish the current feature (local merge, or push + PR per the workflow).
    Finish,
    /// Start a release (or finish it with --finish).
    Release {
        /// The release version.
        version: String,
        /// Finish the release instead of starting it.
        #[arg(long)]
        finish: bool,
    },

    /// Show the active workflow and its policy.
    Status,
}

/// Authentication commands shared by all forge providers.
#[derive(clap::Subcommand)]
pub enum AuthCmd {
    /// List known forge providers and whether credentials are stored.
    List,
    /// Show detailed authentication status.
    Status,
}

/// Native forge management commands.
#[derive(clap::Subcommand)]
pub enum ForgeCmd {
    /// Store a forge credential in the OS credential store.
    Login {
        provider: String,
        /// Forge host, such as `https://gitlab.example.com`.
        #[arg(long)]
        host: Option<String>,
        /// Credential account label.
        #[arg(long, default_value = "default")]
        account: String,
        /// Read the token from stdin; it is never accepted as a command argument.
        #[arg(long, default_value_t = false)]
        token_stdin: bool,
    },
    /// Show authentication status for configured forge providers.
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
    /// Show the authenticated account for one forge.
    Whoami {
        /// Provider name, such as `github`; defaults when unambiguous.
        provider: Option<String>,
        /// Forge host override.
        #[arg(long)]
        host: Option<String>,
        /// Credential account label.
        #[arg(long, default_value = "default")]
        account: String,
    },
    /// Remove the stored forge credential.
    Logout {
        provider: String,
        /// Forge host override.
        #[arg(long)]
        host: Option<String>,
        /// Credential account label.
        #[arg(long, default_value = "default")]
        account: String,
    },
    /// View, create, or delete a hosted repository.
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
    /// List or delete branches on the forge.
    Branch {
        #[command(subcommand)]
        cmd: ForgeBranchCmd,
    },
    /// List, create, or close pull requests (merge requests on GitLab).
    Pr {
        #[command(subcommand)]
        cmd: PrCmd,
    },
}

#[derive(clap::Subcommand)]
pub enum RepoCmd {
    /// Show repository metadata.
    View {
        /// Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote.
        target: Option<String>,
        repo: Option<String>,
    },
    /// Create a repository.
    Create {
        #[arg(long, default_value = "github")]
        provider: String,
        name: String,
        #[arg(long)]
        organization: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        private: bool,
        #[arg(long)]
        auto_init: bool,
    },
    /// Delete a repository after explicit confirmation.
    Delete {
        /// Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote.
        target: Option<String>,
        repo: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum ForgeBranchCmd {
    /// List remote branches, 100 per page.
    List {
        /// Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote.
        target: Option<String>,
        repo: Option<String>,
        /// Which page of 100 to show.
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    /// Delete a remote branch after explicit confirmation.
    Delete {
        branch: String,
        /// Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote.
        target: Option<String>,
        repo: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum PrCmd {
    /// List open pull requests or merge requests, 100 per page.
    List {
        /// Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote.
        target: Option<String>,
        repo: Option<String>,
        /// Which page of 100 to show.
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    /// Create a pull request or merge request.
    Create {
        /// Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote.
        target: Option<String>,
        repo: Option<String>,
        #[arg(long)]
        title: String,
        #[arg(long)]
        head: String,
        #[arg(long)]
        base: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        draft: bool,
    },
    /// Close a pull request or merge request.
    Close {
        number: u64,
        /// Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote.
        target: Option<String>,
        repo: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}
/// Subcommands for CoW workspaces.
#[derive(clap::Subcommand)]
pub enum WorkspaceCmd {
    /// Create a copy-on-write clone of the repo on a new branch.
    New {
        /// The workspace name (also the new branch name).
        name: String,
    },
    /// List this repo's workspaces.
    List,
    /// Remove a workspace.
    Remove {
        /// The workspace name.
        name: String,
    },
}

/// Build or query the semantic index for the current repository.
fn index_cmd(backend: &Arc<dyn GitBackend>, action: IndexCmd) -> anyhow::Result<String> {
    match action {
        IndexCmd::Build { root } => index_build(backend, root.as_deref()),
        IndexCmd::Search { query, limit, root } => {
            semantic_search(backend, root.as_deref(), &query, limit)
        }
        IndexCmd::Code { query, limit, root } => {
            code_search(backend, root.as_deref(), &query, limit)
        }
        IndexCmd::Status => {
            let path = rgit_index::index_path(backend.workdir());
            match rgit_index::load(&path) {
                Some(index) => Ok(format!(
                    "indexed: {} chunks ({})",
                    index.len(),
                    path.display()
                )),
                None => Ok(format!(
                    "no index ({}); run `rgit index build`",
                    path.display()
                )),
            }
        }
    }
}

/// One repo's directory name, for labeling multi-repo output.
fn repo_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_owned())
}

/// Resolve a scope to (label, backend) targets. `root` searches/builds every git
/// repo directly under a directory (global); otherwise the single `default` repo
/// (local).
pub(crate) fn repo_targets(
    root: Option<&str>,
    default: &Arc<dyn GitBackend>,
) -> anyhow::Result<Vec<(String, Arc<dyn GitBackend>)>> {
    match root.filter(|r| !r.trim().is_empty()) {
        None => Ok(vec![(repo_label(default.workdir()), default.clone())]),
        Some(dir) => {
            let mut out: Vec<(String, Arc<dyn GitBackend>)> = Vec::new();
            for entry in std::fs::read_dir(dir)?.flatten() {
                let p = entry.path();
                if p.is_dir()
                    && let Ok(b) = rgit_git::Git2Backend::discover(&p)
                {
                    out.push((repo_label(&p), Arc::new(b)));
                }
            }
            out.sort_by(|a, b| a.0.cmp(&b.0));
            if out.is_empty() {
                anyhow::bail!("no git repositories directly under {dir}");
            }
            Ok(out)
        }
    }
}

/// A fused search result across (possibly) many repos.
pub(crate) struct Hit {
    pub score: f64,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub tag: &'static str,
}

/// Hybrid hits for one repo: fuse literal `grep` and semantic ranking with
/// reciprocal-rank fusion (RRF), bucketing locations to the chunk window so a
/// lexical match and a semantic hit in the same region reinforce each other.
/// Degrades to grep when the repo has no index.
fn hybrid_hits(backend: &Arc<dyn GitBackend>, repo: &str, query: &str, pool: usize) -> Vec<Hit> {
    use std::collections::HashMap;
    const K: f64 = 60.0;
    const CHUNK_STEP: usize = 30;

    let semantic = match rgit_index::load(&rgit_index::index_path(backend.workdir())) {
        Some(index) => {
            let boost = history_boost(backend);
            rgit_index::Embedder::new()
                .and_then(|e| {
                    rgit_index::search_boosted(&index, &e, query, pool, &boost, HISTORY_ALPHA)
                })
                .unwrap_or_default()
        }
        None => Vec::new(),
    };
    let lexical = backend
        .grep_query(&rgit_git::GrepQuery {
            pattern: query.to_owned(),
            regex: false,
            path: None,
            exts: Vec::new(),
        })
        .unwrap_or_default();

    #[derive(Default)]
    struct Fused {
        score: f64,
        lexical: bool,
        semantic: bool,
        line: usize,
    }
    let bucket = |line: usize| (line.saturating_sub(1) / CHUNK_STEP) * CHUNK_STEP + 1;
    let mut acc: HashMap<(String, usize), Fused> = HashMap::new();
    for (rank, m) in lexical.iter().enumerate().take(pool) {
        let e = acc.entry((m.path.clone(), bucket(m.line))).or_default();
        e.score += 1.0 / (K + rank as f64);
        e.lexical = true;
        if e.line == 0 {
            e.line = m.line;
        }
    }
    for (rank, h) in semantic.iter().enumerate() {
        let e = acc
            .entry((h.path.clone(), bucket(h.start_line)))
            .or_default();
        e.score += 1.0 / (K + rank as f64);
        e.semantic = true;
        if e.line == 0 {
            e.line = h.start_line;
        }
    }
    acc.into_iter()
        .map(|((path, _), f)| Hit {
            score: f.score,
            repo: repo.to_owned(),
            path,
            line: f.line,
            tag: match (f.lexical, f.semantic) {
                (true, true) => "both",
                (true, false) => "lexical",
                _ => "semantic",
            },
        })
        .collect()
}

/// Hybrid code search over a scope (one repo, or every repo under `root`),
/// fusing lexical and semantic ranking. Results are rendered `score path:line
/// [tag]`, prefixed with the repo when the scope spans more than one.
/// Ranked hybrid code-search hits, and whether they span several repos.
pub(crate) fn code_hits(
    backend: &Arc<dyn GitBackend>,
    root: Option<&str>,
    query: &str,
    limit: usize,
) -> anyhow::Result<(Vec<Hit>, bool)> {
    let targets = repo_targets(root, backend)?;
    let multi = targets.len() > 1;
    let pool = (limit * 3).max(20);
    let mut hits: Vec<Hit> = Vec::new();
    for (label, b) in &targets {
        hits.extend(hybrid_hits(b, label, query, pool));
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    Ok((hits, multi))
}

pub(crate) fn code_search(
    backend: &Arc<dyn GitBackend>,
    root: Option<&str>,
    query: &str,
    limit: usize,
) -> anyhow::Result<String> {
    let (hits, multi) = code_hits(backend, root, query, limit)?;
    if hits.is_empty() {
        return Ok("no matches".to_owned());
    }
    let mut out = String::new();
    for h in hits {
        if multi {
            out.push_str(&format!(
                "{:.4}  {}/{}:{}  [{}]\n",
                h.score, h.repo, h.path, h.line, h.tag
            ));
        } else {
            out.push_str(&format!(
                "{:.4}  {}:{}  [{}]\n",
                h.score, h.path, h.line, h.tag
            ));
        }
    }
    Ok(out.trim_end().to_owned())
}

/// How strongly git history reorders semantic results, and how far back the
/// churn/recency walk looks.
const HISTORY_ALPHA: f32 = 0.5;
const HISTORY_WINDOW: usize = 500;

/// Per-path churn/recency weights for a repo, or an empty map when history is
/// unavailable (a shallow or empty repo). Never fails the search.
fn history_boost(backend: &Arc<dyn GitBackend>) -> std::collections::HashMap<String, f32> {
    backend
        .file_activity(HISTORY_WINDOW)
        .map(|a| rgit_git::activity_weights(&a))
        .unwrap_or_default()
}

/// Semantic-only search over a scope (one repo, or every repo under `root`).
/// A semantic hit as `(score, repo label, hit)`.
pub(crate) type RepoHit = (f32, String, rgit_index::SearchHit);

/// Semantic hits as `(score, repo, hit)`, best first, and whether they span
/// several repos. Empty when no target repo has an index.
pub(crate) fn semantic_hits(
    backend: &Arc<dyn GitBackend>,
    root: Option<&str>,
    query: &str,
    limit: usize,
) -> anyhow::Result<(Vec<RepoHit>, bool)> {
    let targets = repo_targets(root, backend)?;
    let multi = targets.len() > 1;
    let embedder = rgit_index::Embedder::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut hits: Vec<RepoHit> = Vec::new();
    for (label, b) in &targets {
        if let Some(index) = rgit_index::load(&rgit_index::index_path(b.workdir())) {
            let boost = history_boost(b);
            for h in
                rgit_index::search_boosted(&index, &embedder, query, limit, &boost, HISTORY_ALPHA)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
            {
                hits.push((h.score, label.clone(), h));
            }
        }
    }
    hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);
    Ok((hits, multi))
}

pub(crate) fn semantic_search(
    backend: &Arc<dyn GitBackend>,
    root: Option<&str>,
    query: &str,
    limit: usize,
) -> anyhow::Result<String> {
    let (hits, multi) = semantic_hits(backend, root, query, limit)?;
    if hits.is_empty() {
        return Ok("no index; run `rgit index build` first".to_owned());
    }
    let mut out = String::new();
    for (score, repo, h) in hits {
        if multi {
            out.push_str(&format!(
                "{score:.3}  {}/{}:{}-{}\n",
                repo, h.path, h.start_line, h.end_line
            ));
        } else {
            out.push_str(&format!(
                "{score:.3}  {}:{}-{}\n",
                h.path, h.start_line, h.end_line
            ));
        }
    }
    Ok(out.trim_end().to_owned())
}

/// Build (incrementally) the index for one repo, or every repo under `root`.
pub(crate) fn index_build(
    backend: &Arc<dyn GitBackend>,
    root: Option<&str>,
) -> anyhow::Result<String> {
    let targets = repo_targets(root, backend)?;
    let embedder = rgit_index::Embedder::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut out = String::new();
    for (label, b) in &targets {
        let path = rgit_index::index_path(b.workdir());
        let previous = rgit_index::load(&path);
        let (index, stats) = rgit_index::build_with(b.workdir(), &embedder, previous.as_ref())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        rgit_index::save(&index, &path).map_err(|e| anyhow::anyhow!("{e}"))?;
        out.push_str(&format!(
            "{label}: {} chunks ({} reused, {} embedded)\n",
            stats.total, stats.reused, stats.embedded
        ));
    }
    Ok(out.trim_end().to_owned())
}

/// Parse an ISO date (`YYYY-MM-DD`, optionally with `THH:MM:SS` or a space and a
/// time) into a unix timestamp in UTC. A bare date is midnight UTC. Uses the
/// days-from-civil algorithm rather than pulling in a date crate.
pub(crate) fn parse_date(s: &str) -> anyhow::Result<i64> {
    let err = || anyhow::anyhow!("bad date {s:?}; use YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS");
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut dp = date.split('-');
    let mut next = || dp.next().ok_or_else(err)?.parse::<i64>().map_err(|_| err());
    let (y, m, d) = (next()?, next()?, next()?);
    if dp.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(err());
    }
    // days_from_civil (Howard Hinnant, public domain): days since 1970-01-01.
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let mut secs = (era * 146097 + doe - 719468) * 86400;
    if let Some(t) = time {
        let mut tp = t.split(':');
        let mut part = || tp.next().unwrap_or("0").parse::<i64>().map_err(|_| err());
        secs += part()? * 3600 + part()? * 60 + part()?;
    }
    Ok(secs)
}

/// One-line identity shared by the home view and the installed skill.
pub const DESCRIPTION: &str = "Inspect and change the git repository in the current directory";

/// Next-step commands shared by the home view and the installed skill.
pub const HOME_HELP: [&str; 5] = [
    "Run `rgit diff --patch` to see unstaged changes",
    "Run `rgit stage <path>` to stage a file",
    "Run `rgit commit -m \"<message>\"` to commit staged changes",
    "Run `rgit log --limit 20` for recent commits",
    "Run `rgit smartlog` for local branches and stacks",
];

/// An error with a fix to suggest and its own exit code (2 = usage).
#[derive(Debug)]
pub struct CliError {
    pub message: String,
    pub help: Option<String>,
    pub code: i32,
}

impl CliError {
    pub fn usage(message: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(CliError {
            message: message.into(),
            help: None,
            code: 2,
        })
    }

    pub fn not_a_repo() -> anyhow::Error {
        anyhow::Error::new(CliError {
            message: "no git repository found".to_owned(),
            help: Some("Run `rgit init` to create one here".to_owned()),
            code: 1,
        })
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

/// Git spellings rgit names differently: (rgit command path, git tokens, fix).
pub const GIT_SPELLINGS: &[(&str, &[&str], &str)] = &[
    (
        "rgit",
        &["add"],
        "use `rgit stage <path>` (or `rgit stage-all`)",
    ),
    (
        "rgit",
        &["switch"],
        "use `rgit checkout <branch>` (`-b <new>` to create)",
    ),
    (
        "rgit",
        &["restore"],
        "use `rgit discard <path>` (worktree) or `rgit unstage <path>` (index)",
    ),
    (
        "rgit",
        &["rev-parse", "cat-file", "ls-files", "for-each-ref"],
        "plumbing is not wrapped; run `rgit git <args>`",
    ),
    (
        "rgit branch",
        &["-d", "--delete"],
        "use `rgit branch delete <name>`",
    ),
    (
        "rgit branch",
        &["-D"],
        "use `rgit branch delete <name> --force`",
    ),
    (
        "rgit branch",
        &["-m", "--move"],
        "use `rgit branch rename <old> <new>`",
    ),
    ("rgit stash", &["save"], "use `rgit stash push [<message>]`"),
    (
        "rgit stash",
        &["show"],
        "use `rgit stash list`, then `rgit git stash show -p <stash>`",
    ),
    (
        "rgit log",
        &["--graph"],
        "use `rgit smartlog` for the branch graph",
    ),
    (
        "rgit push",
        &["-f"],
        "use `--force-with-lease` (or `--force`)",
    ),
];

/// Top-level commands grouped by task, in the order the skill lists them.
const SKILL_GROUPS: &[(&str, &[&str])] = &[
    (
        "Inspect",
        &[
            "status", "log", "diff", "show", "blame", "refs", "smartlog", "describe",
        ],
    ),
    (
        "Stage and discard",
        &[
            "stage",
            "unstage",
            "stage-all",
            "unstage-all",
            "discard",
            "resolve",
            "rm",
            "mv",
            "clean",
        ],
    ),
    (
        "Commit and rewrite history",
        &[
            "commit",
            "extend",
            "reword",
            "uncommit",
            "squash",
            "move",
            "split",
            "absorb",
            "cherry-pick",
            "revert",
            "reset",
        ],
    ),
    ("Undo", &["undo", "redo", "oplog"]),
    (
        "Branches, tags, stashes",
        &[
            "branch", "checkout", "merge", "rebase", "tag", "stash", "bisect", "prune",
        ],
    ),
    (
        "Stacks, lanes, workspaces, worktrees",
        &[
            "stack",
            "next",
            "prev",
            "sync",
            "submit",
            "lanes",
            "workspace",
            "worktree",
            "flow",
        ],
    ),
    (
        "Remotes and forge (GitHub/GitLab)",
        &["fetch", "pull", "push", "remote", "forge"],
    ),
    (
        "Patches, notes, config and maintenance",
        &[
            "format-patch",
            "am",
            "apply",
            "archive",
            "notes",
            "config",
            "update-ref",
            "hash-object",
            "gc",
            "fsck",
        ],
    ),
    ("Code search", &["index"]),
    (
        "Repositories and escape hatch",
        &["init", "clone", "submodule", "git"],
    ),
    (
        "Agent integration and servers",
        &["hooks", "skills", "mcp", "serve"],
    ),
];

const SKILL_INTRO: &str = r#"---
name: rgit
description: Use for any git work in this repository - inspecting changes, committing, rewriting or undoing history, branches and stacks, and GitHub/GitLab PRs.
---

# rgit

{DESCRIPTION}. Prefer `rgit` over raw `git`. Run `rgit` with no arguments first: it prints the repo's current state and the next useful commands.

If `rgit` is not on PATH, install it with `cargo install --locked --git https://github.com/rslib/rgit rgit-cli`.

## Start here

"#;

const SKILL_RULES: &str = r#"
## Output

- rgit prints TOON when stdout is not a terminal. If your shell runs commands in a terminal (a PTY), add `--toon` (alias `--axi`) so you still get TOON with no color, spinners, or prompts. `--json` gives the same data as JSON.
- Use `--toon` or `--axi`, not `--porcelain`. In rgit, as in git, `--porcelain` exists only on `status` and prints git's raw script format, with no counts, hints, or schema.
- Output ends with `help` lines naming useful next commands; follow them. Lists include counts (`count: 20 of 65 total`) and say explicitly when they are empty.
- `--fields a,b` adds table columns; an unknown field lists the valid ones. `--full` disables truncation of patches, commit bodies, and long output.
- Exit codes: 0 success (including no-ops), 1 error, 2 usage error. Errors print `error:` and `help:` on stdout.
- rgit never prompts in agent mode. Pass every value as a flag or argument.

## Safety

- Almost every change is recorded in the op-log. `rgit undo` restores HEAD and the working tree (including uncommitted work) from before the last operation; `rgit redo` reverses it; `rgit oplog` lists it.
- Repeating a change whose result already holds is a no-op (exit 0), for example creating an existing branch or deleting a missing tag.
- Destructive forge operations require `--yes`.
- For git plumbing rgit does not wrap, use `rgit git <args>`.

## Git spellings

"#;

const SKILL_FOOTER: &str = r#"
## More commands

rgit also covers history rewriting (reword, squash, split, move, absorb), stacked branches, lanes, copy-on-write workspaces, branching workflows, remotes, GitHub/GitLab repos and PRs, and code search. When a task needs a command not shown above, read [references/commands.md](references/commands.md) for every command with examples, or run `rgit <command> --help`.
"#;

/// The rgit Agent Skill: the home view's next steps plus the rules an agent
/// needs up front. The full command list lives in [`skill_reference`] and is
/// read only on demand.
pub(crate) fn skill_markdown() -> String {
    let mut out = SKILL_INTRO.replace("{DESCRIPTION}", DESCRIPTION);
    for line in HOME_HELP {
        out.push_str(&format!("- {line}\n"));
    }
    out.push_str(SKILL_RULES);
    for (path, args, hint) in GIT_SPELLINGS {
        let prefix = format!("git{}", path.trim_start_matches("rgit"));
        let spelled: Vec<String> = args.iter().map(|a| format!("`{prefix} {a}`")).collect();
        out.push_str(&format!("- {}: {hint}\n", spelled.join(", ")));
    }
    out.push_str(SKILL_FOOTER);
    out
}

/// The skill's command reference, generated from the command tree and its
/// `--help` examples so it lists every command and never drifts from the CLI.
pub(crate) fn skill_reference() -> String {
    use clap::CommandFactory;
    let mut out = String::from(
        "# rgit command reference\n\nEvery command with what it does and example invocations. Run `rgit <command> --help` for every flag.\n",
    );
    let root = Cli::command();
    for (group, names) in SKILL_GROUPS {
        out.push_str(&format!("\n## {group}\n\n"));
        for name in *names {
            if let Some(cmd) = root.find_subcommand(name) {
                skill_entry(&mut out, cmd, name, 0);
            }
        }
    }
    out
}

fn skill_entry(out: &mut String, cmd: &clap::Command, path: &str, depth: usize) {
    let about = cmd.get_about().map(|a| a.to_string()).unwrap_or_default();
    let mut line = format!("{}- `{path}`", "  ".repeat(depth));
    let about = about.trim_end_matches('.');
    if !about.is_empty() {
        line.push_str(&format!(": {about}."));
    }
    if let Some(examples) = crate::examples::lookup(path) {
        let shown: Vec<String> = examples.iter().map(|e| format!("`{e}`")).collect();
        line.push_str(&format!(" e.g. {}", shown.join(", ")));
    }
    out.push_str(&line);
    out.push('\n');
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
        skill_entry(out, sub, &format!("{path} {}", sub.get_name()), depth + 1);
    }
}

pub fn run_skills(cmd: SkillsCmd) -> anyhow::Result<String> {
    match cmd {
        SkillsCmd::List => Ok("rgit\ntargets: agents, claude, all".to_owned()),
        SkillsCmd::Show { reference } => Ok(if reference {
            skill_reference()
        } else {
            skill_markdown()
        }
        .trim_end()
        .to_owned()),
        SkillsCmd::Install {
            project,
            user,
            target,
        } => install_skills(project || !user, user, target),
    }
}

fn install_skills(project: bool, user: bool, target: SkillTarget) -> anyhow::Result<String> {
    let mut installed = Vec::new();
    if project {
        install_target(&std::env::current_dir()?, target, &mut installed)?;
    }
    if user {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        install_target(&home, target, &mut installed)?;
    }
    Ok(installed.join("\n"))
}

fn install_target(
    root: &std::path::Path,
    target: SkillTarget,
    out: &mut Vec<String>,
) -> anyhow::Result<()> {
    match target {
        SkillTarget::Agents => write_skill(&root.join(".agents/skills/rgit"), out),
        SkillTarget::Claude => write_skill(&root.join(".claude/skills/rgit"), out),
        SkillTarget::All => {
            write_skill(&root.join(".agents/skills/rgit"), out)?;
            write_skill(&root.join(".claude/skills/rgit"), out)
        }
    }
}

/// The skill's files, relative to its directory.
fn skill_files() -> [(&'static str, String); 2] {
    [
        ("SKILL.md", skill_markdown()),
        ("references/commands.md", skill_reference()),
    ]
}

fn write_skill(dir: &std::path::Path, out: &mut Vec<String>) -> anyhow::Result<()> {
    for (name, contents) in skill_files() {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(&path).is_ok_and(|cur| cur == contents) {
            out.push(format!("unchanged {}", path.display()));
            continue;
        }
        std::fs::write(&path, contents)?;
        out.push(format!("installed {}", path.display()));
    }
    Ok(())
}

/// Parse a 1-based `START,END` line range (git's -L).
pub(crate) fn parse_line_range(spec: &str) -> anyhow::Result<(usize, usize)> {
    let bad = || CliError::usage(format!("-L wants START,END line numbers, got {spec:?}"));
    let (a, b) = spec.split_once(',').ok_or_else(bad)?;
    let start: usize = a.trim().parse().map_err(|_| bad())?;
    let end: usize = match b.trim().strip_prefix('+') {
        Some(count) => start + count.parse::<usize>().map_err(|_| bad())?.saturating_sub(1),
        None => b.trim().parse().map_err(|_| bad())?,
    };
    Ok((start, end))
}

/// Run a subcommand and return its compact output. When `interactive`, a
/// missing required argument is prompted for; otherwise it errors. `Mcp` is
/// handled by the caller (it takes over the process), so it is unreachable here.
pub fn run(
    backend: &Arc<dyn GitBackend>,
    command: Command,
    interactive: bool,
) -> anyhow::Result<String> {
    // On a terminal, let network ops prompt for a password/passphrase when the
    // agent and credential helpers cannot authenticate.
    if interactive {
        backend.set_credential_prompt(Box::new(crate::creds::TerminalPrompt));
    }
    // Resolve a possibly-missing string arg: use it, prompt for it, or error.
    let resolve = |value: Option<String>,
                   what: &str,
                   pick: &dyn Fn() -> anyhow::Result<String>|
     -> anyhow::Result<String> {
        match value {
            Some(v) => Ok(v),
            None if interactive => pick(),
            None => Err(CliError::usage(format!("{what} required"))),
        }
    };
    Ok(match command {
        Command::Index { action } => index_cmd(backend, action)?,
        Command::Skills { .. } | Command::Hooks { .. } => unreachable!("handled before dispatch"),
        Command::Status {
            untracked,
            ignored,
            paths,
            ..
        } => render::status(&status_view(
            backend,
            &paths,
            untracked.as_deref(),
            ignored,
        )?),
        Command::Log { format, .. } if format.any() => {
            let opts = log_options(backend, &command)?;
            let paths = if opts.follow {
                Vec::new()
            } else {
                opts.paths.clone()
            };
            let mut out = Vec::new();
            for e in backend.log(&opts)? {
                let mut text = render::log(std::slice::from_ref(&e));
                // git shows no diff for a merge unless asked for a combined one.
                if e.parents.len() <= 1 {
                    let files = backend.diff(&rgit_git::DiffSpec {
                        from: e.parents.first().cloned(),
                        to: Some(e.oid.clone()),
                        paths: paths.clone(),
                        ..Default::default()
                    })?;
                    if !files.is_empty() {
                        text.push('\n');
                        text.push_str(diff_out(&files, format).trim_end());
                    }
                }
                out.push(text);
            }
            if out.is_empty() {
                "no commits".to_owned()
            } else {
                out.join("\n\n")
            }
        }
        Command::Log { .. } => render::log(&backend.log(&log_options(backend, &command)?)?),
        Command::Diff { format, .. } => diff_out(&diff_files(backend, &command)?.0, format),
        Command::Show {
            revs,
            paths,
            format,
            no_patch,
        } => {
            let revs = if revs.is_empty() {
                vec!["HEAD".to_owned()]
            } else {
                revs
            };
            let mut out = Vec::new();
            for rev in &revs {
                out.push(show_one(backend, rev, &paths, format, no_patch)?);
            }
            out.join("\n\n")
        }
        Command::Blame { args, lines } => {
            let all = blame(backend, &args)?;
            let selected = match lines {
                Some(spec) => {
                    let (start, end) = parse_line_range(&spec)?;
                    let lo = start.saturating_sub(1);
                    all.into_iter()
                        .skip(lo)
                        .take(end.saturating_sub(lo))
                        .collect()
                }
                None => all,
            };
            render::blame(&selected)
        }
        Command::Refs => render::refs(&backend.refs()?),
        Command::Stage { paths, hunk, lines } => partial(
            &paths,
            hunk,
            &lines,
            |p| backend.stage_file(p),
            |p, h| backend.stage_hunk(p, h),
            |p, h, l| backend.stage_lines(p, h, l),
        )?,
        Command::Unstage { paths, hunk, lines } => partial(
            &paths,
            hunk,
            &lines,
            |p| backend.unstage_file(p),
            |p, h| backend.unstage_hunk(p, h),
            |p, h, l| backend.unstage_lines(p, h, l),
        )?,
        Command::StageAll => ok(backend.stage_all())?,
        Command::UnstageAll => ok(backend.unstage_all())?,
        Command::Discard { paths, hunk, lines } => {
            let paths = if paths.is_empty() {
                vec![resolve(None, "a path", &|| {
                    crate::interactive::pick_file(backend, "Discard which file?")
                })?]
            } else {
                paths
            };
            partial(
                &paths,
                hunk,
                &lines,
                |p| backend.discard_file(p),
                |p, h| backend.discard_hunk(p, h),
                |p, h, l| backend.discard_lines(p, h, l),
            )?
        }
        Command::Resolve { path, ours, theirs } => {
            if !ours && !theirs {
                anyhow::bail!("resolve needs --ours or --theirs");
            }
            ok(backend.resolve_conflict(&path, ours))?
        }
        Command::Commit {
            message,
            amend,
            all,
            no_verify,
        } => {
            let message = resolve(message, "a commit message", &|| {
                crate::interactive::input("Commit message")
            })?;
            // -a: stage worktree changes to tracked files (not untracked ones).
            if all {
                for e in backend.status()?.entries {
                    if matches!(
                        e.worktree,
                        rgit_git::StatusCode::Modified
                            | rgit_git::StatusCode::Deleted
                            | rgit_git::StatusCode::Renamed
                            | rgit_git::StatusCode::TypeChanged
                    ) {
                        backend.stage_file(&e.path)?;
                    }
                }
            }
            if amend {
                backend.amend(&message)?;
            } else if no_verify {
                backend.commit_no_verify(&message)?;
            } else {
                backend.commit(&message)?;
            }
            backend.commit_report().join("\n")
        }
        Command::Extend => {
            backend.commit_extend()?;
            backend.commit_report().join("\n")
        }
        Command::Reword { message, rev } => {
            backend.reword(&rev, &message)?;
            format!("reworded {rev}")
        }
        Command::Uncommit { n } => {
            backend.uncommit(n)?;
            format!("uncommitted {n} commit(s); changes kept staged")
        }
        Command::Squash { rev, from } => match from {
            Some(base) => {
                backend.squash_range(&base)?;
                format!("squashed everything after {base} into one commit")
            }
            None => {
                backend.squash(&rev)?;
                format!("squashed {rev} into its parent")
            }
        },
        Command::Split { rev, paths } => {
            backend.split(&rev, &paths)?;
            format!("split {rev} into two commits")
        }
        Command::Prev => {
            let current = backend
                .status()?
                .head
                .branch
                .ok_or_else(|| anyhow::anyhow!("not on a branch"))?;
            let parent = backend
                .stack_parents()?
                .into_iter()
                .find(|(b, _)| *b == current)
                .and_then(|(_, p)| p)
                .ok_or_else(|| anyhow::anyhow!("{current} has no stack parent"))?;
            backend.checkout_branch(&parent)?;
            format!("checked out {parent}")
        }
        Command::Next => {
            let current = backend
                .status()?
                .head
                .branch
                .ok_or_else(|| anyhow::anyhow!("not on a branch"))?;
            let children: Vec<String> = backend
                .stack_parents()?
                .into_iter()
                .filter(|(_, p)| p.as_deref() == Some(current.as_str()))
                .map(|(b, _)| b)
                .collect();
            match children.as_slice() {
                [] => anyhow::bail!("{current} is at the top of the stack"),
                [one] => {
                    backend.checkout_branch(one)?;
                    format!("checked out {one}")
                }
                many => anyhow::bail!("multiple children: {}", many.join(", ")),
            }
        }
        Command::Prune { dry_run } => {
            let out = backend.prune_objects(dry_run)?;
            if out.trim().is_empty() {
                "nothing to prune".to_owned()
            } else {
                out.trim_end().to_owned()
            }
        }
        Command::Move { rev, before, after } => match (before, after) {
            (Some(t), None) => {
                backend.reorder(&rev, &t, true)?;
                format!("moved {rev} before {t}")
            }
            (None, Some(t)) => {
                backend.reorder(&rev, &t, false)?;
                format!("moved {rev} after {t}")
            }
            _ => anyhow::bail!("pass exactly one of --before or --after"),
        },
        Command::Fetch {
            repository,
            refspecs,
            all,
            prune,
            remote,
            tags,
            depth,
            dry_run,
        } => {
            let (remote, refspecs) = remote_and_refspecs(remote, repository, refspecs);
            let args = rgit_git::FetchArgs {
                all,
                prune,
                tags,
                depth,
                dry_run,
            };
            net(interactive, "fetch", |r| {
                backend.fetch(remote.as_deref(), &refspecs, &args, r)
            })?
        }
        Command::Pull {
            repository,
            branch,
            rebase,
            no_rebase,
            ff_only,
        } => {
            let rebase = (rebase || no_rebase).then_some(rebase);
            net(interactive, "pull", |r| {
                backend.pull(repository.as_deref(), branch.as_deref(), rebase, ff_only, r)
            })?
        }
        Command::Submit => backend.submit_stack(&|_| {})?.join("\n"),
        Command::Sync => {
            let outcome = backend.sync(&|_| {})?;
            let mut msg = String::from("synced");
            if !outcome.restacked.is_empty() {
                msg.push_str(&format!("; restacked {}", outcome.restacked.join(", ")));
            }
            if !outcome.conflicted.is_empty() {
                msg.push_str(&format!("; conflicts in {}", outcome.conflicted.join(", ")));
            }
            msg
        }
        Command::Push {
            repository,
            refspecs,
            force,
            force_with_lease,
            set_upstream,
            remote,
            tags,
            all,
            delete,
            dry_run,
        } => {
            // `push --delete <branch>` (no remote named) deletes on the upstream.
            let (remote, mut refspecs) = match repository {
                Some(b)
                    if delete
                        && remote.is_none()
                        && refspecs.is_empty()
                        && !backend.remotes()?.iter().any(|r| r.name == b) =>
                {
                    (None, vec![b])
                }
                repository => remote_and_refspecs(remote, repository, refspecs),
            };
            if delete {
                if refspecs.is_empty() {
                    anyhow::bail!("--delete needs a branch to delete");
                }
                refspecs = refspecs.iter().map(|b| format!(":{b}")).collect();
            }
            let args = rgit_git::PushArgs {
                force,
                force_with_lease,
                set_upstream,
                all,
                tags,
                dry_run,
            };
            net(interactive, "push", |r| {
                backend.push_to(remote.as_deref(), &refspecs, &args, r)
            })?
        }
        Command::Checkout { rev, branch } => {
            // `-b <new>`: create the branch (from rev/HEAD) and switch to it.
            if let Some(new) = branch {
                if let Some(start) = &rev {
                    backend.checkout_detached(start)?;
                }
                backend.create_branch(&new)?;
                ok(backend.checkout_branch(&new))?
            } else {
                let rev = resolve(rev, "a branch or revision", &|| {
                    crate::interactive::pick_branch(backend, "Check out which branch?")
                })?;
                let is_branch = backend
                    .local_branches()
                    .map(|bs| bs.iter().any(|b| b == &rev))
                    .unwrap_or(false);
                ok(if is_branch {
                    backend.checkout_branch(&rev)
                } else {
                    backend.checkout_detached(&rev)
                })?
            }
        }
        Command::Merge {
            mut revs,
            no_ff,
            ff_only,
            squash,
            no_commit,
            message,
            strategy_option,
            no_edit: _,
            cont,
            abort,
        } => {
            if abort {
                ok(backend.merge_abort())?
            } else if cont {
                ok(backend.merge_continue())?
            } else {
                if revs.is_empty() {
                    revs.push(resolve(None, "a revision to merge", &|| {
                        crate::interactive::pick_branch(backend, "Merge which branch?")
                    })?);
                }
                let opts = rgit_git::MergeOptions {
                    no_ff,
                    ff_only,
                    squash,
                    no_commit,
                    message,
                    strategy_option,
                };
                net(interactive, "merge", |r| {
                    backend.merge_with(&revs, &opts, r)
                })?
            }
        }
        Command::Rebase {
            onto,
            onto_new,
            edit,
            root,
            autosquash,
            exec,
            update_refs,
            strategy_option,
            cont,
            skip,
            abort,
        } => {
            if abort {
                ok(backend.rebase_abort())?
            } else if cont {
                ok(backend.rebase_continue())?
            } else if skip {
                ok(backend.rebase_skip())?
            } else if edit
                || root
                || autosquash
                || update_refs
                || !exec.is_empty()
                || strategy_option.is_some()
            {
                if edit && !interactive {
                    anyhow::bail!("interactive rebase needs a terminal");
                }
                // Pick the base (how far back to edit) when it is not given.
                let onto = match onto {
                    None if edit && !root => Some(crate::interactive::pick_commit(
                        backend,
                        "Rebase onto which commit? (edits the commits after it)",
                    )?),
                    onto => onto,
                };
                let opts = rgit_git::RebaseOptions {
                    onto: onto_new,
                    interactive: edit,
                    root,
                    autosquash,
                    exec,
                    update_refs,
                    strategy_option,
                };
                ok(backend.rebase_with(onto.as_deref(), &opts))?
            } else if let Some(newbase) = onto_new {
                // `rebase --onto NEWBASE UPSTREAM`: replay UPSTREAM..HEAD onto NEWBASE.
                let upstream = resolve(onto, "the upstream (after --onto NEWBASE)", &|| {
                    crate::interactive::pick_branch(backend, "Replay commits after which upstream?")
                })?;
                net(interactive, "rebase", |r| {
                    backend.rebase_range(&upstream, &newbase, r)
                })?
            } else {
                let onto = resolve(
                    onto,
                    "a target revision, or --continue/--skip/--abort",
                    &|| crate::interactive::pick_branch(backend, "Rebase onto which branch?"),
                )?;
                net(interactive, "rebase", |r| backend.rebase_onto(&onto, r))?
            }
        }
        Command::Undo => format!("undid {}", backend.undo()?),
        Command::Redo => format!("redid {}", backend.redo()?),
        Command::Oplog => render::oplog(&backend.oplog()?),
        Command::Smartlog => render::smartlog(&backend.smartlog()?),
        Command::Bisect { args } => {
            let out = backend.bisect(&args)?;
            if out.is_empty() { "ok".to_owned() } else { out }
        }
        Command::Reset {
            rev,
            soft,
            hard,
            paths,
        } => {
            // `reset -- <paths>` unstages those paths (git's path-scoped reset).
            if !paths.is_empty() {
                for p in &paths {
                    backend.unstage_file(p)?;
                }
                return Ok(format!("unstaged {}", paths.join(", ")));
            }
            let rev = resolve(rev, "a revision to reset to", &|| {
                crate::interactive::pick_commit(backend, "Reset to which commit?")
            })?;
            let mode = match (soft, hard) {
                (true, _) => ResetMode::Soft,
                (_, true) => ResetMode::Hard,
                _ => ResetMode::Mixed,
            };
            if hard
                && interactive
                && !crate::interactive::confirm(
                    "Hard reset discards uncommitted changes. Continue?",
                )?
            {
                "cancelled".to_owned()
            } else {
                ok(backend.reset(&rev, mode))?
            }
        }
        Command::CherryPick {
            revs,
            no_commit,
            record_origin,
            mainline,
            strategy_option,
            no_edit: _,
            cont,
            skip,
            abort,
        } => {
            let opts = rgit_git::PickOptions {
                revert: false,
                no_commit,
                record_origin,
                mainline,
                strategy_option,
            };
            pick(backend, revs, &opts, (cont, skip, abort), interactive)?
        }
        Command::Revert {
            revs,
            no_commit,
            mainline,
            strategy_option,
            no_edit: _,
            cont,
            skip,
            abort,
        } => {
            let opts = rgit_git::PickOptions {
                revert: true,
                no_commit,
                record_origin: false,
                mainline,
                strategy_option,
            };
            pick(backend, revs, &opts, (cont, skip, abort), interactive)?
        }
        Command::Branch { cmd, all, remotes } => match cmd {
            None if remotes => backend.remote_branches()?.join("\n"),
            None => {
                let current = backend.status().ok().and_then(|s| s.head.branch);
                let mut names = backend.local_branches()?;
                if all {
                    names.extend(backend.remote_branches()?);
                }
                render::branches(&names, current.as_deref())
            }
            Some(BranchCmd::Create { name }) => ok(backend.create_branch(&name))?,
            Some(BranchCmd::Checkout { name }) => ok(backend.checkout_branch(&name))?,
            Some(BranchCmd::Delete { name, force }) => match name {
                Some(name) => ok(backend.delete_branch(&name, force))?,
                None if interactive => {
                    let names = crate::interactive::multiselect_branches(
                        backend,
                        "Delete which branches?",
                    )?;
                    if names.is_empty() {
                        "none selected".to_owned()
                    } else {
                        for n in &names {
                            backend.delete_branch(n, force)?;
                        }
                        format!("deleted {}", names.join(", "))
                    }
                }
                None => return Err(CliError::usage("a branch name required")),
            },
            Some(BranchCmd::Rename { old, new }) => ok(backend.rename_branch(&old, &new))?,
            Some(BranchCmd::Prune { base }) => {
                let deleted = backend.prune_merged(&base)?;
                if deleted.is_empty() {
                    format!("no branches merged into {base}")
                } else {
                    format!("deleted {} merged: {}", deleted.len(), deleted.join(", "))
                }
            }
        },
        Command::Stash { cmd } => match cmd {
            None => ok_msg(backend.stash_push(false))?,
            Some(StashCmd::Push {
                message: None,
                include_untracked,
            }) => ok_msg(backend.stash_push(include_untracked))?,
            Some(StashCmd::Push {
                message: Some(message),
                include_untracked,
            }) => ok_msg(backend.stash_push_message(&message, include_untracked))?,
            Some(StashCmd::Pop { index }) => ok(backend.stash_pop(stash_index(
                backend,
                index,
                interactive,
                "Pop which stash?",
            )?))?,
            Some(StashCmd::Apply { index }) => ok(backend.stash_apply(stash_index(
                backend,
                index,
                interactive,
                "Apply which stash?",
            )?))?,
            Some(StashCmd::Drop { index }) => ok(backend.stash_drop(stash_index(
                backend,
                index,
                interactive,
                "Drop which stash?",
            )?))?,
            Some(StashCmd::List) => render::stashes(&backend.status()?.stashes),
        },
        Command::Tag {
            name,
            message,
            annotate: _,
            force,
            delete,
            list: _,
        } => {
            if let Some(del) = delete {
                ok(backend.delete_tag(&del))?
            } else if let Some(name) = name {
                // `-f` re-tags: drop an existing tag of the same name first.
                if force {
                    let _ = backend.delete_tag(&name);
                }
                ok(backend.create_tag(&name, message.as_deref().unwrap_or("")))?
            } else {
                let names: Vec<String> = backend.all_tags()?.into_iter().map(|t| t.name).collect();
                if names.is_empty() {
                    "no tags".to_owned()
                } else {
                    names.join("\n")
                }
            }
        }
        Command::Remote { cmd } => match cmd {
            None => render::remotes(&backend.remotes()?),
            Some(RemoteCmd::Add { name, url }) => ok(backend.add_remote(&name, &url))?,
            Some(RemoteCmd::Remove { name }) => ok(backend.remove_remote(&name))?,
            Some(RemoteCmd::SetUrl { name, url }) => ok(backend.set_remote_url(&name, &url))?,
            Some(RemoteCmd::Rename { old, new }) => ok(backend.rename_remote(&old, &new))?,
        },
        Command::Flow { cmd } => match cmd {
            FlowCmd::Init { preset } => rgit_git::workflow::init(backend.as_ref(), &preset)?,
            FlowCmd::Start { name } => rgit_git::workflow::start(backend.as_ref(), &name)?,
            FlowCmd::Finish => rgit_git::workflow::finish(backend.as_ref())?,
            FlowCmd::Release { version, finish } => {
                rgit_git::workflow::release(backend.as_ref(), &version, finish)?
            }
            FlowCmd::Status => rgit_git::workflow::status(backend.as_ref())?,
        },
        Command::Lanes { cmd } => match cmd.unwrap_or(LanesCmd::List) {
            LanesCmd::Init => crate::lanes::init(backend)?,
            LanesCmd::Off => crate::lanes::off(backend)?,
            LanesCmd::List => crate::lanes::list(backend)?,
            LanesCmd::New { name } => crate::lanes::new_lane(backend, &name)?,
            LanesCmd::Stack { name, on } => crate::lanes::stack(backend, &name, &on)?,
            LanesCmd::Assign { lane, path, hunk } => match hunk {
                Some(new_start) => crate::lanes::assign_hunk(backend, &lane, &path, new_start)?,
                None => crate::lanes::assign(backend, &lane, &path)?,
            },
            LanesCmd::Unassign { path } => crate::lanes::unassign(backend, &path)?,
            LanesCmd::Commit { lane, message } => crate::lanes::commit(backend, &lane, &message)?,
            LanesCmd::Rename { old, new } => crate::lanes::rename(backend, &old, &new)?,
            LanesCmd::Delete { name } => crate::lanes::delete(backend, &name)?,
            LanesCmd::Push { lane } => crate::lanes::push(backend, &lane)?,
            LanesCmd::Pr { lane } => crate::lanes::pr(backend, &lane)?,
            LanesCmd::Restack => crate::lanes::restack(backend)?,
        },
        Command::Stack { cmd } => match cmd.unwrap_or(StackCmd::List) {
            StackCmd::New { name } => crate::stack::new(backend, &name)?,
            StackCmd::List => crate::stack::list(backend)?,
            StackCmd::Restack => crate::stack::restack(backend)?,
        },
        Command::Absorb => backend.absorb()?,
        Command::Workspace { cmd } => match cmd.unwrap_or(WorkspaceCmd::List) {
            WorkspaceCmd::New { name } => rgit_git::workspace::create(backend.as_ref(), &name)?,
            WorkspaceCmd::List => rgit_git::workspace::list(backend.as_ref())?,
            WorkspaceCmd::Remove { name } => rgit_git::workspace::remove(backend.as_ref(), &name)?,
        },
        Command::Worktree { cmd } => match cmd {
            None => render::worktrees(&backend.worktrees()?),
            Some(WorktreeCmd::Add { name, path }) => ok(backend.add_worktree(&name, &path))?,
            Some(WorktreeCmd::Remove { name, force }) => ok(backend.remove_worktree(&name, force))?,
            Some(WorktreeCmd::Prune) => {
                let pruned = backend.prune_worktrees()?;
                if pruned.is_empty() {
                    "nothing to prune".to_owned()
                } else {
                    format!("pruned {}", pruned.join(", "))
                }
            }
        },
        Command::Config {
            key,
            value,
            global,
            local,
            get,
            get_all,
            unset,
            unset_all,
            list,
            add,
            as_bool,
            as_int,
        } => {
            use rgit_git::ConfigScope;
            let scope = match (global, local) {
                (true, _) => ConfigScope::Global,
                (_, true) => ConfigScope::Local,
                _ => ConfigScope::Any,
            };
            let typed = |v: &str| rgit_git::config_value(v, as_bool, as_int);
            if list {
                return Ok(backend
                    .config_entries(scope, None)?
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("\n"));
            }
            let key = key.ok_or_else(|| CliError::usage("a config key required"))?;
            if unset || unset_all {
                backend.config_unset(scope, &key, unset_all)?;
                "ok".to_owned()
            } else if let Some(value) = value.filter(|_| !get && !get_all) {
                backend.config_write(scope, &key, &typed(&value)?, add)?;
                "ok".to_owned()
            } else {
                let mut values = backend.config_entries(scope, Some(&key))?;
                if values.is_empty() {
                    return Err(GitError::Other(format!("{key} is not set")).into());
                }
                if !get_all {
                    values.drain(..values.len() - 1);
                }
                values
                    .iter()
                    .map(|(_, v)| typed(v))
                    .collect::<Result<Vec<_>, _>>()?
                    .join("\n")
            }
        }
        Command::Apply {
            patches,
            cached,
            index,
            check,
            reverse,
            stat,
        } => {
            let patches = if patches.is_empty() {
                vec!["-".to_owned()]
            } else {
                patches
            };
            let mut stats = Vec::new();
            for p in &patches {
                let patch = read_input(p)?;
                if stat {
                    stats.push(backend.patch_stat(&patch)?);
                } else {
                    backend.apply_patch(&patch, cached, index, reverse, check)?;
                }
            }
            if stat {
                stats.join("\n")
            } else {
                "ok".to_owned()
            }
        }
        Command::Notes { notes_ref, cmd } => {
            let r = notes_ref.as_deref();
            let head = |rev: Option<String>| rev.unwrap_or_else(|| "HEAD".to_owned());
            match cmd.unwrap_or(NotesCmd::List { rev: None }) {
                NotesCmd::List { rev } => {
                    let notes = backend.notes(r)?;
                    match rev {
                        Some(rev) => {
                            let oid = backend.rev_parse(&rev)?;
                            notes
                                .into_iter()
                                .find(|(_, obj)| *obj == oid)
                                .map(|(note, _)| note)
                                .ok_or_else(|| {
                                    GitError::Other(format!("no note found for object {oid}"))
                                })?
                        }
                        None if notes.is_empty() => "no notes".to_owned(),
                        None => notes
                            .into_iter()
                            .map(|(note, obj)| format!("{note} {obj}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    }
                }
                NotesCmd::Show { rev } => backend.note_show(r, &head(rev))?,
                NotesCmd::Add {
                    rev,
                    message,
                    force,
                } => ok(backend.note_add(r, &head(rev), &message.join("\n\n"), force, false))?,
                NotesCmd::Append { rev, message } => {
                    ok(backend.note_add(r, &head(rev), &message.join("\n\n"), false, true))?
                }
                NotesCmd::Remove { rev } => ok(backend.note_remove(r, &head(rev)))?,
            }
        }
        Command::UpdateRef {
            name,
            new,
            old,
            delete,
            no_deref,
            message,
        } => {
            let (new, old) = if delete {
                if old.is_some() {
                    return Err(CliError::usage("-d takes a ref and an optional old value"));
                }
                (None, new)
            } else {
                let new = new.ok_or_else(|| CliError::usage("a new value required"))?;
                (Some(new), old)
            };
            ok(backend.update_ref(
                &name,
                new.as_deref(),
                old.as_deref(),
                no_deref,
                message.as_deref(),
            ))?
        }
        Command::HashObject {
            paths,
            write,
            stdin,
            kind,
        } => {
            let mut inputs = Vec::new();
            if stdin {
                inputs.push(read_input("-")?);
            }
            for p in &paths {
                inputs.push(read_input(p)?);
            }
            if inputs.is_empty() {
                return Err(CliError::usage("a file or --stdin required"));
            }
            inputs
                .iter()
                .map(|data| backend.hash_object(&kind, data, write))
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
        }
        Command::FormatPatch {
            revs,
            output_dir,
            stdout,
        } => {
            let (counts, ranges): (Vec<&String>, Vec<&String>) = revs.iter().partition(|r| {
                r.strip_prefix('-')
                    .is_some_and(|n| n.parse::<usize>().is_ok())
            });
            let count = counts.last().and_then(|n| n[1..].parse().ok());
            if ranges.len() > 1 || (ranges.is_empty() && count.is_none()) {
                return Err(CliError::usage(
                    "give `-<n>`, a `<since>` revision, or one `<a>..<b>` range",
                ));
            }
            let patches = backend.format_patch(ranges.first().map(|r| r.as_str()), count)?;
            if patches.is_empty() {
                return Ok("no commits to format".to_owned());
            }
            if stdout {
                return Ok(patches
                    .into_iter()
                    .map(|(_, email)| email)
                    .collect::<Vec<_>>()
                    .join("\n"));
            }
            let dir = PathBuf::from(output_dir.unwrap_or_default());
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(&dir)?;
            }
            let mut written = Vec::new();
            for (name, email) in patches {
                let path = dir.join(name);
                std::fs::write(&path, email)?;
                written.push(path.display().to_string());
            }
            written.join("\n")
        }
        Command::Am {
            mbox,
            abort,
            cont,
            skip,
            three_way,
            signoff,
        } => {
            let mut args: Vec<String> = Vec::new();
            args.extend(abort.then(|| "--abort".to_owned()));
            args.extend(cont.then(|| "--continue".to_owned()));
            args.extend(skip.then(|| "--skip".to_owned()));
            args.extend(three_way.then(|| "--3way".to_owned()));
            args.extend(signoff.then(|| "--signoff".to_owned()));
            let resume = abort || cont || skip;
            let input = if !resume && mbox.is_empty() {
                Some(read_input("-")?)
            } else {
                None
            };
            for m in &mbox {
                args.push(std::path::absolute(m)?.display().to_string());
            }
            let out = backend.am(&args, input.as_deref())?;
            if out.is_empty() { "ok".to_owned() } else { out }
        }
        Command::Archive {
            rev,
            paths,
            format,
            output,
            prefix,
        } => {
            let Some(output) = output.filter(|o| o != "-") else {
                return Err(CliError::usage("-o <file> required"));
            };
            let bytes = archive(backend, rev, &paths, format, Some(&output), prefix)?;
            std::fs::write(&output, bytes)?;
            format!("wrote {output}")
        }
        Command::Gc {
            prune,
            aggressive,
            auto,
        } => {
            let mut args: Vec<String> = Vec::new();
            args.extend(prune.map(|p| {
                if p.is_empty() {
                    "--prune".to_owned()
                } else {
                    format!("--prune={p}")
                }
            }));
            args.extend(aggressive.then(|| "--aggressive".to_owned()));
            args.extend(auto.then(|| "--auto".to_owned()));
            args.push("--quiet".to_owned());
            backend.gc(&args)?;
            "ok".to_owned()
        }
        Command::Fsck {
            full,
            strict,
            unreachable,
            no_dangling,
            connectivity_only,
        } => {
            let mut args: Vec<String> = Vec::new();
            for (on, flag) in [
                (full, "--full"),
                (strict, "--strict"),
                (unreachable, "--unreachable"),
                (no_dangling, "--no-dangling"),
                (connectivity_only, "--connectivity-only"),
            ] {
                args.extend(on.then(|| flag.to_owned()));
            }
            let out = backend.fsck(&args)?;
            if out.is_empty() {
                "no problems found".to_owned()
            } else {
                out
            }
        }
        Command::Clean {
            dry_run,
            ignored_too,
            only_ignored,
            exclude,
            paths,
            ..
        } => {
            let mut args: Vec<String> = Vec::new();
            args.extend(ignored_too.then(|| "-x".to_owned()));
            args.extend(only_ignored.then(|| "-X".to_owned()));
            for e in exclude {
                args.extend(["-e".to_owned(), e]);
            }
            if !paths.is_empty() {
                args.push("--".to_owned());
                args.extend(paths);
            }
            if dry_run {
                let out = backend.clean(true, &args)?;
                if out.trim().is_empty() {
                    "nothing to clean".to_owned()
                } else {
                    out.trim_end().to_owned()
                }
            } else if interactive
                && !crate::interactive::confirm("Remove all untracked files and directories?")?
            {
                "cancelled".to_owned()
            } else {
                backend.clean(false, &args)?;
                "ok".to_owned()
            }
        }
        Command::Rm {
            paths,
            cached,
            recursive,
            ..
        } => {
            let paths = if paths.is_empty() {
                vec![resolve(None, "a path", &|| {
                    crate::interactive::pick_file(backend, "Remove which file?")
                })?]
            } else {
                paths
            };
            for p in &paths {
                backend.remove_path(p, cached, recursive)?;
            }
            "ok".to_owned()
        }
        Command::Mv { paths, force } => {
            let (to, from) = paths.split_last().expect("clap requires two paths");
            if from.len() > 1 && !backend.workdir().join(to).is_dir() {
                anyhow::bail!("destination '{to}' is not a directory");
            }
            for f in from {
                backend.move_path(f, to, force)?;
            }
            "ok".to_owned()
        }
        Command::Describe {
            rev,
            tags,
            dirty,
            long,
            abbrev,
            always: _,
            pattern,
            exact_match,
        } => {
            let rev = rev.as_deref().unwrap_or("HEAD");
            if exact_match {
                let tag = backend.describe(rev, tags, false, false, Some(0), pattern.as_deref())?;
                let oid = backend.rev_parse(rev)?;
                if oid.starts_with(&tag) || backend.rev_parse(&tag).ok() != Some(oid) {
                    return Err(CliError {
                        message: format!("no tag exactly matches '{rev}'"),
                        help: Some("Run `rgit describe` for the nearest tag".to_owned()),
                        code: 128,
                    }
                    .into());
                }
                tag
            } else {
                backend.describe(rev, tags, dirty, long, abbrev, pattern.as_deref())?
            }
        }
        Command::Submodule { mut args } => {
            args.insert(0, "submodule".to_owned());
            backend.git(&args)?
        }
        Command::Git { args } => backend.git(&args)?,
        Command::Init { .. }
        | Command::Clone { .. }
        | Command::Mcp
        | Command::Serve { .. }
        | Command::Forge { .. } => unreachable!("handled before dispatch"),
    })
}

/// git's `<repository> [<refspec>...]`: the first positional names the remote,
/// unless `--remote` already did, in which case it is a refspec too.
fn remote_and_refspecs(
    remote: Option<String>,
    repository: Option<String>,
    refspecs: Vec<String>,
) -> (Option<String>, Vec<String>) {
    match remote {
        Some(r) => (Some(r), repository.into_iter().chain(refspecs).collect()),
        None => (repository, refspecs),
    }
}

/// Cherry-pick or revert `revs`, or continue/skip/abort a stopped sequence.
fn pick(
    backend: &Arc<dyn GitBackend>,
    mut revs: Vec<String>,
    opts: &rgit_git::PickOptions,
    (cont, skip, abort): (bool, bool, bool),
    interactive: bool,
) -> anyhow::Result<String> {
    if abort {
        return ok(backend.pick_abort());
    }
    if cont {
        return ok(backend.pick_continue());
    }
    if skip {
        return ok(backend.pick_skip());
    }
    if revs.is_empty() {
        if !interactive {
            return Err(CliError::usage("a commit required"));
        }
        let verb = if opts.revert { "Revert" } else { "Cherry-pick" };
        revs.push(crate::interactive::pick_commit(
            backend,
            &format!("{verb} which commit?"),
        )?);
    }
    ok(backend.pick(&revs, opts))
}

/// The bytes of a file, or of stdin for `-`.
fn read_input(path: &str) -> anyhow::Result<Vec<u8>> {
    if path == "-" {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
        Ok(buf)
    } else {
        std::fs::read(path).map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))
    }
}

/// `rgit archive` output; the format defaults from `output`'s extension.
pub fn archive(
    backend: &Arc<dyn GitBackend>,
    rev: Option<String>,
    paths: &[String],
    format: Option<String>,
    output: Option<&str>,
    prefix: Option<String>,
) -> anyhow::Result<Vec<u8>> {
    let format = format.unwrap_or_else(|| {
        let out = output.unwrap_or_default();
        if out.ends_with(".zip") {
            "zip"
        } else if out.ends_with(".tgz") || out.ends_with(".tar.gz") {
            "tgz"
        } else {
            "tar"
        }
        .to_owned()
    });
    Ok(backend.archive(
        rev.as_deref().unwrap_or("HEAD"),
        &format,
        prefix.as_deref().unwrap_or_default(),
        paths,
    )?)
}

/// Resolve a stash index: the given one, a prompt, or 0 (most recent).
fn stash_index(
    backend: &Arc<dyn GitBackend>,
    index: Option<usize>,
    interactive: bool,
    prompt: &str,
) -> anyhow::Result<usize> {
    match index {
        Some(i) => Ok(i),
        None if interactive => crate::interactive::pick_stash(backend, prompt),
        None => Ok(0),
    }
}

/// Status limited to `paths`, with untracked files per git's `-u` mode and
/// ignored files when asked.
pub(crate) fn status_view(
    backend: &Arc<dyn GitBackend>,
    paths: &[String],
    untracked: Option<&str>,
    ignored: bool,
) -> anyhow::Result<rgit_git::RepoStatus> {
    let mut s = backend.status()?;
    if untracked == Some("no") {
        let gone: Vec<String> = s
            .entries
            .iter()
            .filter(|e| e.is_untracked())
            .map(|e| e.path.clone())
            .collect();
        s.entries.retain(|e| !e.is_untracked());
        s.unstaged.retain(|d| !gone.contains(&d.path));
    }
    if ignored {
        s.entries.extend(backend.ignored()?);
    }
    if !paths.is_empty() {
        let keep = |p: &str| rgit_git::pathspec_matches(paths, p);
        s.entries.retain(|e| keep(&e.path));
        s.unstaged.retain(|d| keep(&d.path));
        s.staged.retain(|d| keep(&d.path));
    }
    Ok(s)
}

/// Rewrite path arguments typed in a subfolder of the repo into repo-root
/// paths, since git reads pathspecs relative to the current folder.
pub fn from_cwd(mut command: Command, workdir: &Path) -> Command {
    let Some((root, prefix)) = std::env::current_dir()
        .and_then(|cwd| cwd.canonicalize())
        .ok()
        .zip(workdir.canonicalize().ok())
        .and_then(|(cwd, root)| {
            let prefix = cwd.strip_prefix(&root).ok()?.to_path_buf();
            Some((root, prefix))
        })
    else {
        return command;
    };
    let fix = |p: &mut String| *p = repo_path(&root, &prefix, p);
    match &mut command {
        Command::Status { paths, .. }
        | Command::Stage { paths, .. }
        | Command::Unstage { paths, .. }
        | Command::Discard { paths, .. }
        | Command::Split { paths, .. }
        | Command::Reset { paths, .. }
        | Command::Archive { paths, .. }
        | Command::Clean { paths, .. }
        | Command::Rm { paths, .. }
        | Command::Mv { paths, .. } => paths.iter_mut().for_each(fix),
        Command::Resolve { path, .. } => fix(path),
        Command::Blame { args, .. } => args.last_mut().into_iter().for_each(fix),
        Command::Show { paths, .. } => paths.iter_mut().for_each(fix),
        Command::Log { revs, paths, .. } | Command::Diff { revs, paths, .. } => {
            // A positional that names a file here is a path, not a revision.
            revs.iter_mut()
                .filter(|r| Path::new(r.as_str()).exists())
                .for_each(fix);
            paths.iter_mut().for_each(fix);
        }
        _ => {}
    }
    command
}

/// `p` typed in the repo subfolder `prefix`, as a path from the repo `root`.
/// `:/x` names `x` at the root, as in git.
fn repo_path(root: &Path, prefix: &Path, p: &str) -> String {
    if let Some(top) = p.strip_prefix(":/") {
        return top.to_owned();
    }
    let full = prefix.join(p);
    let rel = if full.is_absolute() {
        match full.strip_prefix(root) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => return p.to_owned(),
        }
    } else {
        full
    };
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        match c {
            std::path::Component::Normal(n) => parts.push(n.to_string_lossy().into_owned()),
            std::path::Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

/// Run a stage/unstage/discard on whole paths, on each hunk of one path, or
/// on some lines of one hunk. Hunks go last first, so changing one never moves
/// the start line of a hunk before it.
fn partial(
    paths: &[String],
    mut hunks: Vec<u32>,
    lines: &[usize],
    file: impl Fn(&str) -> Result<(), GitError>,
    hunk: impl Fn(&str, u32) -> Result<(), GitError>,
    some: impl Fn(&str, u32, &[usize]) -> Result<(), GitError>,
) -> anyhow::Result<String> {
    if hunks.is_empty() {
        for p in paths {
            file(p)?;
        }
        return Ok("ok".to_owned());
    }
    let [path] = paths else {
        anyhow::bail!("--hunk needs exactly one path");
    };
    if !lines.is_empty() {
        let [h] = hunks[..] else {
            anyhow::bail!("--lines needs exactly one --hunk");
        };
        some(path, h, lines)?;
        return Ok("ok".to_owned());
    }
    hunks.sort_unstable_by(|a, b| b.cmp(a));
    hunks.dedup();
    for h in hunks {
        hunk(path, h)?;
    }
    Ok("ok".to_owned())
}

fn ok(r: Result<(), GitError>) -> anyhow::Result<String> {
    r.map(|()| "ok".to_owned()).map_err(Into::into)
}

/// Like [`ok`], but prints the operation's own success line instead of "ok".
fn ok_msg(r: Result<String, GitError>) -> anyhow::Result<String> {
    r.map_err(Into::into)
}

pub(crate) fn diff_out(files: &[rgit_git::FileDiff], format: DiffFormat) -> String {
    let rows = |row: &dyn Fn(&rgit_git::FileDiff) -> String| {
        files.iter().map(row).collect::<Vec<_>>().join("\n")
    };
    let path = |f: &rgit_git::FileDiff| match &f.old_path {
        Some(old) => format!("{old}\t{}", f.path),
        None => f.path.clone(),
    };
    if format.name_only {
        rows(&|f| f.path.clone())
    } else if format.name_status {
        rows(&|f| {
            let mut code = f.status.letter().to_owned();
            // git prints a similarity score; an unchanged move is 100%.
            if f.old_path.is_some() && f.hunks.is_empty() {
                code.push_str("100");
            }
            format!("{code}\t{}", path(f))
        })
    } else if format.numstat {
        rows(&|f| {
            let (add, del) = crate::axi::line_counts(f);
            let path = match &f.old_path {
                Some(old) => format!("{old} => {}", f.path),
                None => f.path.clone(),
            };
            if f.binary {
                format!("-\t-\t{path}")
            } else {
                format!("{add}\t{del}\t{path}")
            }
        })
    } else if format.patch && format.stat {
        format!("{}\n\n{}", render::diffstat(files), render::patch(files))
    } else if format.patch {
        render::patch(files)
    } else {
        render::diffstat(files)
    }
}

/// The log walk a `rgit log` command asks for.
pub(crate) fn log_options(
    backend: &Arc<dyn GitBackend>,
    command: &Command,
) -> anyhow::Result<LogOptions> {
    let Command::Log {
        limit,
        skip,
        all,
        author,
        since,
        until,
        grep,
        ignore_case,
        first_parent,
        merges,
        no_merges,
        reverse,
        follow,
        revs,
        paths,
        ..
    } = command
    else {
        anyhow::bail!("not a log command");
    };
    let (revs, paths) = split_revs(backend, revs, paths)?;
    Ok(LogOptions {
        limit: *limit,
        offset: *skip,
        all: *all,
        author: author.clone(),
        revs,
        since: since.as_deref().map(parse_date).transpose()?,
        until: until.as_deref().map(parse_date).transpose()?,
        paths,
        grep: grep.clone(),
        grep_ignore_case: *ignore_case,
        first_parent: *first_parent,
        merges: (*merges || *no_merges).then_some(*merges),
        reverse: *reverse,
        follow: *follow,
    })
}

/// The files a `rgit diff` command compares, and a name for that scope.
pub(crate) fn diff_files(
    backend: &Arc<dyn GitBackend>,
    command: &Command,
) -> anyhow::Result<(Vec<rgit_git::FileDiff>, String)> {
    let Command::Diff {
        revs,
        paths,
        cached,
        unified,
        ignore_all_space,
        ignore_space_change,
        ..
    } = command
    else {
        anyhow::bail!("not a diff command");
    };
    let (revs, paths) = split_revs(backend, revs, paths)?;
    let side = if *cached { "index" } else { "working tree" };
    let (from, to, scope) = match &revs[..] {
        [] if *cached => (None, None, "staged".to_owned()),
        [] => (None, None, "unstaged".to_owned()),
        [range] if range.contains("..") => (Some(range.clone()), None, range.clone()),
        [rev] => (Some(rev.clone()), None, format!("{rev}..{side}")),
        [from, to] => (
            Some(from.clone()),
            Some(to.clone()),
            format!("{from}..{to}"),
        ),
        _ => return Err(CliError::usage("diff takes at most two revisions")),
    };
    let files = backend.diff(&rgit_git::DiffSpec {
        from,
        to,
        cached: *cached,
        paths,
        context: *unified,
        ignore_all_space: *ignore_all_space,
        ignore_space_change: *ignore_space_change,
    })?;
    Ok((files, scope))
}

/// Split `log`/`diff` arguments into revisions and paths as git does: the
/// leading ones that name revisions, then paths (and everything after `--`).
fn split_revs(
    backend: &Arc<dyn GitBackend>,
    args: &[String],
    after: &[String],
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let is_rev = |arg: &str| {
        let arg = arg.strip_prefix('^').unwrap_or(arg);
        let (a, b) = arg
            .split_once("...")
            .or_else(|| arg.split_once(".."))
            .unwrap_or((arg, ""));
        [a, b]
            .iter()
            .all(|s| s.is_empty() || backend.rev_parse(s).is_ok())
    };
    let n = args.iter().take_while(|a| is_rev(a)).count();
    let mut paths = args[n..].to_vec();
    if let Some(p) = paths
        .iter()
        .find(|p| !p.contains(['*', '?', '[']) && !backend.workdir().join(p.as_str()).exists())
    {
        return Err(CliError {
            message: format!(
                "ambiguous argument '{p}': unknown revision or path not in the working tree"
            ),
            help: Some("Put paths after `--`, e.g. `rgit log -- <path>`".to_owned()),
            code: 128,
        }
        .into());
    }
    paths.extend_from_slice(after);
    Ok((args[..n].to_vec(), paths))
}

/// One `rgit show` argument: a commit, or `rev:path` for a file or folder.
fn show_one(
    backend: &Arc<dyn GitBackend>,
    rev: &str,
    paths: &[String],
    format: DiffFormat,
    no_patch: bool,
) -> anyhow::Result<String> {
    if let Some((commit, path)) = rev.split_once(':') {
        return match backend.read_blob(commit, path) {
            Ok(blob) => Ok(render::blob(&blob)),
            Err(e) => {
                let Ok(mut entries) = backend.list_tree(commit, path) else {
                    return Err(e.into());
                };
                let key = |e: &rgit_git::TreeEntry| {
                    format!("{}{}", e.name, if e.is_dir { "/" } else { "" })
                };
                entries.sort_by_key(key);
                let names: Vec<String> = entries.iter().map(key).collect();
                Ok(format!("tree {rev}\n\n{}", names.join("\n")))
            }
        };
    }
    let mut details = backend.commit_details(rev)?;
    if !paths.is_empty() {
        details
            .files
            .retain(|f| rgit_git::pathspec_matches(paths, &f.path));
    }
    if no_patch {
        details.files.clear();
    }
    let stat_only = !(format.patch || format.name_only || format.name_status || format.numstat);
    Ok(if stat_only || no_patch {
        render::commit_details(&details)
    } else {
        diff_out(&details.files, format)
    })
}

/// Blame `[rev] path`.
pub(crate) fn blame(
    backend: &Arc<dyn GitBackend>,
    args: &[String],
) -> Result<Vec<rgit_git::BlameLine>, GitError> {
    match args {
        [rev, path] => backend.blame_at(rev, path),
        [path, ..] => backend.blame(path),
        [] => Ok(Vec::new()),
    }
}

/// Run a network/merge/rebase op, collecting its git-style report lines as the
/// output. On a terminal a spinner runs for the duration.
fn net(
    interactive: bool,
    title: &str,
    run: impl FnOnce(&dyn Fn(OpProgress)) -> Result<(), GitError>,
) -> anyhow::Result<String> {
    let lines = Mutex::new(Vec::new());
    let result = {
        if interactive {
            let spinner = crate::prompt::spinner(&format!("{title}\u{2026}"));
            let report = |p: OpProgress| match p {
                OpProgress::Line(s) => {
                    spinner.set_message(&s);
                    lines.lock().expect("report mutex").push(s);
                }
                OpProgress::Transfer { received, total } => {
                    spinner.set_message(format!("{title}: {received}/{total} objects"));
                }
            };
            let result = run(&report);
            match &result {
                Ok(()) => spinner.stop_ok(format!("{title} done")),
                Err(_) => spinner.stop_err(format!("{title} failed")),
            }
            result
        } else {
            let report = |p: OpProgress| {
                if let OpProgress::Line(s) = p {
                    lines.lock().expect("report mutex").push(s);
                }
            };
            run(&report)
        }
    };
    result?;
    let lines = lines.into_inner().expect("report mutex");
    Ok(if lines.is_empty() {
        "ok".to_owned()
    } else {
        lines.join("\n")
    })
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        super::Cli::command().debug_assert();
    }

    #[test]
    fn every_command_is_in_a_skill_group() {
        let grouped: Vec<&str> = super::SKILL_GROUPS
            .iter()
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        for cmd in super::Cli::command().get_subcommands() {
            let name = cmd.get_name();
            assert!(
                grouped.iter().filter(|g| **g == name).count() == 1,
                "{name} must appear in exactly one SKILL_GROUPS entry"
            );
        }
    }

    #[test]
    fn committed_skill_matches_generated() {
        assert_eq!(
            include_str!("../../../skills/rgit/SKILL.md"),
            super::skill_markdown(),
            "skills/rgit/SKILL.md is stale; run `rgit --human skills show > skills/rgit/SKILL.md`"
        );
        assert_eq!(
            include_str!("../../../skills/rgit/references/commands.md"),
            super::skill_reference(),
            "skills/rgit/references/commands.md is stale; run \
             `rgit --human skills show --reference > skills/rgit/references/commands.md`"
        );
    }
}
