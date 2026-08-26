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
        /// Maximum number of commits to show.
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        /// Walk every ref, not just HEAD.
        #[arg(long)]
        all: bool,
        /// Keep only commits whose author name/email contains this.
        #[arg(long)]
        author: Option<String>,
    },
    /// Diffstat of the staged changes, or between two revisions.
    Diff {
        /// Diff FROM..TO; omit both for the staged diff.
        from: Option<String>,
        /// The second revision (defaults to HEAD when only FROM is given).
        to: Option<String>,
        /// Print the full unified patch instead of a diffstat.
        #[arg(short, long)]
        patch: bool,
    },
    /// A commit's header and diffstat.
    Show {
        /// The commit to show (branch, tag, or sha).
        rev: String,
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
    },
    /// Amend HEAD with the staged changes, keeping its message (no editor).
    Extend,
    /// Fetch the current branch's remote.
    Fetch,
    /// Fetch and fast-forward the current branch.
    Pull,
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
    },
    /// Check out a branch or, for any other revision, a detached HEAD.
    Checkout {
        /// The branch or revision (prompted for if omitted on a terminal).
        rev: Option<String>,
    },
    /// Merge a revision into the current branch.
    Merge {
        /// The branch or revision to merge (prompted for if omitted).
        rev: Option<String>,
        /// Always create a merge commit, even if a fast-forward is possible.
        #[arg(long = "no-ff")]
        no_ff: bool,
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
    },
    /// Stash management (no subcommand stashes the working tree).
    Stash {
        #[command(subcommand)]
        cmd: Option<StashCmd>,
    },
    /// Tag management.
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
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
    Clean,
    /// Remove a tracked path from the index and working tree.
    Rm {
        /// The path to remove (prompted for if omitted on a terminal).
        path: Option<String>,
    },
    /// Rename/move a tracked path.
    Mv {
        /// The current path.
        from: String,
        /// The new path.
        to: String,
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
    },
    /// Clone a repository into a new directory.
    Clone {
        /// The repository URL to clone.
        url: String,
        /// The target directory (defaults to the repository name).
        dir: Option<String>,
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

/// Run a subcommand and return its compact output. When `interactive`, a
/// missing required argument is prompted for; otherwise it errors. `Mcp` is
/// handled by the caller (it takes over the process), so it is unreachable here.
pub fn run(
    backend: &Arc<dyn GitBackend>,
    command: Command,
    interactive: bool,
) -> anyhow::Result<String> {
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
        Command::Status => render::status(&backend.status()?),
        Command::Log { limit, all, author } => {
            render::log(&backend.log(&LogOptions { limit, all, author })?)
        }
        Command::Diff { from, to, patch } => match (from, to) {
            (Some(from), Some(to)) => diff_out(&backend.diff_refs(&from, &to)?, patch),
            (Some(rev), None) => diff_out(&backend.diff_refs(&rev, "HEAD")?, patch),
            (None, _) if patch => backend.staged_patch()?,
            (None, _) => render::diffstat(&backend.status()?.staged),
        },
        Command::Show { rev } => render::commit_details(&backend.commit_details(&rev)?),
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
        Command::Commit { message, amend } => {
            let message = resolve(message, "a commit message", &|| {
                crate::interactive::input("Commit message")
            })?;
            if amend {
                backend.amend(&message)?;
            } else {
                backend.commit(&message)?;
            }
            backend.commit_report().join("\n")
        }
        Command::Extend => {
            backend.commit_extend()?;
            backend.commit_report().join("\n")
        }
        Command::Fetch => net(interactive, "fetch", |r| backend.fetch(r))?,
        Command::Pull => net(interactive, "pull", |r| backend.pull(r))?,
        Command::Push {
            force,
            force_with_lease,
            set_upstream,
        } => net(interactive, "push", |r| {
            backend.push(force, force_with_lease, set_upstream, r)
        })?,
        Command::Checkout { rev } => {
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
        Command::Merge { rev, no_ff } => {
            let rev = resolve(rev, "a revision to merge", &|| {
                crate::interactive::pick_branch(backend, "Merge which branch?")
            })?;
            net(interactive, "merge", |r| backend.merge(&rev, no_ff, r))?
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
        Command::Branch { cmd } => match cmd {
            None => {
                let current = backend.status().ok().and_then(|s| s.head.branch);
                render::branches(&backend.local_branches()?, current.as_deref())
            }
            Some(BranchCmd::Create { name }) => ok(backend.create_branch(&name)),
            Some(BranchCmd::Checkout { name }) => ok(backend.checkout_branch(&name)),
            Some(BranchCmd::Delete { name }) => match name {
                Some(name) => ok(backend.delete_branch(&name)),
                None if interactive => {
                    let names = crate::interactive::multiselect_branches(
                        backend,
                        "Delete which branches?",
                    )?;
                    if names.is_empty() {
                        "none selected".to_owned()
                    } else {
                        for n in &names {
                            backend.delete_branch(n)?;
                        }
                        format!("deleted {}", names.join(", "))
                    }
                }
                None => anyhow::bail!("a branch name required"),
            },
            Some(BranchCmd::Rename { old, new }) => ok(backend.rename_branch(&old, &new)),
        },
        Command::Stash { cmd } => match cmd {
            None | Some(StashCmd::Push { message: None }) => ok_msg(backend.stash_push()),
            Some(StashCmd::Push {
                message: Some(message),
            }) => ok_msg(backend.stash_push_message(&message)),
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
            TagCmd::Create { name, message } => {
                ok(backend.create_tag(&name, message.as_deref().unwrap_or("")))
            }
            TagCmd::Delete { name } => {
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
        Command::Clean => {
            if interactive
                && !crate::interactive::confirm("Remove all untracked files and directories?")?
            {
                "cancelled".to_owned()
            } else {
                ok(backend.clean())
            }
        }
        Command::Rm { path } => {
            let path = resolve(path, "a path", &|| {
                crate::interactive::pick_file(backend, "Remove which file?")
            })?;
            ok(backend.remove_path(&path))
        }
        Command::Mv { from, to } => ok(backend.move_path(&from, &to)),
        Command::Describe { rev } => backend.describe(rev.as_deref().unwrap_or("HEAD"))?,
        Command::Submodule { mut args } => {
            args.insert(0, "submodule".to_owned());
            backend.git(&args)?
        }
        Command::Git { args } => backend.git(&args)?,
        Command::Init { .. } | Command::Clone { .. } | Command::Mcp => {
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

fn diff_out(files: &[rgit_git::FileDiff], patch: bool) -> String {
    if patch {
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
