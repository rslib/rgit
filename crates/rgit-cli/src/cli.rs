//! The command-line surface. With no subcommand rgit launches the TUI; each
//! subcommand drives the same `GitBackend` and prints compact, agent-friendly output.
//!
//! When a required argument is missing and stdout is a real terminal, the
//! missing value is prompted for (our own widgets); with `--no-input` or a non-TTY
//! (an agent, a pipe, CI) it errors instead, so scripted use stays predictable.

use std::path::PathBuf;
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
        /// Walk every ref, not just HEAD.
        #[arg(long)]
        all: bool,
        /// Keep only commits whose author name/email contains this.
        #[arg(long)]
        author: Option<String>,
        /// Only commits at or after this date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS).
        #[arg(long)]
        since: Option<String>,
        /// Only commits at or before this date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS).
        #[arg(long)]
        until: Option<String>,
        /// Accepted for git compatibility (rgit's log is already one line each).
        #[arg(long)]
        oneline: bool,
        /// Start from this revision instead of HEAD, and/or limit to a path:
        /// `rgit log <rev>` or `rgit log -- <path>` or `rgit log <rev> -- <path>`.
        #[arg(value_name = "REV_OR_PATH")]
        rev: Option<String>,
        /// Limit to commits touching this path (after `--`).
        #[arg(last = true, value_name = "PATH")]
        path: Option<String>,
    },
    /// Diffstat of unstaged changes (`--cached` for staged), or between two revisions.
    Diff {
        /// Diff FROM..TO; omit both to diff the working tree.
        from: Option<String>,
        /// The second revision (defaults to HEAD when only FROM is given).
        to: Option<String>,
        /// Print the full unified patch instead of a diffstat.
        #[arg(short, long)]
        patch: bool,
        /// Diff the staged changes (index vs HEAD), like git's --cached.
        #[arg(long, visible_alias = "staged")]
        cached: bool,
        /// List only the names of changed files.
        #[arg(long = "name-only")]
        name_only: bool,
        /// Show a diffstat (the default when neither --patch nor --name-only).
        #[arg(long)]
        stat: bool,
    },
    /// A commit's header and diffstat.
    Show {
        /// The commit to show (branch, tag, or sha).
        rev: String,
        /// Show the full unified patch (git's -p), not just the diffstat.
        #[arg(short, long)]
        patch: bool,
        /// List only the names of the files the commit changed.
        #[arg(long = "name-only")]
        name_only: bool,
    },
    /// Blame a file: `sha author line` per line.
    Blame {
        /// The file to annotate.
        path: String,
        /// Limit to a 1-based line range `START,END` (git's -L).
        #[arg(short = 'L', value_name = "START,END")]
        lines: Option<String>,
    },
    /// All refs (local branches, remotes, tags).
    Refs,
    /// Stage a path, or one hunk / specific lines of it.
    Stage {
        /// The path to stage.
        path: String,
        /// Stage only the hunk at this new-side start line.
        #[arg(long)]
        hunk: Option<u32>,
        /// Stage only these line indices within --hunk (comma-separated).
        #[arg(long, value_delimiter = ',', requires = "hunk")]
        lines: Vec<usize>,
    },
    /// Unstage a path, or one hunk / specific lines of it.
    Unstage {
        /// The path to unstage.
        path: String,
        /// Unstage only the hunk at this new-side start line.
        #[arg(long)]
        hunk: Option<u32>,
        /// Unstage only these line indices within --hunk (comma-separated).
        #[arg(long, value_delimiter = ',', requires = "hunk")]
        lines: Vec<usize>,
    },
    /// Stage every change.
    StageAll,
    /// Unstage everything.
    UnstageAll,
    /// Discard a path's unstaged changes, or one hunk / specific lines.
    Discard {
        /// The path to discard (prompted for if omitted on a terminal).
        path: Option<String>,
        /// Discard only the hunk at this new-side start line.
        #[arg(long)]
        hunk: Option<u32>,
        /// Discard only these line indices within --hunk (comma-separated).
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
    /// Fetch the current branch's remote.
    Fetch {
        /// Fetch from every remote (git's --all).
        #[arg(long)]
        all: bool,
        /// Delete remote-tracking refs that no longer exist upstream (--prune).
        #[arg(short = 'p', long)]
        prune: bool,
        /// Fetch this named remote instead of the branch's upstream.
        #[arg(long)]
        remote: Option<String>,
    },
    /// Fetch and integrate the current branch's upstream.
    Pull {
        /// Rebase local commits onto the upstream instead of fast-forwarding.
        #[arg(short = 'r', long)]
        rebase: bool,
    },
    /// Fetch, fast-forward branches to their upstreams, and restack the stack.
    Sync,
    /// Push every branch in the stack and open a pull request per branch.
    Submit,
    /// Push the current branch to its upstream.
    Push {
        /// Overwrite the remote branch unconditionally (dangerous).
        #[arg(long, conflicts_with = "force_with_lease")]
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
        /// Delete this branch on the remote (git's --delete).
        #[arg(long, value_name = "BRANCH")]
        delete: Option<String>,
    },
    /// Check out a branch or, for any other revision, a detached HEAD.
    Checkout {
        /// The branch or revision (prompted for if omitted on a terminal).
        rev: Option<String>,
        /// Create a new branch and switch to it (git's -b), from `rev` or HEAD.
        #[arg(short = 'b', value_name = "NEW_BRANCH")]
        branch: Option<String>,
    },
    /// Merge a revision into the current branch.
    Merge {
        /// The branch or revision to merge (prompted for if omitted).
        rev: Option<String>,
        /// Always create a merge commit, even if a fast-forward is possible.
        #[arg(long = "no-ff", conflicts_with = "ff_only")]
        no_ff: bool,
        /// Refuse to merge unless it can fast-forward (git's --ff-only).
        #[arg(long = "ff-only")]
        ff_only: bool,
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
    /// Cherry-pick a commit onto HEAD.
    CherryPick {
        /// The commit to cherry-pick (prompted for if omitted on a terminal).
        rev: Option<String>,
        /// Apply the change without committing (git's -n/--no-commit).
        #[arg(short = 'n', long = "no-commit")]
        no_commit: bool,
    },
    /// Revert a commit on HEAD.
    Revert {
        /// The commit to revert (prompted for if omitted on a terminal).
        rev: Option<String>,
        /// Apply the inverse without committing (git's -n/--no-commit).
        #[arg(short = 'n', long = "no-commit")]
        no_commit: bool,
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
    /// Remove all untracked files and directories.
    Clean {
        /// List what would be removed without deleting (git's -n).
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
    },
    /// Remove a tracked path from the index and working tree.
    Rm {
        /// The path to remove (prompted for if omitted on a terminal).
        path: Option<String>,
        /// Remove only from the index, keeping the working-tree file (--cached).
        #[arg(long)]
        cached: bool,
    },
    /// Rename/move a tracked path.
    Mv {
        /// The current path.
        from: String,
        /// The new path.
        to: String,
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
        /// Number of hex digits for the abbreviated commit oid.
        #[arg(long, value_name = "N")]
        abbrev: Option<u32>,
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
    /// List remote branches.
    List {
        /// Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote.
        target: Option<String>,
        repo: Option<String>,
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
    /// List open pull requests or merge requests.
    List {
        /// Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote.
        target: Option<String>,
        repo: Option<String>,
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
        "rgit log",
        &["-p", "--patch"],
        "use `rgit show <id> --patch` for one commit's patch",
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
    let end: usize = b.trim().parse().map_err(|_| bad())?;
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
        Command::Status { .. } => render::status(&backend.status()?),
        Command::Log {
            limit,
            all,
            author,
            since,
            until,
            oneline: _,
            rev,
            path,
        } => {
            let since = since.as_deref().map(parse_date).transpose()?;
            let until = until.as_deref().map(parse_date).transpose()?;
            render::log(&backend.log(&LogOptions {
                limit,
                all,
                author,
                rev,
                since,
                until,
                path,
                ..LogOptions::default()
            })?)
        }
        Command::Diff {
            from,
            to,
            patch,
            cached,
            name_only,
            stat: _,
        } => {
            // Bare `diff` shows the unstaged (worktree vs index) changes, like
            // git; `--cached` shows the staged (index vs HEAD) changes.
            let files = match (from, to) {
                (Some(from), Some(to)) => backend.diff_refs(&from, &to)?,
                (Some(rev), None) => backend.diff_refs(&rev, "HEAD")?,
                (None, _) if cached => backend.status()?.staged,
                (None, _) => backend.status()?.unstaged,
            };
            diff_out(&files, patch, name_only)
        }
        Command::Show {
            rev,
            patch,
            name_only,
        } => {
            let details = backend.commit_details(&rev)?;
            if name_only {
                details
                    .files
                    .iter()
                    .map(|f| f.path.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else if patch {
                render::patch(&details.files)
            } else {
                render::commit_details(&details)
            }
        }
        Command::Blame { path, lines } => {
            let all = backend.blame(&path)?;
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
        Command::Stage { path, hunk, lines } => ok(match (hunk, lines.as_slice()) {
            (Some(h), l) if !l.is_empty() => backend.stage_lines(&path, h, l),
            (Some(h), _) => backend.stage_hunk(&path, h),
            (None, _) => backend.stage_file(&path),
        })?,
        Command::Unstage { path, hunk, lines } => ok(match (hunk, lines.as_slice()) {
            (Some(h), l) if !l.is_empty() => backend.unstage_lines(&path, h, l),
            (Some(h), _) => backend.unstage_hunk(&path, h),
            (None, _) => backend.unstage_file(&path),
        })?,
        Command::StageAll => ok(backend.stage_all())?,
        Command::UnstageAll => ok(backend.unstage_all())?,
        Command::Discard { path, hunk, lines } => {
            let path = resolve(path, "a path", &|| {
                crate::interactive::pick_file(backend, "Discard which file?")
            })?;
            ok(match (hunk, lines.as_slice()) {
                (Some(h), l) if !l.is_empty() => backend.discard_lines(&path, h, l),
                (Some(h), _) => backend.discard_hunk(&path, h),
                (None, _) => backend.discard_file(&path),
            })?
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
        Command::Fetch { all, prune, remote } => net(interactive, "fetch", |r| {
            backend.fetch(remote.as_deref(), all, prune, r)
        })?,
        Command::Pull { rebase } => net(interactive, "pull", |r| backend.pull(rebase, r))?,
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
            force,
            force_with_lease,
            set_upstream,
            remote,
            tags,
            delete,
        } => net(interactive, "push", |r| {
            if let Some(branch) = &delete {
                backend.push_delete(remote.as_deref(), branch, r)
            } else if tags {
                backend.push_tags(remote.as_deref(), r)
            } else {
                backend.push(remote.as_deref(), force, force_with_lease, set_upstream, r)
            }
        })?,
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
            rev,
            no_ff,
            ff_only,
            abort,
        } => {
            if abort {
                ok(backend.merge_abort())?
            } else {
                let rev = resolve(rev, "a revision to merge", &|| {
                    crate::interactive::pick_branch(backend, "Merge which branch?")
                })?;
                net(interactive, "merge", |r| {
                    backend.merge(&rev, no_ff, ff_only, r)
                })?
            }
        }
        Command::Rebase {
            onto,
            onto_new,
            edit,
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
            } else if let Some(newbase) = onto_new {
                // `rebase --onto NEWBASE UPSTREAM`: replay UPSTREAM..HEAD onto NEWBASE.
                let upstream = resolve(onto, "the upstream (after --onto NEWBASE)", &|| {
                    crate::interactive::pick_branch(backend, "Replay commits after which upstream?")
                })?;
                net(interactive, "rebase", |r| {
                    backend.rebase_range(&upstream, &newbase, r)
                })?
            } else if edit {
                if !interactive {
                    anyhow::bail!("interactive rebase needs a terminal");
                }
                // Pick the base (how far back to edit) when it is not given.
                let onto = match onto {
                    Some(o) => o,
                    None => crate::interactive::pick_commit(
                        backend,
                        "Rebase onto which commit? (edits the commits after it)",
                    )?,
                };
                ok(backend.rebase_interactive(Some(&onto)))?
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
        Command::CherryPick { rev, no_commit } => {
            let rev = resolve(rev, "a commit to cherry-pick", &|| {
                crate::interactive::pick_commit(backend, "Cherry-pick which commit?")
            })?;
            ok(backend.cherry_pick(&rev, no_commit))?
        }
        Command::Revert { rev, no_commit } => {
            let rev = resolve(rev, "a commit to revert", &|| {
                crate::interactive::pick_commit(backend, "Revert which commit?")
            })?;
            ok(backend.revert(&rev, no_commit))?
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
        Command::Clean { dry_run } => {
            if dry_run {
                let out = backend.clean(true)?;
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
                backend.clean(false)?;
                "ok".to_owned()
            }
        }
        Command::Rm { path, cached } => {
            let path = resolve(path, "a path", &|| {
                crate::interactive::pick_file(backend, "Remove which file?")
            })?;
            ok(backend.remove_path(&path, cached))?
        }
        Command::Mv { from, to, force } => ok(backend.move_path(&from, &to, force))?,
        Command::Describe {
            rev,
            tags,
            dirty,
            long,
            abbrev,
        } => backend.describe(rev.as_deref().unwrap_or("HEAD"), tags, dirty, long, abbrev)?,
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

fn ok(r: Result<(), GitError>) -> anyhow::Result<String> {
    r.map(|()| "ok".to_owned()).map_err(Into::into)
}

/// Like [`ok`], but prints the operation's own success line instead of "ok".
fn ok_msg(r: Result<String, GitError>) -> anyhow::Result<String> {
    r.map_err(Into::into)
}

fn diff_out(files: &[rgit_git::FileDiff], patch: bool, name_only: bool) -> String {
    if name_only {
        files
            .iter()
            .map(|f| f.path.clone())
            .collect::<Vec<_>>()
            .join("\n")
    } else if patch {
        render::patch(files)
    } else {
        render::diffstat(files)
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
