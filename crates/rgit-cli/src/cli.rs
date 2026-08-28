//! The command-line surface. With no subcommand rgit launches the TUI; each
//! subcommand drives the same `GitBackend` and prints compact, agent-friendly output.
//!
//! When a required argument is missing and stdout is a real terminal, the
//! missing value is prompted for (our own widgets); with `--no-input` or a non-TTY
//! (an agent, a pipe, CI) it errors instead, so scripted use stays predictable.

use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
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
}

#[derive(Subcommand)]
pub enum Command {
    /// Compact working-tree status.
    Status,
    /// Recent commits as `sha subject` lines.
    Log {
        /// Maximum number of commits to show (git's -n).
        #[arg(short = 'n', short_alias = 'l', long = "max-count", visible_alias = "limit", default_value_t = 20)]
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
    /// Diffstat of the staged changes, or between two revisions.
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
    /// Delete local branches already merged into a base (default HEAD).
    Prune {
        /// Delete branches merged into this revision.
        #[arg(default_value = "HEAD")]
        base: String,
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
        /// The branch or revision to rebase onto (prompted for if omitted).
        onto: Option<String>,
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
    },
    /// Cherry-pick a commit onto HEAD.
    CherryPick {
        /// The commit to cherry-pick (prompted for if omitted on a terminal).
        rev: Option<String>,
    },
    /// Revert a commit on HEAD.
    Revert {
        /// The commit to revert (prompted for if omitted on a terminal).
        rev: Option<String>,
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
    /// Tag management.
    Tag {
        #[command(subcommand)]
        cmd: Option<TagCmd>,
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
pub enum TagCmd {
    /// List all tags, newest first (the default when `tag` has no subcommand).
    List,
    /// Create a tag (annotated when a message is given).
    Create {
        /// The tag name.
        name: String,
        /// An annotation message.
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Delete a tag (prompted for if no name on a terminal).
    Delete {
        /// The tag to delete.
        name: Option<String>,
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
    },
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
                Some(index) => Ok(format!("indexed: {} chunks ({})", index.len(), path.display())),
                None => Ok(format!("no index ({}); run `rgit index build`", path.display())),
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
                if p.is_dir() {
                    if let Ok(b) = rgit_git::Git2Backend::discover(&p) {
                        out.push((repo_label(&p), Arc::new(b)));
                    }
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
struct Hit {
    score: f64,
    repo: String,
    path: String,
    line: usize,
    tag: &'static str,
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
pub(crate) fn code_search(
    backend: &Arc<dyn GitBackend>,
    root: Option<&str>,
    query: &str,
    limit: usize,
) -> anyhow::Result<String> {
    let targets = repo_targets(root, backend)?;
    let multi = targets.len() > 1;
    let pool = (limit * 3).max(20);
    let mut hits: Vec<Hit> = Vec::new();
    for (label, b) in &targets {
        hits.extend(hybrid_hits(b, label, query, pool));
    }
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);
    if hits.is_empty() {
        return Ok("no matches".to_owned());
    }
    let mut out = String::new();
    for h in hits {
        if multi {
            out.push_str(&format!("{:.4}  {}/{}:{}  [{}]\n", h.score, h.repo, h.path, h.line, h.tag));
        } else {
            out.push_str(&format!("{:.4}  {}:{}  [{}]\n", h.score, h.path, h.line, h.tag));
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
pub(crate) fn semantic_search(
    backend: &Arc<dyn GitBackend>,
    root: Option<&str>,
    query: &str,
    limit: usize,
) -> anyhow::Result<String> {
    let targets = repo_targets(root, backend)?;
    let multi = targets.len() > 1;
    let embedder = rgit_index::Embedder::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut hits: Vec<(f32, String, rgit_index::SearchHit)> = Vec::new();
    for (label, b) in &targets {
        if let Some(index) = rgit_index::load(&rgit_index::index_path(b.workdir())) {
            let boost = history_boost(b);
            for h in rgit_index::search_boosted(&index, &embedder, query, limit, &boost, HISTORY_ALPHA)
                .map_err(|e| anyhow::anyhow!("{e}"))?
            {
                hits.push((h.score, label.clone(), h));
            }
        }
    }
    if hits.is_empty() {
        return Ok("no index; run `rgit index build` first".to_owned());
    }
    hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);
    let mut out = String::new();
    for (score, repo, h) in hits {
        if multi {
            out.push_str(&format!("{score:.3}  {}/{}:{}-{}\n", repo, h.path, h.start_line, h.end_line));
        } else {
            out.push_str(&format!("{score:.3}  {}:{}-{}\n", h.path, h.start_line, h.end_line));
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
fn parse_date(s: &str) -> anyhow::Result<i64> {
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
            None => anyhow::bail!("{what} required"),
        }
    };
    Ok(match command {
        Command::Index { action } => index_cmd(backend, action)?,
        Command::Status => render::status(&backend.status()?),
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
        Command::Blame { path } => render::blame(&backend.blame(&path)?),
        Command::Refs => render::refs(&backend.refs()?),
        Command::Stage { path, hunk, lines } => ok(match (hunk, lines.as_slice()) {
            (Some(h), l) if !l.is_empty() => backend.stage_lines(&path, h, l),
            (Some(h), _) => backend.stage_hunk(&path, h),
            (None, _) => backend.stage_file(&path),
        }),
        Command::Unstage { path, hunk, lines } => ok(match (hunk, lines.as_slice()) {
            (Some(h), l) if !l.is_empty() => backend.unstage_lines(&path, h, l),
            (Some(h), _) => backend.unstage_hunk(&path, h),
            (None, _) => backend.unstage_file(&path),
        }),
        Command::StageAll => ok(backend.stage_all()),
        Command::UnstageAll => ok(backend.unstage_all()),
        Command::Discard { path, hunk, lines } => {
            let path = resolve(path, "a path", &|| {
                crate::interactive::pick_file(backend, "Discard which file?")
            })?;
            ok(match (hunk, lines.as_slice()) {
                (Some(h), l) if !l.is_empty() => backend.discard_lines(&path, h, l),
                (Some(h), _) => backend.discard_hunk(&path, h),
                (None, _) => backend.discard_file(&path),
            })
        }
        Command::Resolve { path, ours, theirs } => {
            if !ours && !theirs {
                anyhow::bail!("resolve needs --ours or --theirs");
            }
            ok(backend.resolve_conflict(&path, ours))
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
        Command::Prune { base } => {
            let deleted = backend.prune_merged(&base)?;
            if deleted.is_empty() {
                format!("no branches merged into {base}")
            } else {
                format!("deleted {} merged: {}", deleted.len(), deleted.join(", "))
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
                ok(backend.checkout_branch(&new))
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
                })
            }
        }
        Command::Merge {
            rev,
            no_ff,
            ff_only,
            abort,
        } => {
            if abort {
                ok(backend.merge_abort())
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
            edit,
            cont,
            skip,
            abort,
        } => {
            if abort {
                ok(backend.rebase_abort())
            } else if cont {
                ok(backend.rebase_continue())
            } else if skip {
                ok(backend.rebase_skip())
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
                ok(backend.rebase_interactive(Some(&onto)))
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
        Command::Reset { rev, soft, hard } => {
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
                ok(backend.reset(&rev, mode))
            }
        }
        Command::CherryPick { rev } => {
            let rev = resolve(rev, "a commit to cherry-pick", &|| {
                crate::interactive::pick_commit(backend, "Cherry-pick which commit?")
            })?;
            ok(backend.cherry_pick(&rev))
        }
        Command::Revert { rev } => {
            let rev = resolve(rev, "a commit to revert", &|| {
                crate::interactive::pick_commit(backend, "Revert which commit?")
            })?;
            ok(backend.revert(&rev))
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
            Some(BranchCmd::Create { name }) => ok(backend.create_branch(&name)),
            Some(BranchCmd::Checkout { name }) => ok(backend.checkout_branch(&name)),
            Some(BranchCmd::Delete { name, force }) => match name {
                Some(name) => ok(backend.delete_branch(&name, force)),
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
                None => anyhow::bail!("a branch name required"),
            },
            Some(BranchCmd::Rename { old, new }) => ok(backend.rename_branch(&old, &new)),
        },
        Command::Stash { cmd } => match cmd {
            None => ok_msg(backend.stash_push(false)),
            Some(StashCmd::Push {
                message: None,
                include_untracked,
            }) => ok_msg(backend.stash_push(include_untracked)),
            Some(StashCmd::Push {
                message: Some(message),
                include_untracked,
            }) => ok_msg(backend.stash_push_message(&message, include_untracked)),
            Some(StashCmd::Pop { index }) => ok(backend.stash_pop(stash_index(
                backend,
                index,
                interactive,
                "Pop which stash?",
            )?)),
            Some(StashCmd::Apply { index }) => ok(backend.stash_apply(stash_index(
                backend,
                index,
                interactive,
                "Apply which stash?",
            )?)),
            Some(StashCmd::Drop { index }) => ok(backend.stash_drop(stash_index(
                backend,
                index,
                interactive,
                "Drop which stash?",
            )?)),
            Some(StashCmd::List) => render::stashes(&backend.status()?.stashes),
        },
        Command::Tag { cmd } => match cmd {
            None | Some(TagCmd::List) => {
                let names: Vec<String> = backend.all_tags()?.into_iter().map(|t| t.name).collect();
                if names.is_empty() {
                    "no tags".to_owned()
                } else {
                    names.join("\n")
                }
            }
            Some(TagCmd::Create { name, message }) => {
                ok(backend.create_tag(&name, message.as_deref().unwrap_or("")))
            }
            Some(TagCmd::Delete { name }) => {
                let name = resolve(name, "a tag name", &|| {
                    crate::interactive::pick_tag(backend, "Delete which tag?")
                })?;
                ok(backend.delete_tag(&name))
            }
        },
        Command::Remote { cmd } => match cmd {
            None => render::remotes(&backend.remotes()?),
            Some(RemoteCmd::Add { name, url }) => ok(backend.add_remote(&name, &url)),
            Some(RemoteCmd::Remove { name }) => ok(backend.remove_remote(&name)),
            Some(RemoteCmd::SetUrl { name, url }) => ok(backend.set_remote_url(&name, &url)),
            Some(RemoteCmd::Rename { old, new }) => ok(backend.rename_remote(&old, &new)),
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
            Some(WorktreeCmd::Add { name, path }) => ok(backend.add_worktree(&name, &path)),
            Some(WorktreeCmd::Remove { name }) => ok(backend.remove_worktree(&name)),
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
            ok(backend.remove_path(&path, cached))
        }
        Command::Mv { from, to, force } => ok(backend.move_path(&from, &to, force)),
        Command::Describe { rev } => backend.describe(rev.as_deref().unwrap_or("HEAD"))?,
        Command::Submodule { mut args } => {
            args.insert(0, "submodule".to_owned());
            backend.git(&args)?
        }
        Command::Git { args } => backend.git(&args)?,
        Command::Init { .. } | Command::Clone { .. } | Command::Mcp | Command::Serve { .. } => {
            unreachable!("handled before dispatch")
        }
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

fn ok(r: Result<(), GitError>) -> String {
    match r {
        Ok(()) => "ok".to_owned(),
        Err(e) => e.to_string(),
    }
}

/// Like [`ok`], but prints the operation's own success line instead of "ok".
fn ok_msg(r: Result<String, GitError>) -> String {
    match r {
        Ok(msg) => msg,
        Err(e) => e.to_string(),
    }
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
