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
        /// Also show the staged diff; repeat for the unstaged one (git's -v).
        #[arg(short = 'v', long, action = clap::ArgAction::Count)]
        verbose: u8,
        /// Count commits ahead of and behind the upstream (git's default).
        #[arg(long, overrides_with = "no_ahead_behind")]
        ahead_behind: bool,
        /// Skip the ahead/behind count against the upstream.
        #[arg(long)]
        no_ahead_behind: bool,
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
    #[command(flatten)]
    Plumbing(Plumbing),
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
    /// Stage paths as git's `add` does: `add <paths>`, `add .`, `add -A`, `add -u`.
    Add {
        /// The paths to add: files, folders or globs.
        paths: Vec<String>,
        /// Stage every change, new and deleted files too (the whole repo when
        /// no paths are given).
        #[arg(short = 'A', long = "all", conflicts_with = "update")]
        all: bool,
        /// Stage only changes to tracked files (the whole repo when no paths
        /// are given).
        #[arg(short = 'u', long)]
        update: bool,
        /// Add ignored files too.
        #[arg(short = 'f', long)]
        force: bool,
        /// Show what would be added without adding it (git's -n).
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Print each added or removed path.
        #[arg(short = 'v', long)]
        verbose: bool,
        /// Record only that untracked paths will be added, with no content
        /// (git's -N).
        #[arg(short = 'N', long)]
        intent_to_add: bool,
        /// Add what can be added and report the paths that failed.
        #[arg(long)]
        ignore_errors: bool,
        /// Pick hunks to stage, one by one (needs a terminal).
        #[arg(short = 'p', long)]
        patch: bool,
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
    /// Restore files in the working tree (or, with --staged, the index) from
    /// the index or a revision.
    Restore {
        /// The paths to restore: files, folders or globs.
        #[arg(required_unless_present = "patch")]
        paths: Vec<String>,
        /// Restore from this revision (default: the index, or HEAD with --staged).
        #[arg(short = 's', long, value_name = "REV")]
        source: Option<String>,
        /// Restore the index (unstage).
        #[arg(short = 'S', long)]
        staged: bool,
        /// Restore the working tree (the default; with --staged, both).
        #[arg(short = 'W', long)]
        worktree: bool,
        /// For conflicted paths, take our side (git's --ours).
        #[arg(long, conflicts_with_all = ["theirs", "source", "staged"])]
        ours: bool,
        /// For conflicted paths, take their side (git's --theirs).
        #[arg(long, conflicts_with_all = ["source", "staged"])]
        theirs: bool,
        /// Keep files the source lacks instead of removing them.
        #[arg(long, overrides_with = "no_overlay")]
        overlay: bool,
        /// Remove files the source lacks (the default).
        #[arg(long, hide = true)]
        no_overlay: bool,
        /// Pick hunks to restore, one by one (needs a terminal).
        #[arg(short = 'p', long, conflicts_with = "source")]
        patch: bool,
        /// Accepted for git compatibility.
        #[arg(short = 'q', long, hide = true)]
        quiet: bool,
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
    /// Commit the staged changes (runs hooks), or only the given paths.
    Commit {
        /// The commit message; repeat for more paragraphs (prompted for if
        /// omitted on a terminal).
        #[arg(short, long)]
        message: Vec<String>,
        /// Read the commit message from a file (`-` for stdin).
        #[arg(short = 'F', long, value_name = "FILE", conflicts_with = "message")]
        file: Option<String>,
        /// Amend the previous commit instead of creating a new one.
        #[arg(long)]
        amend: bool,
        /// Keep the amended commit's message (with --amend), or -c's without
        /// opening the editor.
        #[arg(long)]
        no_edit: bool,
        /// Edit the message in the editor before committing (git's -e).
        #[arg(short = 'e', long, overrides_with = "no_edit")]
        edit: bool,
        /// Reuse this commit's message and author (git's -C).
        #[arg(
            short = 'C',
            long,
            value_name = "REV",
            conflicts_with_all = ["message", "file", "reedit_message"]
        )]
        reuse_message: Option<String>,
        /// Like -C, but edit the message first when on a terminal (git's -c).
        #[arg(short = 'c', long, value_name = "REV", conflicts_with_all = ["message", "file"])]
        reedit_message: Option<String>,
        /// With -C, -c or --amend, make the committer the author, dated now.
        #[arg(long)]
        reset_author: bool,
        /// The author date: `YYYY-MM-DD[THH:MM:SS]`, `@<unix>` or
        /// `<unix>`, each with an optional `+HHMM` offset.
        #[arg(long, value_name = "DATE")]
        date: Option<String>,
        /// Show what would be committed, without committing.
        #[arg(long)]
        dry_run: bool,
        /// Stage the given paths too, then commit the whole index (git's -i).
        #[arg(short = 'i', long, requires = "paths")]
        include: bool,
        /// Commit only the given paths (the default with paths; git's -o).
        #[arg(short = 'o', long, hide = true)]
        only: bool,
        /// Accepted for git compatibility.
        #[arg(short = 'v', long, hide = true)]
        verbose: bool,
        /// Stage all tracked, modified files before committing (git's -a).
        #[arg(short = 'a', long = "all", conflicts_with = "paths")]
        all: bool,
        /// Skip the pre-commit and commit-msg hooks (git's --no-verify).
        #[arg(short = 'n', long = "no-verify")]
        no_verify: bool,
        /// Set the author, as `Name <email>` or a pattern that names an
        /// existing author.
        #[arg(long, value_name = "NAME <EMAIL>")]
        author: Option<String>,
        /// Add a Signed-off-by trailer for the committer (git's -s).
        #[arg(short = 's', long)]
        signoff: bool,
        /// Commit even when nothing changed.
        #[arg(long)]
        allow_empty: bool,
        /// Make a `fixup!` commit for this revision, for a later autosquash.
        #[arg(long, value_name = "REV", conflicts_with = "squash")]
        fixup: Option<String>,
        /// Make a `squash!` commit for this revision, for a later autosquash.
        #[arg(long, value_name = "REV")]
        squash: Option<String>,
        /// Accepted for git compatibility.
        #[arg(short = 'q', long, hide = true)]
        quiet: bool,
        /// Commit only these paths, as they are in the working tree; other
        /// staged changes stay staged.
        paths: Vec<String>,
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
        /// Take every positional as a remote to fetch (git's --multiple).
        #[arg(long)]
        multiple: bool,
        /// Fetch the whole history of a shallow clone.
        #[arg(long)]
        unshallow: bool,
        /// Deepen a shallow history by N commits.
        #[arg(long, value_name = "N", default_value_t = 0)]
        deepen: i32,
        /// Deepen a shallow history to the commits after DATE.
        #[arg(long, value_name = "DATE")]
        shallow_since: Option<String>,
        /// Fetch every tag and, with --prune, delete local tags gone upstream.
        #[arg(short = 'P', long)]
        prune_tags: bool,
        /// Follow no tags.
        #[arg(short = 'n', long, conflicts_with = "tags")]
        no_tags: bool,
        /// Update refs even when that is not a fast-forward.
        #[arg(short = 'f', long)]
        force: bool,
        /// Map the refspecs given through REFSPEC instead of the configured
        /// ones; `--refmap=` updates no remote-tracking ref.
        #[arg(long, value_name = "REFSPEC")]
        refmap: Vec<String>,
        /// Record the fetched branch as the current branch's upstream.
        #[arg(long)]
        set_upstream: bool,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
        /// Report more (accepted for git compatibility).
        #[arg(short, long)]
        verbose: bool,
        /// Parallel fetches (accepted; remotes are fetched in turn).
        #[arg(short = 'j', long, value_name = "N")]
        jobs: Option<usize>,
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
        /// Always make a merge commit, even for a fast-forward.
        #[arg(long, conflicts_with = "ff_only")]
        no_ff: bool,
        /// Stage the upstream's changes without committing or recording a merge.
        #[arg(long)]
        squash: bool,
        /// Merge but stop before making the merge commit.
        #[arg(long)]
        no_commit: bool,
        /// Stash local changes before the pull and restore them after
        /// (default: `rebase.autoStash` / `merge.autoStash`).
        #[arg(long, overrides_with = "no_autostash")]
        autostash: bool,
        /// Do not stash local changes around the pull.
        #[arg(long)]
        no_autostash: bool,
        /// On conflicting hunks take `ours` or `theirs` (merge only).
        #[arg(short = 'X', long = "strategy-option", value_name = "OPTION")]
        strategy_option: Option<String>,
        /// Fetch every remote first.
        #[arg(long)]
        all: bool,
        /// Limit the fetched history to N commits.
        #[arg(long, value_name = "N", default_value_t = 0)]
        depth: i32,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
        /// Report more (accepted for git compatibility).
        #[arg(short, long)]
        verbose: bool,
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
        /// Also push annotated tags that point into the pushed history
        /// (default: `push.followTags`).
        #[arg(long)]
        follow_tags: bool,
        /// Push every ref or none.
        #[arg(long)]
        atomic: bool,
        /// With --all or --tags, delete remote refs no local ref maps to.
        #[arg(long)]
        prune: bool,
        /// Make every remote ref match the local one: force-updates and deletes.
        #[arg(long, conflicts_with_all = ["all", "tags", "delete"])]
        mirror: bool,
        /// Pass OPTION to the server's hooks (repeatable).
        #[arg(short = 'o', long = "push-option", value_name = "OPTION")]
        push_option: Vec<String>,
        /// Skip the pre-push hook.
        #[arg(long)]
        no_verify: bool,
        /// Report in git's machine-readable format.
        #[arg(long)]
        porcelain: bool,
        /// Submodule check: check, on-demand, only or no (accepted; submodules
        /// are not pushed or checked).
        #[arg(long, value_name = "MODE", require_equals = true)]
        recurse_submodules: Option<String>,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
        /// Also list refs that are already up to date.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Check out a branch or, for any other revision, a detached HEAD; or
    /// restore paths from a revision or the index.
    Checkout {
        /// The branch or revision, `-` for the previous one (prompted for if
        /// omitted on a terminal). A path here restores it from the index.
        rev: Option<String>,
        /// Paths to restore from `rev` (index and working tree), or from the
        /// index when no revision is given.
        #[arg(value_name = "PATH")]
        pathspec: Vec<String>,
        /// Create a new branch and switch to it (git's -b), from `rev` or HEAD.
        #[arg(
            short = 'b',
            value_name = "NEW_BRANCH",
            conflicts_with = "force_branch"
        )]
        branch: Option<String>,
        /// Create or reset a branch and switch to it (git's -B).
        #[arg(short = 'B', value_name = "BRANCH")]
        force_branch: Option<String>,
        /// Detach HEAD at `rev` (default HEAD), even when it is a branch.
        #[arg(long, conflicts_with_all = ["branch", "force_branch"])]
        detach: bool,
        /// Track `rev` as the new branch's upstream; without -b, create a local
        /// branch named after the remote one.
        #[arg(short = 't', long)]
        track: bool,
        /// Never set an upstream for the new branch.
        #[arg(long, conflicts_with = "track")]
        no_track: bool,
        /// Throw away local changes when switching (git's -f).
        #[arg(short = 'f', long)]
        force: bool,
        /// Carry local changes over to the new branch with a three-way merge.
        #[arg(short = 'm', long, conflicts_with = "force")]
        merge: bool,
        /// Conflict marker style for -m: `merge` or `diff3`.
        #[arg(long, value_name = "STYLE", value_parser = ["merge", "diff3"])]
        conflict: Option<String>,
        /// Start a new branch with no history at `rev` (default HEAD).
        #[arg(long, value_name = "NEW_BRANCH", conflicts_with_all = ["branch", "force_branch", "detach"])]
        orphan: Option<String>,
        /// For conflicted paths, write our side to the working tree.
        #[arg(long, conflicts_with = "theirs")]
        ours: bool,
        /// For conflicted paths, write their side to the working tree.
        #[arg(long)]
        theirs: bool,
        /// Do not turn a remote branch's name into a local tracking branch.
        #[arg(long, overrides_with = "guess")]
        no_guess: bool,
        /// Turn a remote branch's name into a local tracking branch (default).
        #[arg(long, hide = true)]
        guess: bool,
        /// Pick hunks to discard from the working tree, one by one (needs a
        /// terminal).
        #[arg(short = 'p', long)]
        patch: bool,
        /// Accepted for git compatibility.
        #[arg(short = 'q', long, hide = true)]
        quiet: bool,
        /// Paths to restore (after `--`).
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Switch branches: `switch <branch>`, `-c <new> [<start>]`, `--detach
    /// <rev>`, or `-` for the previous branch.
    Switch {
        /// The branch to switch to (`-` for the previous one); with -c, -C or
        /// --detach, the start point (prompted for if omitted on a terminal).
        rev: Option<String>,
        /// Create a new branch and switch to it.
        #[arg(
            short = 'c',
            long,
            value_name = "NEW_BRANCH",
            conflicts_with = "force_create"
        )]
        create: Option<String>,
        /// Create or reset a branch and switch to it.
        #[arg(short = 'C', long, value_name = "BRANCH")]
        force_create: Option<String>,
        /// Switch to a revision as a detached HEAD.
        #[arg(short = 'd', long, conflicts_with_all = ["create", "force_create"])]
        detach: bool,
        /// Track the start point as the new branch's upstream.
        #[arg(short = 't', long)]
        track: bool,
        /// Never set an upstream for the new branch.
        #[arg(long, conflicts_with = "track")]
        no_track: bool,
        /// Throw away local changes when switching.
        #[arg(short = 'f', long, alias = "force")]
        discard_changes: bool,
        /// Carry local changes over to the new branch with a three-way merge.
        #[arg(short = 'm', long, conflicts_with = "discard_changes")]
        merge: bool,
        /// Conflict marker style for -m: `merge` or `diff3`.
        #[arg(long, value_name = "STYLE", value_parser = ["merge", "diff3"])]
        conflict: Option<String>,
        /// Start a new branch with no history and an empty working tree.
        #[arg(long, value_name = "NEW_BRANCH", conflicts_with_all = ["create", "force_create", "detach"])]
        orphan: Option<String>,
        /// Do not turn a remote branch's name into a local tracking branch.
        #[arg(long, overrides_with = "guess")]
        no_guess: bool,
        /// Turn a remote branch's name into a local tracking branch (default).
        #[arg(long, hide = true)]
        guess: bool,
        /// Accepted for git compatibility.
        #[arg(short = 'q', long, hide = true)]
        quiet: bool,
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
        /// Read the merge commit message from a file (`-` for stdin).
        #[arg(
            short = 'F',
            long = "file",
            value_name = "PATH",
            conflicts_with = "message"
        )]
        file: Option<String>,
        /// Take this side on conflicting hunks (git's -X).
        #[arg(short = 'X', long = "strategy-option", value_parser = ["ours", "theirs"])]
        strategy_option: Option<String>,
        /// The merge strategy; `ours` records the merge but keeps HEAD's tree.
        #[arg(short = 's', long, value_parser = ["ort", "recursive", "resolve", "octopus", "ours"])]
        strategy: Option<String>,
        /// Allow merging histories that share no commit.
        #[arg(long = "allow-unrelated-histories")]
        allow_unrelated_histories: bool,
        /// Add the merged commits' subjects (at most N, default 20) to the message.
        #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "20")]
        log: Option<usize>,
        /// Show a diffstat at the end (the default).
        #[arg(long, visible_alias = "summary")]
        stat: bool,
        /// Do not show a diffstat at the end.
        #[arg(short = 'n', long = "no-stat", conflicts_with = "stat")]
        no_stat: bool,
        /// Open the editor on the merge commit message.
        #[arg(short = 'e', long)]
        edit: bool,
        /// Keep the generated message without an editor (the default).
        #[arg(long = "no-edit", hide = true, conflicts_with = "edit")]
        no_edit: bool,
        /// Skip the pre-merge-commit and commit-msg hooks.
        #[arg(long = "no-verify")]
        no_verify: bool,
        /// Run the pre-merge-commit and commit-msg hooks (the default).
        #[arg(long, hide = true, conflicts_with = "no_verify")]
        verify: bool,
        /// Add a Signed-off-by trailer.
        #[arg(long)]
        signoff: bool,
        /// Print nothing on success.
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Commit a merge whose conflicts are resolved.
        #[arg(long = "continue")]
        cont: bool,
        /// Abort an in-progress (conflicted) merge, restoring HEAD.
        #[arg(long)]
        abort: bool,
        /// Forget an in-progress merge, leaving the index and working tree as they are.
        #[arg(long)]
        quit: bool,
    },
    /// Rebase onto a revision, or continue/skip/abort an in-progress rebase.
    Rebase {
        /// The upstream to rebase onto (prompted for if omitted). With --onto,
        /// this is the upstream whose commits after it are replayed.
        onto: Option<String>,
        /// Check out this branch first and rebase it (git's `<upstream> <branch>`).
        branch: Option<String>,
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
        #[command(flatten)]
        more: RebaseFlags,
        /// Continue after resolving conflicts.
        #[arg(long = "continue")]
        cont: bool,
        /// Skip the current commit.
        #[arg(long)]
        skip: bool,
        /// Abort the in-progress rebase.
        #[arg(long)]
        abort: bool,
        /// Stop the rebase, leaving HEAD, the index and the working tree as they are.
        #[arg(long)]
        quit: bool,
        /// Edit the todo list of the in-progress rebase (needs a terminal).
        #[arg(long = "edit-todo")]
        edit_todo: bool,
        /// Show the commit the rebase stopped at.
        #[arg(long = "show-current-patch")]
        show_current_patch: bool,
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
    /// Find the commit that introduced a change by binary search: `start <bad>
    /// <good>`, then mark each step `good`/`bad` (or `run <cmd>`), then `reset`.
    Bisect {
        #[command(subcommand)]
        cmd: BisectCmd,
    },
    /// Reset HEAD to a revision (default mixed).
    Reset {
        /// The revision to reset to (prompted for if omitted on a terminal). A
        /// path here unstages it (git's `reset <paths>`).
        rev: Option<String>,
        /// Paths whose index entries to reset to `rev` (default HEAD), leaving
        /// HEAD alone.
        #[arg(value_name = "PATH")]
        pathspec: Vec<String>,
        /// Move HEAD only, keep the index and working tree.
        #[arg(long, conflicts_with_all = ["hard", "mixed", "keep", "merge"])]
        soft: bool,
        /// Reset the index but not the working tree (the default).
        #[arg(long, conflicts_with_all = ["hard", "keep", "merge"])]
        mixed: bool,
        /// Reset the index and working tree too (discards changes).
        #[arg(long, conflicts_with_all = ["keep", "merge"])]
        hard: bool,
        /// Like --hard, but keep local changes and refuse to overwrite them.
        #[arg(long, conflicts_with = "merge")]
        keep: bool,
        /// Reset the index and the files that change, keeping unstaged
        /// changes to other files (aborts a merge; git's --merge).
        #[arg(long)]
        merge: bool,
        /// Pick staged hunks to unstage, one by one (needs a terminal).
        #[arg(short = 'p', long, conflicts_with_all = ["soft", "hard", "keep", "merge"])]
        patch: bool,
        /// Print nothing on success.
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Paths to reset (after `--`).
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
        /// Open the editor on each commit message.
        #[arg(short = 'e', long)]
        edit: bool,
        /// Keep the original message without an editor (the default).
        #[arg(long = "no-edit", hide = true, conflicts_with = "edit")]
        no_edit: bool,
        /// Add a Signed-off-by trailer.
        #[arg(short = 's', long)]
        signoff: bool,
        /// Fast-forward over a commit whose parent is HEAD instead of copying it.
        #[arg(long, conflicts_with_all = ["no_commit", "record_origin", "signoff", "edit"])]
        ff: bool,
        /// Keep commits that were empty to begin with.
        #[arg(long = "allow-empty")]
        allow_empty: bool,
        /// Keep commits that become empty (same as --empty=keep).
        #[arg(long = "keep-redundant-commits", conflicts_with = "empty")]
        keep_redundant_commits: bool,
        /// What to do with a commit whose change is already in HEAD.
        #[arg(long, value_parser = ["stop", "drop", "keep"])]
        empty: Option<String>,
        /// Commit the resolved commit and apply the rest.
        #[arg(long = "continue")]
        cont: bool,
        /// Drop the current commit and apply the rest.
        #[arg(long)]
        skip: bool,
        /// Cancel and return to where the cherry-pick started.
        #[arg(long)]
        abort: bool,
        /// Forget the stopped cherry-pick, leaving HEAD, the index and the working tree as they are.
        #[arg(long)]
        quit: bool,
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
        /// Open the editor on each revert message.
        #[arg(short = 'e', long)]
        edit: bool,
        /// Keep git's revert message without an editor (the default).
        #[arg(long = "no-edit", hide = true, conflicts_with = "edit")]
        no_edit: bool,
        /// Add a Signed-off-by trailer.
        #[arg(short = 's', long)]
        signoff: bool,
        /// Name the reverted commit as `abbrev (subject, date)`, under a title to fill in.
        #[arg(long)]
        reference: bool,
        /// Commit the resolved revert and apply the rest.
        #[arg(long = "continue")]
        cont: bool,
        /// Drop the current commit and apply the rest.
        #[arg(long)]
        skip: bool,
        /// Cancel and return to where the revert started.
        #[arg(long)]
        abort: bool,
        /// Forget the stopped revert, leaving HEAD, the index and the working tree as they are.
        #[arg(long)]
        quit: bool,
    },
    /// Branch management: no subcommand lists local branches, `branch <name>
    /// [<start>]` creates one without switching to it, and git's flags work as
    /// in git.
    #[command(
        args_conflicts_with_subcommands = true,
        group = clap::ArgGroup::new("branch_action").multiple(false)
    )]
    Branch {
        #[command(subcommand)]
        cmd: Option<BranchCmd>,
        #[command(flatten)]
        opts: BranchOpts,
    },
    /// Stash management (no subcommand stashes the working tree, taking
    /// `stash push`'s flags).
    #[command(args_conflicts_with_subcommands = true)]
    Stash {
        #[command(subcommand)]
        cmd: Option<StashCmd>,
        #[command(flatten)]
        push: StashPush,
        /// Stash only these paths (after `--`, as in git).
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Tag management: `tag` lists, `tag <name> [<rev>]` creates, `tag -d
    /// <name>...` deletes, `tag -l <pattern>...` lists matching tags.
    Tag {
        /// The tag to create, then the revision to tag (default HEAD); with -d,
        /// the tags to delete; with -v, the tags to verify; with -l, patterns
        /// to list.
        names: Vec<String>,
        #[command(flatten)]
        opts: TagOpts,
    },
    /// Remote management (no subcommand lists remotes).
    #[command(args_conflicts_with_subcommands = true)]
    Remote {
        #[command(subcommand)]
        cmd: Option<RemoteCmd>,
        /// List each remote's fetch and push URLs (git's -v).
        #[arg(short, long)]
        verbose: bool,
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
        /// Print nothing on success.
        #[arg(short = 'q', long)]
        quiet: bool,
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
        /// Remove files even with staged or unstaged changes.
        #[arg(short = 'f', long)]
        force: bool,
        /// List what would be removed without removing it (git's -n).
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Print nothing on success.
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Succeed even when a path matches no file.
        #[arg(long)]
        ignore_unmatch: bool,
    },
    /// Rename/move tracked files or folders.
    Mv {
        /// The paths to move, then the destination (a folder when moving several).
        #[arg(required = true, num_args = 2.., value_name = "PATH")]
        paths: Vec<String>,
        /// Overwrite the destination if it exists (git's -f).
        #[arg(short = 'f', long)]
        force: bool,
        /// Skip moves that would fail instead of stopping (git's -k).
        #[arg(short = 'k')]
        skip_errors: bool,
        /// Show what would be moved without moving it (git's -n).
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Print each move.
        #[arg(short = 'v', long)]
        verbose: bool,
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
        /// Copy hooks and other files from DIR into the new repository.
        #[arg(long, value_name = "DIR")]
        template: Option<String>,
        /// Share the repository: group (default), all, umask or an octal mode.
        #[arg(
            long,
            value_name = "PERMISSIONS",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "group"
        )]
        shared: Option<String>,
        /// Put the repository in GIT_DIR and link it from the working tree.
        #[arg(long, value_name = "GIT_DIR")]
        separate_git_dir: Option<String>,
        /// The object id hash: only sha1 is supported.
        #[arg(long, value_name = "FORMAT")]
        object_format: Option<String>,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
    },
    /// List the refs in a remote repository, like `git ls-remote`.
    LsRemote {
        /// The remote name or URL (defaults to the current branch's remote).
        repository: Option<String>,
        /// Only refs whose name ends in one of these (globs allowed: `v1.*`).
        patterns: Vec<String>,
        /// Only branches (refs/heads).
        #[arg(short = 'b', long, visible_alias = "branches")]
        heads: bool,
        /// Only tags (refs/tags).
        #[arg(short = 't', long)]
        tags: bool,
        /// Leave out peeled tags (`^{}`) and HEAD.
        #[arg(long)]
        refs: bool,
        /// Also show what symbolic refs point at.
        #[arg(long)]
        symref: bool,
        /// Do not print the remote's URL.
        #[arg(short, long)]
        quiet: bool,
        /// Exit with status 2 when no ref matches.
        #[arg(long)]
        exit_code: bool,
        /// Print the remote's URL instead of listing refs.
        #[arg(long)]
        get_url: bool,
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
        /// Fetch only the branch checked out (-b, else the remote's HEAD).
        #[arg(long, overrides_with = "no_single_branch")]
        single_branch: bool,
        /// Fetch every branch (the default).
        #[arg(long)]
        no_single_branch: bool,
        /// Leave the working tree empty.
        #[arg(short = 'n', long)]
        no_checkout: bool,
        /// A bare copy of every ref that `fetch` keeps in step with the remote.
        #[arg(long)]
        mirror: bool,
        /// Follow no tags, now or on later fetches.
        #[arg(long)]
        no_tags: bool,
        /// Borrow objects from this local repository (git's alternates).
        #[arg(long, value_name = "REPO")]
        reference: Vec<String>,
        /// With --reference, copy the borrowed objects and drop the link.
        #[arg(long)]
        dissociate: bool,
        /// Share the source's objects instead of copying them (local only).
        #[arg(short = 's', long)]
        shared: bool,
        /// A partial clone, e.g. `blob:none` or `tree:0`.
        #[arg(long, value_name = "SPEC")]
        filter: Option<String>,
        /// Start with a sparse checkout of the top-level files only.
        #[arg(long)]
        sparse: bool,
        /// Copy hooks and other files from DIR into the new repository.
        #[arg(long, value_name = "DIR")]
        template: Option<String>,
        /// Shallow-clone the commits after DATE.
        #[arg(long, value_name = "DATE")]
        shallow_since: Option<String>,
        /// Put the repository in GIT_DIR and link it from the working tree.
        #[arg(long, value_name = "GIT_DIR")]
        separate_git_dir: Option<String>,
        /// Set KEY=VALUE in the new repository's config (repeatable).
        #[arg(short = 'c', long = "config", value_name = "KEY=VALUE")]
        config: Vec<String>,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
        /// Report more (accepted for git compatibility).
        #[arg(short, long)]
        verbose: bool,
        /// Parallel submodule fetches (accepted; fetched in turn).
        #[arg(short = 'j', long, value_name = "N")]
        jobs: Option<usize>,
    },
    /// Submodules: status (the default), add, init, update, sync, deinit,
    /// foreach, summary, set-url, set-branch and absorbgitdirs.
    Submodule {
        #[command(subcommand)]
        cmd: Option<SubmoduleCmd>,
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

/// Read-only plumbing commands, done natively with git's output formats.
#[derive(Subcommand)]
pub enum Plumbing {
    /// Resolve revisions to object ids, or print repository paths, like
    /// `git rev-parse`. Answers print in the order the options and revisions
    /// are given.
    RevParse {
        /// Revisions (`HEAD`, `main~2`, `v1^{commit}`, `HEAD:path`, `^A`,
        /// `A..B`, `A...B`) and options: `--verify`, `-q`, `--short[=N]`,
        /// `--abbrev-ref`, `--symbolic`, `--symbolic-full-name`, `--not`,
        /// `--all`, `--branches`, `--tags`, `--remotes`, `--default REV`,
        /// `--show-toplevel`, `--show-prefix`, `--show-cdup`, `--git-dir`,
        /// `--absolute-git-dir`, `--git-common-dir`, `--is-inside-work-tree`, `--is-inside-git-dir`, `--is-bare-repository`,
        /// `--is-shallow-repository`, `--show-object-format`, `--local-env-vars`,
        /// `--sq-quote ARGS...`.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
        args: Vec<String>,
    },
    /// List files in the index and the working tree, like `git ls-files`.
    LsFiles {
        /// Show tracked files (the default).
        #[arg(short = 'c', long)]
        cached: bool,
        /// Show mode, object id and stage for each tracked file.
        #[arg(short = 's', long)]
        stage: bool,
        /// Show untracked files (with ignored ones unless --exclude-standard).
        #[arg(short = 'o', long)]
        others: bool,
        /// With -o or -c and --exclude-standard, show only ignored files.
        #[arg(short = 'i', long)]
        ignored: bool,
        /// Honor .gitignore, .git/info/exclude and core.excludesFile.
        #[arg(long = "exclude-standard")]
        exclude_standard: bool,
        /// Show files changed in the working tree (deleted ones too).
        #[arg(short = 'm', long)]
        modified: bool,
        /// Show files deleted from the working tree.
        #[arg(short = 'd', long)]
        deleted: bool,
        /// Show only unmerged files, with -s's format.
        #[arg(short = 'u', long)]
        unmerged: bool,
        /// End each entry with NUL instead of a newline.
        #[arg(short = 'z')]
        z: bool,
        /// Print paths from the top level, not from the current folder.
        #[arg(long = "full-name")]
        full_name: bool,
        /// Fail if a path matches no file.
        #[arg(long = "error-unmatch")]
        error_unmatch: bool,
        /// Limit to these paths: files, folders or globs (default: the
        /// current folder).
        paths: Vec<String>,
    },
    /// List a tree's entries, like `git ls-tree`.
    LsTree {
        /// Recurse into subtrees.
        #[arg(short = 'r')]
        recursive: bool,
        /// Show only trees.
        #[arg(short = 'd')]
        only_trees: bool,
        /// Show trees even when recursing into them.
        #[arg(short = 't')]
        show_trees: bool,
        /// Show blob sizes.
        #[arg(short = 'l', long)]
        long: bool,
        /// Show only paths.
        #[arg(long = "name-only", visible_alias = "name-status")]
        name_only: bool,
        /// Show only object ids.
        #[arg(long = "object-only")]
        object_only: bool,
        /// Abbreviate object ids to a unique prefix of at least N digits.
        #[arg(
            long,
            value_name = "N",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "0"
        )]
        abbrev: Option<usize>,
        /// End each entry with NUL instead of a newline.
        #[arg(short = 'z')]
        z: bool,
        /// Print paths from the top level, not from the current folder.
        #[arg(long = "full-name")]
        full_name: bool,
        /// List from the top level, not the current folder; implies --full-name.
        #[arg(long = "full-tree")]
        full_tree: bool,
        /// The line format: `%(objectmode)`, `%(objecttype)`, `%(objectname)`,
        /// `%(objectsize)`, `%(objectsize:padded)`, `%(path)`.
        #[arg(long)]
        format: Option<String>,
        /// The commit, tag or tree to list.
        rev: String,
        /// Limit to these paths; `dir/` lists inside dir.
        paths: Vec<String>,
    },
    /// Print an object's type, size or content, like `git cat-file`.
    CatFile {
        /// Print the object's type.
        #[arg(short = 't')]
        kind: bool,
        /// Print the object's size in bytes.
        #[arg(short = 's')]
        size: bool,
        /// Print the object's content (a tree as `ls-tree` lists it).
        #[arg(short = 'p')]
        pretty: bool,
        /// Exit 0 if the object exists, 1 if not, printing nothing.
        #[arg(short = 'e')]
        exists: bool,
        /// Read object names from stdin; print each one's id, type, size and
        /// content, or the given format (`%(objectname)`, `%(objecttype)`,
        /// `%(objectsize)`, `%(rest)`).
        #[arg(long, value_name = "FORMAT", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        batch: Option<String>,
        /// Like --batch, without the content.
        #[arg(long = "batch-check", value_name = "FORMAT", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        batch_check: Option<String>,
        /// With --batch or --batch-check, answer for every object instead of stdin.
        #[arg(long = "batch-all-objects")]
        batch_all_objects: bool,
        /// With --batch or --batch-check, do not flush after each object.
        #[arg(long)]
        buffer: bool,
        /// The object (`HEAD`, `HEAD:src/lib.rs`, an id), optionally after its
        /// type (`blob HEAD:a.txt`).
        #[arg(num_args = 0..=2, value_name = "[TYPE] OBJECT")]
        args: Vec<String>,
    },
    /// List refs with their object ids, like `git show-ref`.
    ShowRef {
        /// Only branches (refs/heads).
        #[arg(long, visible_alias = "branches")]
        heads: bool,
        /// Only tags (refs/tags).
        #[arg(long)]
        tags: bool,
        /// Match full ref names exactly (`refs/heads/main`), failing if one is missing.
        #[arg(long)]
        verify: bool,
        /// Also show what annotated tags point at (`refs/tags/v1^{}`).
        #[arg(short = 'd', long)]
        dereference: bool,
        /// Show only object ids, abbreviated to N digits when given.
        #[arg(
            short = 's',
            long,
            value_name = "N",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "0"
        )]
        hash: Option<usize>,
        /// Abbreviate object ids to a unique prefix of at least N digits.
        #[arg(
            long,
            value_name = "N",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "0"
        )]
        abbrev: Option<usize>,
        /// Exit 0 if the one ref given exists, 2 if not.
        #[arg(long, conflicts_with = "verify")]
        exists: bool,
        /// Also show HEAD.
        #[arg(long)]
        head: bool,
        /// Print nothing; only set the exit code.
        #[arg(short, long)]
        quiet: bool,
        /// Show refs whose name ends with one of these (`main`, `origin/main`).
        patterns: Vec<String>,
    },
    /// List refs in a custom format, like `git for-each-ref`.
    ForEachRef {
        /// The line format: `%(refname)`, `%(refname:short)`, `%(objectname)`,
        /// `%(objectname:short)`, `%(objecttype)`, `%(subject)`, `%(body)`,
        /// `%(authorname)`, `%(authoremail)`, `%(authordate[:short|iso|unix|relative])`,
        /// `%(committer*)`, `%(tagger*)`, `%(creatordate)`, `%(contents[:signature])`,
        /// `%(upstream[:short|track|trackshort])`, `%(HEAD)`, `%(symref)`,
        /// `%(objectsize)`, `%(tree)`, `%(parent)`, `%(*objectname)`,
        /// `%(align:N[,middle|right])...%(end)`,
        /// `%(if[:equals=X])...%(then)...[%(else)...]%(end)`.
        #[arg(long)]
        format: Option<String>,
        /// Sort by this field; `-` in front reverses (`-committerdate`);
        /// `version:` compares numbers by value (`version:refname`). The
        /// last --sort is the main key.
        #[arg(long, value_name = "KEY")]
        sort: Vec<String>,
        /// Show at most N refs.
        #[arg(long, value_name = "N")]
        count: Option<usize>,
        /// Only refs reachable from this commit (default HEAD).
        #[arg(long, value_name = "COMMIT", num_args = 0..=1, default_missing_value = "HEAD")]
        merged: Vec<String>,
        /// Only refs not reachable from this commit (default HEAD).
        #[arg(long = "no-merged", value_name = "COMMIT", num_args = 0..=1, default_missing_value = "HEAD")]
        no_merged: Vec<String>,
        /// Only refs that contain this commit (default HEAD).
        #[arg(long, value_name = "COMMIT", num_args = 0..=1, default_missing_value = "HEAD")]
        contains: Vec<String>,
        /// Only refs that do not contain this commit (default HEAD).
        #[arg(long = "no-contains", value_name = "COMMIT", num_args = 0..=1, default_missing_value = "HEAD")]
        no_contains: Vec<String>,
        /// Only refs that point at this object.
        #[arg(long = "points-at", value_name = "OBJECT")]
        points_at: Vec<String>,
        /// Leave out refs matching this pattern.
        #[arg(long, value_name = "PATTERN")]
        exclude: Vec<String>,
        /// Print nothing for a ref whose format expands to nothing.
        #[arg(long = "omit-empty")]
        omit_empty: bool,
        /// Show refs that start with one of these (`refs/heads`) or match a glob.
        patterns: Vec<String>,
    },
    /// List commit ids reachable from revisions, like `git rev-list`.
    RevList {
        /// Show at most N commits.
        #[arg(short = 'n', long = "max-count", value_name = "N")]
        max_count: Option<usize>,
        /// Print only the number of commits.
        #[arg(long)]
        count: bool,
        /// Walk every ref and HEAD.
        #[arg(long)]
        all: bool,
        /// Show the oldest commit first.
        #[arg(long)]
        reverse: bool,
        /// Follow only the first parent of merges.
        #[arg(long = "first-parent")]
        first_parent: bool,
        /// Show only merge commits.
        #[arg(long, conflicts_with = "no_merges")]
        merges: bool,
        /// Leave out merge commits.
        #[arg(long = "no-merges")]
        no_merges: bool,
        /// Print each commit's parents after it.
        #[arg(long)]
        parents: bool,
        /// Leave out the first N commits.
        #[arg(long, value_name = "N")]
        skip: Option<usize>,
        /// Walk every branch, or those matching the pattern.
        #[arg(long, value_name = "PATTERN", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        branches: Option<String>,
        /// Walk every tag, or those matching the pattern.
        #[arg(long, value_name = "PATTERN", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        tags: Option<String>,
        /// Walk every remote-tracking branch, or those matching the pattern.
        #[arg(long, value_name = "PATTERN", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        remotes: Option<String>,
        /// Never show a parent before all of its children.
        #[arg(long = "topo-order")]
        topo_order: bool,
        /// Show commits by date (the default).
        #[arg(long = "date-order")]
        date_order: bool,
        /// Abbreviate commit ids.
        #[arg(long = "abbrev-commit")]
        abbrev_commit: bool,
        /// Revisions: `HEAD`, `^A` (exclude), `A..B`, `A...B`.
        revs: Vec<String>,
        /// Only commits that change these paths (after `--`), simplified as
        /// git does.
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Find the common ancestor of two commits, like `git merge-base`.
    MergeBase {
        /// Print every best common ancestor.
        #[arg(short = 'a', long)]
        all: bool,
        /// Exit 0 if the first commit is an ancestor of the second, else 1.
        #[arg(long = "is-ancestor")]
        is_ancestor: bool,
        /// The first commit.
        a: String,
        /// The second commit.
        b: String,
    },
    /// Show where a ref pointed over time, like `git reflog show`.
    Reflog {
        /// Show at most N entries.
        #[arg(short = 'n', long = "max-count", value_name = "N")]
        max_count: Option<usize>,
        /// `show` (optional) and the ref (default HEAD).
        #[arg(num_args = 0..=2, value_name = "[show] REF")]
        args: Vec<String>,
    },
    /// Summarize commits by author, like `git shortlog`.
    Shortlog {
        /// Print only each author's commit count.
        #[arg(short = 's', long)]
        summary: bool,
        /// Sort by commit count instead of by name.
        #[arg(short = 'n', long)]
        numbered: bool,
        /// Show email addresses.
        #[arg(short = 'e', long)]
        email: bool,
        /// Group by committer instead of author.
        #[arg(short = 'c', long)]
        committer: bool,
        /// Walk every ref and HEAD.
        #[arg(long)]
        all: bool,
        /// Revisions or ranges (default HEAD).
        revs: Vec<String>,
    },
    /// Search tracked files, the index or a revision, like `git grep`. Paths
    /// print from the current folder, which limits the search by default.
    #[command(disable_help_flag = true)]
    Grep {
        /// Print help.
        #[arg(long, action = clap::ArgAction::Help)]
        help: Option<bool>,
        /// Show this many lines of context after each match.
        #[arg(short = 'A', long = "after-context", value_name = "N")]
        after: Option<usize>,
        /// Show this many lines of context before each match.
        #[arg(short = 'B', long = "before-context", value_name = "N")]
        before: Option<usize>,
        /// Show this many lines of context around each match.
        #[arg(short = 'C', long = "context", value_name = "N")]
        context: Option<usize>,
        /// Show only the matching part of each line.
        #[arg(short = 'o', long = "only-matching")]
        only_matching: bool,
        /// Show only the names of files without a match.
        #[arg(short = 'L', long = "files-without-match")]
        files_without_match: bool,
        /// Show each file's name once, above its matches.
        #[arg(long)]
        heading: bool,
        /// Print an empty line between files.
        #[arg(long = "break")]
        break_: bool,
        /// Leave file names out of match lines.
        #[arg(short = 'h', conflicts_with = "with_filename")]
        no_filename: bool,
        /// Show file names on match lines (the default).
        #[arg(short = 'H')]
        with_filename: bool,
        /// Stop each file after N matching lines.
        #[arg(short = 'm', long = "max-count", value_name = "N")]
        max_count: Option<u64>,
        /// Skip binary files.
        #[arg(short = 'I')]
        skip_binary: bool,
        /// Print NUL after file names instead of `:`.
        #[arg(short = 'z', long = "null")]
        null: bool,
        /// Print paths from the top level, not from the current folder.
        #[arg(long = "full-name")]
        full_name: bool,
        /// Match regardless of case.
        #[arg(short = 'i', long = "ignore-case")]
        ignore_case: bool,
        /// Match whole words only.
        #[arg(short = 'w', long = "word-regexp")]
        word: bool,
        /// Show lines that do not match.
        #[arg(short = 'v', long = "invert-match")]
        invert: bool,
        /// Prefix each line with its number.
        #[arg(short = 'n', long = "line-number")]
        line_number: bool,
        /// Show only the names of files with matches.
        #[arg(short = 'l', long = "files-with-matches", visible_alias = "name-only")]
        files: bool,
        /// Show the number of matching lines per file.
        #[arg(short = 'c', long)]
        count: bool,
        /// Print nothing; exit 0 on a match, 1 otherwise.
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Patterns are fixed strings, not regexes.
        #[arg(short = 'F', long = "fixed-strings")]
        fixed: bool,
        /// Patterns are extended regexes (the default is basic).
        #[arg(short = 'E', long = "extended-regexp")]
        extended: bool,
        /// Patterns are Perl-style regexes (read as extended).
        #[arg(short = 'P', long = "perl-regexp")]
        perl: bool,
        /// A pattern; repeat to match any of several.
        #[arg(short = 'e', value_name = "PATTERN", allow_hyphen_values = true)]
        patterns: Vec<String>,
        /// Search the index instead of the working tree.
        #[arg(long)]
        cached: bool,
        /// The pattern (unless -e is given), then revisions or paths.
        #[arg(value_name = "PATTERN_OR_REV")]
        args: Vec<String>,
        /// Limit to these paths (after `--`).
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Show which paths are ignored and by which rule, like `git check-ignore`.
    CheckIgnore {
        /// Show the rule that matched: `source:line:pattern<TAB>path`.
        #[arg(short = 'v', long)]
        verbose: bool,
        /// Print nothing; only set the exit code.
        #[arg(short = 'q', long)]
        quiet: bool,
        /// With -v, also show paths that are not ignored.
        #[arg(short = 'n', long = "non-matching")]
        non_matching: bool,
        /// Check tracked paths too.
        #[arg(long = "no-index")]
        no_index: bool,
        /// Read the paths from stdin, one per line.
        #[arg(long)]
        stdin: bool,
        /// Separate input and output fields with NUL.
        #[arg(short = 'z')]
        z: bool,
        /// The paths to check.
        paths: Vec<String>,
    },
    /// Print a git variable: GIT_AUTHOR_IDENT, GIT_COMMITTER_IDENT, GIT_EDITOR,
    /// GIT_SEQUENCE_EDITOR, GIT_PAGER, GIT_DEFAULT_BRANCH, GIT_SHELL_PATH, GIT_ATTR_SYSTEM,
    /// GIT_ATTR_GLOBAL, GIT_CONFIG_SYSTEM or GIT_CONFIG_GLOBAL, like `git var`.
    Var {
        /// Print the config, then every variable as `NAME=value`.
        #[arg(short = 'l', conflicts_with = "name")]
        list: bool,
        /// The variable.
        #[arg(required_unless_present = "list")]
        name: Option<String>,
    },
    /// Print where a symbolic ref points (`HEAD` -> `refs/heads/main`), point
    /// it elsewhere, or delete it, like `git symbolic-ref`.
    SymbolicRef {
        /// Shorten the ref name (`main`).
        #[arg(long)]
        short: bool,
        /// Fail without a message when the ref is not symbolic (detached HEAD).
        #[arg(short, long)]
        quiet: bool,
        /// Delete the symbolic ref.
        #[arg(short = 'd', long, conflicts_with = "target")]
        delete: bool,
        /// The reflog message for the change.
        #[arg(short = 'm', value_name = "REASON")]
        message: Option<String>,
        /// The symbolic ref, usually HEAD.
        name: String,
        /// Point it at this ref (`refs/heads/main`).
        target: Option<String>,
    },
    /// Count loose and packed objects, like `git count-objects`.
    CountObjects {
        /// Show packs, packed objects and sizes too.
        #[arg(short = 'v', long)]
        verbose: bool,
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

/// `rgit branch`'s git flags, for listing and changing branches.
#[derive(clap::Args, Default)]
pub struct BranchOpts {
    /// List remote-tracking branches too (git's -a).
    #[arg(short = 'a', long)]
    pub all: bool,
    /// List only remote-tracking branches (git's -r).
    #[arg(short = 'r', long)]
    pub remotes: bool,
    /// List branches, only those matching the given patterns (git's -l).
    #[arg(short, long)]
    pub list: bool,
    /// Show each branch's commit and subject; twice adds its upstream (-vv).
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// List only branches merged into this commit (default HEAD).
    #[arg(long, value_name = "REV", num_args = 0..=1, default_missing_value = "HEAD")]
    pub merged: Option<String>,
    /// List only branches not merged into this commit (default HEAD).
    #[arg(
        long = "no-merged",
        value_name = "REV",
        num_args = 0..=1,
        default_missing_value = "HEAD"
    )]
    pub no_merged: Option<String>,
    /// List only branches that contain this commit (default HEAD).
    #[arg(long, value_name = "REV", num_args = 0..=1, default_missing_value = "HEAD")]
    pub contains: Option<String>,
    /// List only branches that do not contain this commit (default HEAD).
    #[arg(
        long = "no-contains",
        value_name = "REV",
        num_args = 0..=1,
        default_missing_value = "HEAD"
    )]
    pub no_contains: Option<String>,
    /// List only branches that point at this commit (default HEAD).
    #[arg(
        long = "points-at",
        value_name = "REV",
        num_args = 0..=1,
        default_missing_value = "HEAD"
    )]
    pub points_at: Option<String>,
    /// Sort by a for-each-ref key (`-committerdate`, `version:refname`); the
    /// last --sort is the main key (default refname, or `branch.sort`).
    #[arg(long, value_name = "KEY")]
    pub sort: Vec<String>,
    /// Print each branch in a for-each-ref format, e.g. `%(refname:short)`.
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<String>,
    /// Match patterns and sort case-insensitively (git's -i).
    #[arg(short = 'i', long = "ignore-case")]
    pub ignore_case: bool,
    /// Accepted for git compatibility; branches list one per line.
    #[arg(long, value_name = "STYLE", num_args = 0..=1, require_equals = true, default_missing_value = "always")]
    pub column: Option<String>,
    /// Accepted for git compatibility.
    #[arg(long = "no-column")]
    pub no_column: bool,
    /// A new branch tracks its start point, or with `=inherit` the start
    /// point's own upstream (git's -t).
    #[arg(
        short = 't',
        long,
        value_name = "direct|inherit",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "direct"
    )]
    pub track: Option<String>,
    /// A new branch tracks nothing, even when started from a remote branch.
    #[arg(long = "no-track", conflicts_with = "track")]
    pub no_track: bool,
    /// Edit `[<branch>]`'s (default current) description in the editor.
    #[arg(long = "edit-description", group = "branch_action")]
    pub edit_description: bool,
    /// Accepted for git compatibility.
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Print the current branch's name (nothing when detached).
    #[arg(long = "show-current", group = "branch_action")]
    pub show_current: bool,
    /// Delete the named branches if merged into HEAD (git's -d).
    #[arg(short = 'd', long, group = "branch_action")]
    pub delete: bool,
    /// Delete the named branches even if not merged (git's -D).
    #[arg(short = 'D', group = "branch_action")]
    pub force_delete: bool,
    /// Rename `[<old>] <new>`, the current branch by default (git's -m).
    #[arg(short = 'm', long = "move", group = "branch_action")]
    pub rename: bool,
    /// Rename, replacing an existing branch named `<new>` (git's -M).
    #[arg(short = 'M', group = "branch_action")]
    pub force_rename: bool,
    /// Copy `[<old>] <new>`, the current branch by default (git's -c).
    #[arg(short = 'c', long = "copy", group = "branch_action")]
    pub copy: bool,
    /// Copy, replacing an existing branch named `<new>` (git's -C).
    #[arg(short = 'C', group = "branch_action")]
    pub force_copy: bool,
    /// Make `[<branch>]` (default current) track this upstream (git's -u).
    #[arg(
        short = 'u',
        long = "set-upstream-to",
        value_name = "UPSTREAM",
        group = "branch_action"
    )]
    pub set_upstream_to: Option<String>,
    /// Stop `[<branch>]` (default current) tracking its upstream.
    #[arg(long = "unset-upstream", group = "branch_action")]
    pub unset_upstream: bool,
    /// Create even if the branch exists, moving it; delete even if unmerged.
    #[arg(short, long)]
    pub force: bool,
    /// A branch to create and its start point (default HEAD); the branches
    /// to delete, rename, copy or track; or with -l, patterns to list.
    pub args: Vec<String>,
}

impl BranchOpts {
    /// Whether these flags list branches rather than change one.
    pub fn is_listing(&self) -> bool {
        let acts = self.show_current
            || self.delete
            || self.force_delete
            || self.rename
            || self.force_rename
            || self.copy
            || self.force_copy
            || self.set_upstream_to.is_some()
            || self.unset_upstream
            || self.edit_description;
        !acts
            && (self.args.is_empty()
                || self.list
                || self.merged.is_some()
                || self.no_merged.is_some()
                || self.contains.is_some()
                || self.no_contains.is_some()
                || self.points_at.is_some())
    }
}

/// `rgit tag`'s git flags.
#[derive(clap::Args, Default)]
pub struct TagOpts {
    /// Annotation message (implies an annotated tag).
    #[arg(short, long)]
    pub message: Option<String>,
    /// Read the annotation message from a file, `-` for stdin (git's -F).
    #[arg(short = 'F', long, value_name = "FILE", conflicts_with = "message")]
    pub file: Option<String>,
    /// Edit the message in the editor (terminal only; git's -e).
    #[arg(short, long)]
    pub edit: bool,
    /// Make an annotated tag (needs -m, or a prompt on a terminal).
    #[arg(short, long)]
    pub annotate: bool,
    /// Make a GPG-signed annotated tag (git's -s).
    #[arg(short, long)]
    pub sign: bool,
    /// Do not sign, even with `tag.gpgSign` set.
    #[arg(long = "no-sign", conflicts_with = "sign")]
    pub no_sign: bool,
    /// Sign with this key (git's -u).
    #[arg(short = 'u', long = "local-user", value_name = "KEY-ID")]
    pub local_user: Option<String>,
    /// How to clean the message: strip (default; drops `#` lines),
    /// whitespace or verbatim.
    #[arg(long, value_name = "MODE")]
    pub cleanup: Option<String>,
    /// Replace an existing tag of the same name (git's -f).
    #[arg(short, long)]
    pub force: bool,
    /// Delete the named tags (git's -d).
    #[arg(short = 'd', long)]
    pub delete: bool,
    /// Verify the named tags' GPG signatures (git's -v).
    #[arg(short = 'v', long)]
    pub verify: bool,
    /// List tags, only those matching the given patterns (git's -l).
    #[arg(short, long)]
    pub list: bool,
    /// List tags with up to N lines of their message (git's -n, default 1).
    #[arg(
        short = 'n',
        value_name = "N",
        num_args = 0..=1,
        default_missing_value = "1"
    )]
    pub lines: Option<usize>,
    /// List only tags that contain this commit (default HEAD).
    #[arg(long, value_name = "REV", num_args = 0..=1, default_missing_value = "HEAD")]
    pub contains: Option<String>,
    /// List only tags that do not contain this commit (default HEAD).
    #[arg(
        long = "no-contains",
        value_name = "REV",
        num_args = 0..=1,
        default_missing_value = "HEAD"
    )]
    pub no_contains: Option<String>,
    /// List only tags merged into this commit (default HEAD).
    #[arg(long, value_name = "REV", num_args = 0..=1, default_missing_value = "HEAD")]
    pub merged: Option<String>,
    /// List only tags not merged into this commit (default HEAD).
    #[arg(
        long = "no-merged",
        value_name = "REV",
        num_args = 0..=1,
        default_missing_value = "HEAD"
    )]
    pub no_merged: Option<String>,
    /// List only tags that point at this commit (default HEAD).
    #[arg(
        long = "points-at",
        value_name = "REV",
        num_args = 0..=1,
        default_missing_value = "HEAD"
    )]
    pub points_at: Option<String>,
    /// Sort by a for-each-ref key (`-creatordate`, `version:refname` or
    /// `-v:refname`); the last --sort is the main key (default refname, or
    /// `tag.sort`).
    #[arg(long, value_name = "KEY")]
    pub sort: Vec<String>,
    /// Print each tag in a for-each-ref format, e.g. `%(refname:short)`.
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<String>,
    /// Match patterns and sort case-insensitively (git's -i).
    #[arg(short = 'i', long = "ignore-case")]
    pub ignore_case: bool,
    /// Accepted for git compatibility; tags list one per line.
    #[arg(long, value_name = "STYLE", num_args = 0..=1, require_equals = true, default_missing_value = "always")]
    pub column: Option<String>,
    /// Accepted for git compatibility.
    #[arg(long = "no-column")]
    pub no_column: bool,
}

impl TagOpts {
    /// Whether these flags, with `names`, list tags rather than change them.
    pub fn is_listing(&self, names: &[String]) -> bool {
        !self.delete && !self.verify && (names.is_empty() || self.list || self.lines.is_some())
            || self.contains.is_some()
            || self.no_contains.is_some()
            || self.merged.is_some()
            || self.no_merged.is_some()
            || self.points_at.is_some()
    }
}

#[derive(Subcommand)]
pub enum BranchCmd {
    /// Create a branch (at HEAD, or START) and switch to it.
    Create {
        /// The new branch name.
        name: String,
        /// Where the branch starts (default HEAD).
        start: Option<String>,
    },
    /// Check out an existing branch.
    Checkout {
        /// The branch to check out.
        name: String,
    },
    /// Delete branches (multiselect prompt if no name on a terminal).
    Delete {
        /// The branches to delete.
        names: Vec<String>,
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
    /// Stash the working tree, or only some paths, with an optional message.
    Push {
        #[command(flatten)]
        push: StashPush,
        /// Stash only these paths: files, folders or globs.
        paths: Vec<String>,
    },
    /// Apply a stash and drop it (prompted for if no index on a terminal).
    Pop {
        /// The stash: `N` or `stash@{N}` (defaults to the most recent).
        #[arg(value_parser = stash_ref)]
        index: Option<usize>,
        /// Restore the staged changes too (git's --index).
        #[arg(long = "index")]
        restore_index: bool,
    },
    /// Apply a stash without dropping it.
    Apply {
        /// The stash: `N` or `stash@{N}` (defaults to the most recent).
        #[arg(value_parser = stash_ref)]
        index: Option<usize>,
        /// Restore the staged changes too (git's --index).
        #[arg(long = "index")]
        restore_index: bool,
    },
    /// Drop a stash.
    Drop {
        /// The stash: `N` or `stash@{N}` (defaults to the most recent).
        #[arg(value_parser = stash_ref)]
        index: Option<usize>,
    },
    /// List the stashes.
    List {
        /// Print each stash in a log format: `%gd` (stash@{N}), `%gs` (its
        /// message), `%H`, `%h`, `%s`, `%an`, `%ae`, `%ar`, `%ad`, `%cr`, `%n`.
        #[arg(long, alias = "pretty", value_name = "FORMAT")]
        format: Option<String>,
        /// Show at most N stashes.
        #[arg(short = 'n', long = "max-count", value_name = "N")]
        max_count: Option<usize>,
    },
    /// Show the changes a stash records, as a diffstat (-p for the patch).
    Show {
        /// The stash: `N` or `stash@{N}` (defaults to the most recent).
        #[arg(value_parser = stash_ref)]
        index: Option<usize>,
        /// Print the full unified patch (git's -p).
        #[arg(short, long)]
        patch: bool,
        /// List only the names of changed files.
        #[arg(long = "name-only")]
        name_only: bool,
        /// List the changed files with their status letter.
        #[arg(long = "name-status")]
        name_status: bool,
        /// Show added and deleted line counts per file.
        #[arg(long)]
        numstat: bool,
        /// Show a diffstat (the default).
        #[arg(long)]
        stat: bool,
        /// Show the stashed untracked files too (git's -u).
        #[arg(short = 'u', long = "include-untracked")]
        include_untracked: bool,
        /// Show only the stashed untracked files.
        #[arg(long = "only-untracked", conflicts_with = "include_untracked")]
        only_untracked: bool,
    },
    /// Make a stash commit of the local changes and print its id, without
    /// storing it or touching the working tree (git's `stash create`).
    Create {
        /// The stash message.
        message: Vec<String>,
    },
    /// Put a stash commit (from `stash create`) on the stash list.
    Store {
        /// The stash commit.
        commit: String,
        /// The message for the stash list.
        #[arg(short, long)]
        message: Option<String>,
        /// Accepted for git compatibility.
        #[arg(short = 'q', long)]
        quiet: bool,
    },
    /// Create and check out a branch at the stash's base commit, apply the
    /// stash there and drop it.
    Branch {
        /// The new branch name.
        name: String,
        /// The stash: `N` or `stash@{N}` (defaults to the most recent).
        #[arg(value_parser = stash_ref)]
        index: Option<usize>,
    },
    /// Drop every stash.
    Clear,
}

/// `stash push`'s arguments, also taken by a bare `stash`.
#[derive(clap::Args, Default)]
pub struct StashPush {
    /// A description for the stash (git's -m).
    #[arg(short, long)]
    pub message: Option<String>,
    /// Also stash untracked files (git's -u).
    #[arg(short = 'u', long = "include-untracked")]
    pub include_untracked: bool,
    /// Also stash ignored and untracked files (git's -a).
    #[arg(short = 'a', long, conflicts_with = "include_untracked")]
    pub all: bool,
    /// Leave the staged changes in the index as well (git's -k).
    #[arg(short = 'k', long = "keep-index")]
    pub keep_index: bool,
    /// With -p, reset the index too.
    #[arg(long = "no-keep-index", conflicts_with = "keep_index")]
    pub no_keep_index: bool,
    /// Stash only the staged changes, taking them out of the index and
    /// working tree (git's -S).
    #[arg(short = 'S', long, conflicts_with_all = ["patch", "include_untracked", "all"])]
    pub staged: bool,
    /// Pick the hunks to stash, one by one (needs a terminal; git's -p).
    #[arg(short = 'p', long, conflicts_with_all = ["include_untracked", "all"])]
    pub patch: bool,
    /// Read the paths to stash from this file, one per line (`-` for stdin).
    #[arg(long = "pathspec-from-file", value_name = "FILE")]
    pub pathspec_from_file: Option<String>,
    /// With --pathspec-from-file, paths are NUL-separated.
    #[arg(long = "pathspec-file-nul", requires = "pathspec_from_file")]
    pub pathspec_file_nul: bool,
    /// Accepted for git compatibility.
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

/// `bisect` subcommands.
#[derive(Subcommand)]
pub enum BisectCmd {
    /// Start a bisect, optionally with the bad commit and good ones.
    Start {
        /// The bad (new) commit, then good (old) ones.
        revs: Vec<String>,
        /// The word for the new state instead of `bad` (e.g. `fixed`).
        #[arg(long = "term-new", visible_alias = "term-bad", value_name = "TERM")]
        term_new: Option<String>,
        /// The word for the old state instead of `good` (e.g. `broken`).
        #[arg(long = "term-old", visible_alias = "term-good", value_name = "TERM")]
        term_old: Option<String>,
        /// Leave the working tree alone; move BISECT_HEAD to each step instead.
        #[arg(long = "no-checkout")]
        no_checkout: bool,
        /// Follow only the first parent of merge commits.
        #[arg(long = "first-parent")]
        first_parent: bool,
        /// Only test commits that touch these paths (after `--`).
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Mark commits (HEAD by default) as bad: the change is there.
    Bad { revs: Vec<String> },
    /// Mark commits (HEAD by default) as good: the change is not there yet.
    Good { revs: Vec<String> },
    /// Mark commits as new (with `--term-new`/`--term-old` terms).
    New { revs: Vec<String> },
    /// Mark commits as old (with `--term-new`/`--term-old` terms).
    Old { revs: Vec<String> },
    /// Skip commits (HEAD by default) or ranges `A..B` that cannot be tested.
    Skip { revs: Vec<String> },
    /// End the bisect and check out where it started (or `commit`).
    Reset { commit: Option<String> },
    /// Print the bisect log, to save for `replay`.
    Log,
    /// Redo the bisect a saved log records.
    Replay { file: String },
    /// Mark each step by running a command: exit 0 good, 125 skip, other bad.
    Run {
        #[arg(
            required = true,
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "CMD"
        )]
        cmd: Vec<String>,
    },
    /// List the commits still in the search.
    #[command(visible_alias = "view")]
    Visualize,
    /// Print the terms for the old and new states.
    Terms {
        /// Print only the term for good (old) commits.
        #[arg(long = "term-good", visible_alias = "term-old")]
        good: bool,
        /// Print only the term for bad (new) commits.
        #[arg(long = "term-bad", visible_alias = "term-new", conflicts_with = "good")]
        bad: bool,
    },
    /// Mark commits with a custom term set by `start --term-new/--term-old`.
    #[command(external_subcommand)]
    Mark(Vec<String>),
}

impl BisectCmd {
    /// The `git bisect` arguments for this subcommand.
    fn git_args(&self) -> Vec<String> {
        let words = |cmd: &str, rest: &[String]| {
            std::iter::once(cmd.to_owned())
                .chain(rest.iter().cloned())
                .collect::<Vec<_>>()
        };
        match self {
            BisectCmd::Start {
                revs,
                term_new,
                term_old,
                no_checkout,
                first_parent,
                paths,
            } => {
                let mut args = vec!["start".to_owned()];
                args.extend(term_new.iter().map(|t| format!("--term-new={t}")));
                args.extend(term_old.iter().map(|t| format!("--term-old={t}")));
                if *no_checkout {
                    args.push("--no-checkout".into());
                }
                if *first_parent {
                    args.push("--first-parent".into());
                }
                args.extend(revs.iter().cloned());
                args.push("--".into());
                args.extend(paths.iter().cloned());
                args
            }
            BisectCmd::Bad { revs } => words("bad", revs),
            BisectCmd::Good { revs } => words("good", revs),
            BisectCmd::New { revs } => words("new", revs),
            BisectCmd::Old { revs } => words("old", revs),
            BisectCmd::Skip { revs } => words("skip", revs),
            BisectCmd::Reset { commit } => words("reset", commit.as_slice()),
            BisectCmd::Log => words("log", &[]),
            BisectCmd::Replay { file } => words("replay", std::slice::from_ref(file)),
            BisectCmd::Run { cmd } => words("run", cmd),
            BisectCmd::Visualize => words("visualize", &[]),
            BisectCmd::Terms { good, bad } => {
                let flag = match (good, bad) {
                    (true, _) => vec!["--term-good".to_owned()],
                    (_, true) => vec!["--term-bad".to_owned()],
                    _ => Vec::new(),
                };
                words("terms", &flag)
            }
            BisectCmd::Mark(args) => args.clone(),
        }
    }
}

/// `rebase` flags handed as they are to git's sequencer.
#[derive(clap::Args, Default)]
pub struct RebaseFlags {
    /// Keep commits that start out empty.
    #[arg(long = "keep-empty")]
    pub keep_empty: bool,
    /// Do not move `fixup!`/`squash!` commits (overrides rebase.autoSquash).
    #[arg(long = "no-autosquash", conflicts_with = "autosquash")]
    pub no_autosquash: bool,
    /// Replay every commit, even ones that could be kept as they are.
    #[arg(short = 'f', long = "force-rebase", visible_alias = "no-ff")]
    pub force_rebase: bool,
    /// Refine the upstream with its reflog (git's merge-base --fork-point).
    #[arg(long = "fork-point")]
    pub fork_point: bool,
    /// Use the upstream as it is, without its reflog.
    #[arg(long = "no-fork-point", conflicts_with = "fork_point")]
    pub no_fork_point: bool,
    /// Keep the base: replay onto the merge base of the upstream and the branch.
    #[arg(long = "keep-base")]
    pub keep_base: bool,
    /// Give each commit its author date as the committer date.
    #[arg(long = "committer-date-is-author-date")]
    pub committer_date_is_author_date: bool,
    /// Give each commit the current time as its author date.
    #[arg(long = "reset-author-date", visible_alias = "ignore-date")]
    pub reset_author_date: bool,
    /// Recreate merge commits instead of flattening them.
    #[arg(short = 'r', long = "rebase-merges", value_name = "MODE", num_args = 0..=1,
          require_equals = true, default_missing_value = "no-rebase-cousins",
          value_parser = ["rebase-cousins", "no-rebase-cousins"])]
    pub rebase_merges: Option<String>,
    /// What to do with a commit that becomes empty.
    #[arg(long, value_parser = ["drop", "keep", "stop"])]
    pub empty: Option<String>,
    /// Apply every commit, even ones already upstream.
    #[arg(long = "reapply-cherry-picks")]
    pub reapply_cherry_picks: bool,
    /// Add a Signed-off-by trailer to each commit.
    #[arg(long)]
    pub signoff: bool,
    /// Stash local changes first and restore them after.
    #[arg(long)]
    pub autostash: bool,
    /// Skip the pre-rebase hook.
    #[arg(long = "no-verify")]
    pub no_verify: bool,
    /// Print nothing on success.
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Show a diffstat of what changed upstream.
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

impl RebaseFlags {
    fn git_flags(&self) -> Vec<String> {
        let mut flags: Vec<String> = [
            (self.keep_empty, "--keep-empty"),
            (self.no_autosquash, "--no-autosquash"),
            (self.force_rebase, "--force-rebase"),
            (self.fork_point, "--fork-point"),
            (self.no_fork_point, "--no-fork-point"),
            (self.keep_base, "--keep-base"),
            (
                self.committer_date_is_author_date,
                "--committer-date-is-author-date",
            ),
            (self.reset_author_date, "--reset-author-date"),
            (self.reapply_cherry_picks, "--reapply-cherry-picks"),
            (self.signoff, "--signoff"),
            (self.autostash, "--autostash"),
            (self.no_verify, "--no-verify"),
            (self.quiet, "--quiet"),
            (self.verbose, "--verbose"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, flag)| flag.to_owned())
        .collect();
        flags.extend(
            self.rebase_merges
                .iter()
                .map(|m| format!("--rebase-merges={m}")),
        );
        flags.extend(self.empty.iter().map(|e| format!("--empty={e}")));
        flags
    }
}

/// A stash as git names it: `N` or `stash@{N}`.
pub(crate) fn stash_ref(s: &str) -> Result<usize, String> {
    s.strip_prefix("stash@{")
        .and_then(|r| r.strip_suffix('}'))
        .unwrap_or(s)
        .parse()
        .map_err(|_| format!("expected N or stash@{{N}}, got {s:?}"))
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
        /// Fetch the remote right after adding it (git's -f).
        #[arg(short = 'f', long)]
        fetch: bool,
        /// Track only this branch; repeat for more (git's -t).
        #[arg(short = 't', long = "track", value_name = "BRANCH")]
        track: Vec<String>,
        /// Point `<name>/HEAD` at this branch (git's -m).
        #[arg(short = 'm', long = "master", value_name = "BRANCH")]
        master: Option<String>,
        /// Mirror the remote: `fetch` copies every ref as is, `push` pushes
        /// every ref; both without a value.
        #[arg(
            long,
            value_name = "fetch|push",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "both"
        )]
        mirror: Option<String>,
        /// Fetch every tag from it.
        #[arg(long)]
        tags: bool,
        /// Fetch no tags from it.
        #[arg(long = "no-tags", conflicts_with = "tags")]
        no_tags: bool,
    },
    /// Remove a remote.
    #[command(alias = "rm")]
    Remove {
        /// The remote name.
        name: String,
    },
    /// Change a remote's URL, add one (--add) or delete those matching a
    /// regex (--delete).
    SetUrl {
        /// The remote name.
        name: String,
        /// The new URL; with --delete, a regex of the URLs to delete.
        url: String,
        /// Replace only the URL matching this regex.
        #[arg(conflicts_with_all = ["add", "delete"])]
        old: Option<String>,
        /// Set the push URL instead (git's --push).
        #[arg(long)]
        push: bool,
        /// Add the URL, keeping the others.
        #[arg(long, conflicts_with = "delete")]
        add: bool,
        /// Delete the URLs matching the regex.
        #[arg(long)]
        delete: bool,
    },
    /// Print a remote's URL.
    GetUrl {
        /// The remote name.
        name: String,
        /// Print the push URL instead (git's --push).
        #[arg(long)]
        push: bool,
        /// Print every URL, not just the first (git's --all).
        #[arg(long)]
        all: bool,
    },
    /// Rename a remote, its tracking refs and the branches that track it.
    Rename {
        /// The current remote name.
        old: String,
        /// The new remote name.
        new: String,
    },
    /// Delete remote-tracking branches that no longer exist on the remotes.
    Prune {
        /// The remotes to prune.
        #[arg(required = true)]
        names: Vec<String>,
        /// Only list what would be pruned (git's -n).
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
    },
    /// Describe remotes as `git remote show` does: URLs, HEAD branch, remote
    /// branches, and the local branches that pull from and push to them.
    Show {
        /// The remotes (all when none).
        names: Vec<String>,
        /// Do not query the remotes (git's -n).
        #[arg(short = 'n')]
        no_query: bool,
    },
    /// Fetch remotes: all by default, or the named remotes and groups
    /// (`remotes.<group>`).
    Update {
        /// Remotes or groups to fetch.
        names: Vec<String>,
        /// Prune stale remote-tracking branches too (git's -p).
        #[arg(short = 'p', long)]
        prune: bool,
    },
    /// Set `<name>/HEAD`: to a branch, from the remote (-a), or delete it (-d).
    SetHead {
        /// The remote name.
        name: String,
        /// The remote branch HEAD should name.
        #[arg(required_unless_present_any = ["auto", "delete"])]
        branch: Option<String>,
        /// Ask the remote which branch its HEAD names (git's -a).
        #[arg(short = 'a', long, conflicts_with_all = ["branch", "delete"])]
        auto: bool,
        /// Delete `<name>/HEAD` (git's -d).
        #[arg(short = 'd', long, conflicts_with = "branch")]
        delete: bool,
    },
    /// Fetch only these branches from a remote (with --add, these too).
    SetBranches {
        /// The remote name.
        name: String,
        /// The branches to fetch.
        #[arg(required = true)]
        branches: Vec<String>,
        /// Add to the branches fetched instead of replacing them.
        #[arg(long)]
        add: bool,
    },
}

#[derive(Subcommand)]
pub enum SubmoduleCmd {
    /// Show each submodule's commit, path and name for it: ` ` in step,
    /// `-` not initialized, `+` another commit checked out, `U` conflicts.
    Status {
        /// Show the commit the superproject records instead of the checked-out one.
        #[arg(long)]
        cached: bool,
        /// Include nested submodules.
        #[arg(long)]
        recursive: bool,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
        /// Only these submodule paths.
        paths: Vec<String>,
    },
    /// Clone URL into PATH (default: the URL's name) and add it as a submodule.
    Add {
        /// The repository to add.
        url: String,
        /// Where to put it.
        path: Option<String>,
        /// Check out and track this branch.
        #[arg(short = 'b', long)]
        branch: Option<String>,
        /// Name the submodule this instead of its path.
        #[arg(long)]
        name: Option<String>,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Register submodules in .git/config from .gitmodules.
    Init {
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
        /// Only these submodule paths.
        paths: Vec<String>,
    },
    /// Clone missing submodules and check out the commits the superproject records.
    Update {
        /// Initialize submodules that are not yet.
        #[arg(long)]
        init: bool,
        /// Update nested submodules too.
        #[arg(long)]
        recursive: bool,
        /// Check out the submodule's remote-tracking branch instead.
        #[arg(long)]
        remote: bool,
        /// Check out detached (the default; accepted for git compatibility).
        #[arg(long)]
        checkout: bool,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
        /// Parallel clones (accepted; cloned in turn).
        #[arg(short = 'j', long, value_name = "N")]
        jobs: Option<usize>,
        /// Only these submodule paths.
        paths: Vec<String>,
    },
    /// Copy submodule URLs from .gitmodules into the config and the submodules.
    Sync {
        /// Sync nested submodules too.
        #[arg(long)]
        recursive: bool,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
        /// Only these submodule paths.
        paths: Vec<String>,
    },
    /// Unregister submodules and empty their folders.
    Deinit {
        /// Discard local changes in them.
        #[arg(short = 'f', long)]
        force: bool,
        /// Every submodule (required when no path is given).
        #[arg(long)]
        all: bool,
        /// Print nothing unless an error occurs.
        #[arg(short, long)]
        quiet: bool,
        /// Only these submodule paths.
        paths: Vec<String>,
    },
    /// Run a shell command in each checked-out submodule ($name, $sm_path,
    /// $displaypath, $sha1 and $toplevel are set).
    Foreach {
        /// Include nested submodules.
        #[arg(long)]
        recursive: bool,
        /// Do not print `Entering '<path>'`.
        #[arg(short, long)]
        quiet: bool,
        /// The command.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Summarize the commits between recorded and checked-out submodule commits.
    Summary {
        /// git's `submodule summary` arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Change a submodule's URL in .gitmodules and sync it.
    SetUrl {
        /// The submodule's path.
        path: String,
        /// The new URL.
        url: String,
    },
    /// Set (or with -d, clear) the branch `update --remote` follows.
    SetBranch {
        /// The branch to follow.
        #[arg(short = 'b', long, required_unless_present = "default")]
        branch: Option<String>,
        /// Follow the remote's default branch again.
        #[arg(short = 'd', long, conflicts_with = "branch")]
        default: bool,
        /// The submodule's path.
        path: String,
    },
    /// Move submodules' repositories into the superproject's .git/modules.
    Absorbgitdirs {
        /// Only these submodule paths.
        paths: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum WorktreeCmd {
    /// Add a linked worktree at PATH, as `git worktree add` does: on BRANCH,
    /// on a new branch with -b, detached at a commit or with --detach, or by
    /// default on a branch named after PATH's last folder.
    Add {
        /// The path for the new worktree.
        path: String,
        /// The branch to check out there, or the commit to start from.
        #[arg(value_name = "BRANCH")]
        commitish: Option<String>,
        /// Create this branch at BRANCH (default HEAD) and check it out there.
        #[arg(
            short = 'b',
            value_name = "NEW_BRANCH",
            conflicts_with = "reset_branch"
        )]
        new_branch: Option<String>,
        /// Like -b, but reset the branch to BRANCH if it exists.
        #[arg(short = 'B', value_name = "NEW_BRANCH")]
        reset_branch: Option<String>,
        /// Check out a detached HEAD (git's --detach).
        #[arg(short = 'd', long, conflicts_with_all = ["new_branch", "reset_branch", "orphan"])]
        detach: bool,
        /// Start an empty, unborn branch (-b's, or named after PATH).
        #[arg(long)]
        orphan: bool,
        /// Check out the branch even if another worktree has it.
        #[arg(short, long)]
        force: bool,
        /// Leave the new worktree's files and index empty.
        #[arg(long = "no-checkout")]
        no_checkout: bool,
        /// Lock the new worktree (see `worktree lock`).
        #[arg(long)]
        lock: bool,
        /// Why it is locked (with --lock).
        #[arg(long, requires = "lock")]
        reason: Option<String>,
        /// A new branch tracks BRANCH.
        #[arg(long, conflicts_with = "no_track")]
        track: bool,
        /// A new branch tracks nothing, even when BRANCH is remote.
        #[arg(long = "no-track")]
        no_track: bool,
        /// Accepted for git compatibility.
        #[arg(short, long)]
        quiet: bool,
    },
    /// List the worktrees, as `git worktree list` prints them.
    List {
        /// git's stable format, one `key value` line per field.
        #[arg(long)]
        porcelain: bool,
        /// With --porcelain, end lines with NUL.
        #[arg(short = 'z', requires = "porcelain")]
        z: bool,
        /// Show lock and prune reasons.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Fix the links between worktrees and the repository after moving
    /// either; name worktrees moved by hand.
    Repair {
        /// Worktrees now at these paths.
        paths: Vec<String>,
    },
    /// Remove a linked worktree (by name or path) and its folder.
    Remove {
        /// The worktree's name or path.
        name: String,
        /// Remove even with changes or when locked (git's -f/--force).
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Lock a worktree (by name or path) so prune leaves it alone.
    Lock {
        /// The worktree's name or path.
        name: String,
        /// Why it is locked.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Unlock a worktree (by name or path).
    Unlock {
        /// The worktree's name or path.
        name: String,
    },
    /// Move a worktree (by name or path) to a new path.
    Move {
        /// The worktree's name or path.
        name: String,
        /// The new path.
        new_path: String,
    },
    /// Prune worktree entries whose working tree is gone (git's `worktree prune`).
    Prune {
        /// Only list what would be pruned (git's -n).
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
        /// Accepted for git compatibility; the pruned names are printed.
        #[arg(short, long)]
        verbose: bool,
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

/// A commit `--date`: `@<unix>`, `<unix>` or an ISO date, each with an
/// optional `+HHMM`, `+HH:MM` or `Z` offset (default UTC), as unix seconds and
/// the offset in minutes.
pub(crate) fn parse_git_date(s: &str) -> anyhow::Result<(i64, i32)> {
    let s = s.trim();
    let (rest, offset) = match s.strip_suffix('Z') {
        Some(rest) => (rest, 0),
        None => {
            let tz = s
                .rfind(['+', '-'])
                .map(|i| (&s[..i], s[i..].replace(':', "")));
            match tz {
                Some((rest, tz))
                    if tz.len() == 5 && tz[1..].bytes().all(|b| b.is_ascii_digit()) =>
                {
                    let (h, m): (i32, i32) = (tz[1..3].parse()?, tz[3..].parse()?);
                    let sign = if tz.starts_with('-') { -1 } else { 1 };
                    (rest, sign * (h * 60 + m))
                }
                _ => (s, 0),
            }
        }
    };
    let rest = rest.trim();
    let unix = rest.strip_prefix('@').unwrap_or(rest);
    if let Ok(secs) = unix.parse::<i64>() {
        return Ok((secs, offset));
    }
    Ok((parse_date(rest)? - i64::from(offset) * 60, offset))
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
        "rgit stash",
        &["save"],
        "use `rgit stash push -m <message>`",
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
            "add",
            "discard",
            "restore",
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
            "branch", "checkout", "switch", "merge", "rebase", "tag", "stash", "bisect", "prune",
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
        &["fetch", "pull", "push", "ls-remote", "remote", "forge"],
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
    (
        "Plumbing (git's own output formats)",
        &[
            "rev-parse",
            "ls-files",
            "ls-tree",
            "cat-file",
            "show-ref",
            "for-each-ref",
            "rev-list",
            "merge-base",
            "reflog",
            "shortlog",
            "grep",
            "check-ignore",
            "var",
            "symbolic-ref",
            "count-objects",
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
        Command::Plumbing(c) => crate::plumbing::run(backend, c, true)?.text,
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
        Command::Add {
            paths,
            all,
            update,
            force,
            dry_run,
            verbose,
            intent_to_add,
            ignore_errors,
            patch,
        } => {
            if patch {
                return crate::interactive::patch(
                    backend,
                    interactive,
                    crate::interactive::PatchMode::Stage,
                    &paths,
                );
            }
            if paths.is_empty() && !all && !update {
                return Err(anyhow::Error::new(CliError {
                    message: "nothing specified, nothing added".to_owned(),
                    help: Some("Run `rgit add .` or `rgit add -A` to add everything".to_owned()),
                    code: 2,
                }));
            }
            if intent_to_add {
                backend.intent_to_add(&paths)?;
                return Ok("ok".to_owned());
            }
            // git's `add -n`/`-v` lines: what the add changes in the index.
            let specs: Vec<String> = paths
                .iter()
                .map(|p| if p == "." { "*".to_owned() } else { p.clone() })
                .collect();
            let lines: Vec<String> = backend
                .status()?
                .entries
                .into_iter()
                .filter(|e| !matches!(e.worktree, rgit_git::StatusCode::Unmodified))
                .filter(|e| !(update && e.is_untracked()))
                .filter(|e| specs.is_empty() || rgit_git::pathspec_matches(&specs, &e.path))
                .map(|e| match e.worktree {
                    rgit_git::StatusCode::Deleted => format!("remove '{}'", e.path),
                    _ => format!("add '{}'", e.path),
                })
                .collect();
            if !dry_run {
                if ignore_errors && paths.len() > 1 {
                    let failed: Vec<String> = paths
                        .iter()
                        .filter_map(|p| {
                            backend
                                .add(std::slice::from_ref(p), update, force)
                                .err()
                                .map(|e| e.to_string())
                        })
                        .collect();
                    if !failed.is_empty() {
                        anyhow::bail!("{}", failed.join("\n"));
                    }
                } else {
                    backend.add(&paths, update, force)?;
                }
            }
            if dry_run || verbose {
                lines.join("\n")
            } else {
                "ok".to_owned()
            }
        }
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
        Command::Restore {
            paths,
            source,
            staged,
            worktree,
            ours,
            theirs,
            overlay,
            patch,
            ..
        } => {
            if patch {
                use crate::interactive::PatchMode;
                let mode = if staged && !worktree {
                    PatchMode::Unstage
                } else if !staged {
                    PatchMode::Discard
                } else {
                    anyhow::bail!("restore -p restores the index or the working tree, not both");
                };
                return crate::interactive::patch(backend, interactive, mode, &paths);
            }
            if ours || theirs {
                ok(backend.checkout_side(&paths, ours))?
            } else {
                ok(backend.restore(
                    &paths,
                    source.as_deref(),
                    staged,
                    worktree || !staged,
                    overlay,
                ))?
            }
        }
        Command::Resolve { path, ours, theirs } => {
            if !ours && !theirs {
                anyhow::bail!("resolve needs --ours or --theirs");
            }
            ok(backend.resolve_conflict(&path, ours))?
        }
        Command::Commit {
            message,
            file,
            amend,
            no_edit,
            all,
            no_verify,
            author,
            signoff,
            allow_empty,
            fixup,
            squash,
            edit,
            reuse_message,
            reedit_message,
            reset_author,
            date,
            dry_run,
            include,
            quiet: _,
            only: _,
            verbose: _,
            mut paths,
        } => {
            let reuse = reuse_message.as_ref().or(reedit_message.as_ref());
            let mut text = match (file.as_deref(), reuse) {
                (Some("-"), _) => std::io::read_to_string(std::io::stdin())?,
                (Some(f), _) => std::fs::read_to_string(f)
                    .map_err(|e| anyhow::anyhow!("could not read {f}: {e}"))?,
                (None, Some(rev)) => backend.commit_overview(rev)?.message,
                (None, None) => message.join("\n\n"),
            };
            let target = fixup
                .map(|r| ("fixup", r))
                .or(squash.map(|r| ("squash", r)));
            if let Some((kind, rev)) = target {
                let old = backend.commit_overview(&rev)?.message;
                let head = format!("{kind}! {}", old.lines().next().unwrap_or(""));
                text = if text.is_empty() {
                    head
                } else {
                    format!("{head}\n\n{text}")
                };
            }
            if text.is_empty() && amend && (no_edit || edit) {
                text = backend.head_message().unwrap_or_default();
            }
            // Like git: a merge, squash or stopped pick prepared the message.
            if text.is_empty() && !amend {
                text = backend.prepared_message().unwrap_or_default();
            }
            // -e always opens the editor, -c only on a terminal, as git would
            // have no one to edit for.
            if edit || (reedit_message.is_some() && !no_edit && interactive) {
                text = crate::interactive::edit_message(backend, &text)?;
            }
            if dry_run {
                let status = backend.status()?;
                if status.staged.is_empty() && !all && paths.is_empty() && !allow_empty {
                    anyhow::bail!("nothing to commit");
                }
                return Ok(render::status(&status));
            }
            let message = resolve(
                (!text.trim().is_empty()).then_some(text),
                "a commit message",
                &|| crate::interactive::input("Commit message"),
            )?;
            // -a: stage worktree changes to tracked files (not untracked ones).
            if all {
                backend.add(&[], true, false)?;
            }
            if include {
                backend.add(&paths, true, false)?;
                paths.clear();
            }
            backend.commit_with(
                &message,
                &rgit_git::CommitOptions {
                    amend,
                    no_verify,
                    allow_empty,
                    signoff,
                    author,
                    author_from: reuse.cloned(),
                    reset_author,
                    date: date.as_deref().map(parse_git_date).transpose()?,
                    paths,
                },
            )?;
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
            multiple,
            unshallow,
            deepen,
            shallow_since,
            prune_tags,
            no_tags,
            force,
            refmap,
            set_upstream,
            quiet,
            verbose: _,
            jobs: _,
        } => {
            let args = rgit_git::FetchArgs {
                all,
                prune,
                tags,
                depth,
                dry_run,
                unshallow,
                deepen,
                shallow_since,
                prune_tags,
                no_tags,
                force,
                refmap: (!refmap.is_empty())
                    .then(|| refmap.into_iter().filter(|m| !m.is_empty()).collect()),
                set_upstream,
            };
            let out = if multiple {
                let mut out = Vec::new();
                for name in remote.into_iter().chain(repository).chain(refspecs) {
                    out.push(net(interactive, "fetch", |r| {
                        backend.fetch(Some(&name), &[], &args, r)
                    })?);
                }
                out.join("\n")
            } else {
                let (remote, refspecs) = remote_and_refspecs(remote, repository, refspecs);
                net(interactive, "fetch", |r| {
                    backend.fetch(remote.as_deref(), &refspecs, &args, r)
                })?
            };
            if quiet { String::new() } else { out }
        }
        Command::Pull {
            repository,
            branch,
            rebase,
            no_rebase,
            ff_only,
            no_ff,
            squash,
            no_commit,
            autostash,
            no_autostash,
            strategy_option,
            all,
            depth,
            quiet,
            verbose: _,
        } => {
            let args = rgit_git::PullArgs {
                rebase: (rebase || no_rebase).then_some(rebase),
                ff_only,
                no_ff,
                squash,
                no_commit,
                autostash: (autostash || no_autostash).then_some(autostash),
                strategy_option,
                all,
                depth,
            };
            let out = net(interactive, "pull", |r| {
                backend.pull(repository.as_deref(), branch.as_deref(), &args, r)
            })?;
            if quiet { String::new() } else { out }
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
            follow_tags,
            atomic,
            prune,
            mirror,
            push_option,
            no_verify,
            porcelain,
            recurse_submodules: _,
            quiet,
            verbose,
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
                follow_tags,
                atomic,
                prune,
                mirror,
                push_options: push_option,
                no_verify,
                porcelain,
                verbose,
            };
            let out = net(interactive, "push", |r| {
                backend.push_to(remote.as_deref(), &refspecs, &args, r)
            })?;
            if quiet { String::new() } else { out }
        }
        Command::Checkout {
            rev,
            pathspec,
            branch,
            force_branch,
            detach,
            track,
            no_track,
            force,
            merge,
            conflict,
            orphan,
            ours,
            theirs,
            no_guess,
            guess: _,
            patch,
            quiet: _,
            paths,
        } => {
            let (rev, paths) = rev_and_paths(rev, pathspec, paths, |r| {
                r == "-" || backend.rev_parse(r).is_ok() || guess_remote(backend, r).is_some()
            });
            if patch {
                if let Some(rev) = rev {
                    anyhow::bail!(
                        "checkout -p from a revision ({rev}) is not supported; run `rgit checkout -p` to discard working-tree hunks"
                    );
                }
                return crate::interactive::patch(
                    backend,
                    interactive,
                    crate::interactive::PatchMode::Discard,
                    &paths,
                );
            }
            if ours || theirs {
                if paths.is_empty() {
                    anyhow::bail!("--ours/--theirs needs paths");
                }
                backend.checkout_side(&paths, ours)?;
                return Ok(format!("checked out {} from the conflict", paths.join(" ")));
            }
            if !paths.is_empty() {
                // `checkout [<rev>] -- <paths>`: take the paths from <rev> into
                // the index and working tree, or from the index.
                backend.restore(&paths, rev.as_deref(), rev.is_some(), true, true)?;
                let from = rev.as_deref().unwrap_or("the index");
                return Ok(format!("restored {} from {from}", paths.join(" ")));
            }
            if let Some(name) = orphan {
                backend.checkout_orphan(&name, Some(rev.as_deref().unwrap_or("HEAD")))?;
                return Ok(format!("switched to a new branch {name} with no history"));
            }
            let new = branch
                .map(|b| (b, false))
                .or(force_branch.map(|b| (b, true)));
            let rev = match rev {
                // `checkout -f`/`-m` alone re-checks out the current branch.
                None if new.is_none() && !detach && (force || merge) => Some(
                    backend
                        .status()?
                        .head
                        .branch
                        .unwrap_or_else(|| "HEAD".to_owned()),
                ),
                None if new.is_none() && !detach => {
                    Some(resolve(None, "a branch or revision", &|| {
                        crate::interactive::pick_branch(backend, "Check out which branch?")
                    })?)
                }
                rev => rev,
            };
            let opts = SwitchOpts {
                detach,
                track,
                no_track,
                guess: !no_guess,
                mode: checkout_mode(force, merge, conflict.as_deref()),
                detach_ok: true,
            };
            switch(backend, rev, new, &opts)?
        }
        Command::Switch {
            rev,
            create,
            force_create,
            detach,
            track,
            no_track,
            discard_changes,
            merge,
            conflict,
            orphan,
            no_guess,
            guess: _,
            quiet: _,
        } => {
            if let Some(name) = orphan {
                backend.checkout_orphan(&name, None)?;
                return Ok(format!("switched to a new branch {name} with no history"));
            }
            let new = create
                .map(|b| (b, false))
                .or(force_create.map(|b| (b, true)));
            let rev = match rev {
                None if new.is_none() && !detach => Some(resolve(None, "a branch", &|| {
                    crate::interactive::pick_branch(backend, "Switch to which branch?")
                })?),
                rev => rev,
            };
            let opts = SwitchOpts {
                detach,
                track,
                no_track,
                guess: !no_guess,
                mode: checkout_mode(discard_changes, merge, conflict.as_deref()),
                detach_ok: false,
            };
            switch(backend, rev, new, &opts)?
        }
        Command::Merge {
            mut revs,
            no_ff,
            ff_only,
            squash,
            no_commit,
            message,
            file,
            strategy_option,
            strategy,
            allow_unrelated_histories,
            log,
            stat: _,
            no_stat,
            edit,
            no_edit: _,
            no_verify,
            verify: _,
            signoff,
            quiet,
            cont,
            abort,
            quit,
        } => {
            if abort {
                ok(backend.merge_abort())?
            } else if cont {
                ok(backend.merge_continue())?
            } else if quit {
                ok(backend.merge_quit())?
            } else {
                if revs.is_empty() {
                    revs.push(resolve(None, "a revision to merge", &|| {
                        crate::interactive::pick_branch(backend, "Merge which branch?")
                    })?);
                }
                let message = match file.as_deref() {
                    Some(f) => Some(String::from_utf8_lossy(&read_input(f)?).into_owned()),
                    None => message,
                };
                let opts = rgit_git::MergeOptions {
                    no_ff,
                    ff_only,
                    squash,
                    no_commit,
                    message,
                    strategy_option,
                    strategy,
                    allow_unrelated: allow_unrelated_histories,
                    log,
                    edit,
                    no_verify,
                    signoff,
                    stat: !no_stat && !quiet,
                };
                let out = net(interactive, "merge", |r| {
                    backend.merge_with(&revs, &opts, r)
                })?;
                if quiet { "ok".to_owned() } else { out }
            }
        }
        Command::Rebase {
            onto,
            branch,
            onto_new,
            edit,
            root,
            autosquash,
            exec,
            update_refs,
            strategy_option,
            more,
            cont,
            skip,
            abort,
            quit,
            edit_todo,
            show_current_patch,
        } => {
            if abort {
                ok(backend.rebase_abort())?
            } else if cont {
                ok(backend.rebase_continue())?
            } else if skip {
                ok(backend.rebase_skip())?
            } else if quit {
                ok(backend.rebase_quit())?
            } else if edit_todo {
                if !interactive {
                    anyhow::bail!("rebase --edit-todo needs a terminal");
                }
                ok(backend.rebase_edit_todo())?
            } else if show_current_patch {
                if backend.rev_parse("REBASE_HEAD").is_err() {
                    anyhow::bail!("no rebase in progress");
                }
                show_one(backend, "REBASE_HEAD", &[], DiffFormat::default(), false)?
            } else {
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
                // git's own sequencer, so a conflict stops for --continue as in
                // git; no upstream argument means the branch's upstream.
                let opts = rgit_git::RebaseOptions {
                    onto: onto_new,
                    interactive: edit,
                    root,
                    autosquash,
                    exec,
                    update_refs,
                    strategy_option,
                    branch,
                    flags: more.git_flags(),
                };
                let out = backend.rebase_with(onto.as_deref(), &opts)?;
                if out.is_empty() || more.quiet {
                    "ok".to_owned()
                } else {
                    out
                }
            }
        }
        Command::Undo => format!("undid {}", backend.undo()?),
        Command::Redo => format!("redid {}", backend.redo()?),
        Command::Oplog => render::oplog(&backend.oplog()?),
        Command::Smartlog => render::smartlog(&backend.smartlog()?),
        Command::Bisect { cmd } => bisect(backend, cmd)?.0,
        Command::Reset {
            rev,
            pathspec,
            soft,
            mixed: _,
            hard,
            keep,
            merge,
            patch,
            quiet,
            paths,
        } => {
            let (rev, paths) =
                rev_and_paths(rev, pathspec, paths, |r| backend.rev_parse(r).is_ok());
            if patch {
                if let Some(rev) = rev.filter(|r| r != "HEAD") {
                    anyhow::bail!(
                        "reset -p to a revision ({rev}) is not supported; run `rgit reset -p` to unstage hunks"
                    );
                }
                return crate::interactive::patch(
                    backend,
                    interactive,
                    crate::interactive::PatchMode::Unstage,
                    &paths,
                );
            }
            let done = |text: String| if quiet { String::new() } else { text };
            // `reset [<rev>] [--] <paths>` resets those index entries to <rev>
            // (default HEAD), leaving HEAD and the working tree alone.
            if !paths.is_empty() {
                let Some(rev) = rev else {
                    for p in &paths {
                        backend.unstage_file(p)?;
                    }
                    return Ok(done(format!("unstaged {}", paths.join(", "))));
                };
                backend.reset_paths(&rev, &paths)?;
                return Ok(done(format!("reset {} to {rev}", paths.join(", "))));
            }
            let rev = match rev {
                None if merge || keep || hard || !interactive => "HEAD".to_owned(),
                rev => resolve(rev, "a revision to reset to", &|| {
                    crate::interactive::pick_commit(backend, "Reset to which commit?")
                })?,
            };
            let mode = match (soft, hard, keep, merge) {
                (true, ..) => ResetMode::Soft,
                (_, true, ..) => ResetMode::Hard,
                (_, _, true, _) => ResetMode::Keep,
                (.., true) => ResetMode::Merge,
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
                done(ok(backend.reset(&rev, mode))?)
            }
        }
        Command::CherryPick {
            revs,
            no_commit,
            record_origin,
            mainline,
            strategy_option,
            edit,
            no_edit: _,
            signoff,
            ff,
            allow_empty,
            keep_redundant_commits,
            empty,
            cont,
            skip,
            abort,
            quit,
        } => {
            let empty = match empty.as_deref() {
                Some("drop") => rgit_git::EmptyCommit::Drop,
                Some("keep") => rgit_git::EmptyCommit::Keep,
                _ if keep_redundant_commits => rgit_git::EmptyCommit::Keep,
                _ => rgit_git::EmptyCommit::Stop,
            };
            let opts = rgit_git::PickOptions {
                revert: false,
                no_commit,
                record_origin,
                mainline,
                strategy_option,
                edit,
                signoff,
                allow_empty: allow_empty || keep_redundant_commits,
                empty,
                ff,
                reference: false,
            };
            pick(backend, revs, &opts, (cont, skip, abort, quit), interactive)?
        }
        Command::Revert {
            revs,
            no_commit,
            mainline,
            strategy_option,
            edit,
            no_edit: _,
            signoff,
            reference,
            cont,
            skip,
            abort,
            quit,
        } => {
            let opts = rgit_git::PickOptions {
                revert: true,
                no_commit,
                mainline,
                strategy_option,
                edit,
                signoff,
                reference,
                ..Default::default()
            };
            pick(backend, revs, &opts, (cont, skip, abort, quit), interactive)?
        }
        Command::Branch { cmd, opts } => match cmd {
            None if opts.is_listing() => {
                render_branch_rows(backend, &branch_rows(backend, &opts)?, &opts)?
            }
            None => branch_change(backend, opts, interactive)?,
            Some(BranchCmd::Create { name, start: None }) => ok(backend.create_branch(&name))?,
            Some(BranchCmd::Create {
                name,
                start: Some(start),
            }) => {
                backend.create_branch_at(&name, &start, false)?;
                ok(backend.checkout_branch(&name))?
            }
            Some(BranchCmd::Checkout { name }) => ok(backend.checkout_branch(&name))?,
            Some(BranchCmd::Delete { names, force }) if !names.is_empty() => {
                delete_branches(backend, &names, force)?
            }
            Some(BranchCmd::Delete { force, .. }) if interactive => {
                let names =
                    crate::interactive::multiselect_branches(backend, "Delete which branches?")?;
                if names.is_empty() {
                    "none selected".to_owned()
                } else {
                    for n in &names {
                        backend.delete_branch(n, force)?;
                    }
                    format!("deleted {}", names.join(", "))
                }
            }
            Some(BranchCmd::Delete { .. }) => {
                return Err(CliError::usage("a branch name required"));
            }
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
        Command::Stash { cmd, push, paths } => match cmd {
            None | Some(StashCmd::Push { .. }) => {
                let (p, mut paths) = match cmd {
                    Some(StashCmd::Push { push, paths }) => (push, paths),
                    _ => (push, paths),
                };
                if let Some(file) = &p.pathspec_from_file {
                    paths.extend(pathspec_from_file(file, p.pathspec_file_nul)?);
                }
                if p.staged {
                    ok_msg(backend.stash_push_part(p.message.as_deref(), None, true, &paths))?
                } else if p.patch {
                    if !interactive {
                        return Err(CliError::usage(
                            "stash -p picks hunks on a terminal; stash whole files with `rgit stash push -- <path>`",
                        ));
                    }
                    let files = backend.diff(&rgit_git::DiffSpec {
                        from: Some("HEAD".to_owned()),
                        paths: paths.clone(),
                        ..rgit_git::DiffSpec::default()
                    })?;
                    let hunks = crate::interactive::pick_hunks(&files, "Stash")?;
                    if hunks.is_empty() {
                        return Ok("No changes selected".to_owned());
                    }
                    ok_msg(backend.stash_push_part(
                        p.message.as_deref(),
                        Some(&hunks),
                        !p.no_keep_index,
                        &paths,
                    ))?
                } else {
                    ok_msg(backend.stash_push_opts(
                        p.message.as_deref(),
                        p.include_untracked,
                        p.all,
                        p.keep_index,
                        &paths,
                    ))?
                }
            }
            Some(StashCmd::Pop {
                index,
                restore_index,
            }) => ok(backend.stash_apply_opts(
                stash_index(backend, index, interactive, "Pop which stash?")?,
                restore_index,
                true,
            ))?,
            Some(StashCmd::Apply {
                index,
                restore_index,
            }) => ok(backend.stash_apply_opts(
                stash_index(backend, index, interactive, "Apply which stash?")?,
                restore_index,
                false,
            ))?,
            Some(StashCmd::Drop { index }) => ok(backend.stash_drop(stash_index(
                backend,
                index,
                interactive,
                "Drop which stash?",
            )?))?,
            Some(StashCmd::List {
                format: Some(format),
                max_count,
            }) => stash_log(backend, &format, max_count)?,
            Some(StashCmd::List { max_count, .. }) => {
                let mut stashes = backend.status()?.stashes;
                stashes.truncate(max_count.unwrap_or(usize::MAX));
                render::stashes(&stashes)
            }
            Some(StashCmd::Show {
                index,
                patch,
                name_only,
                name_status,
                numstat,
                stat,
                include_untracked,
                only_untracked,
            }) => diff_out(
                &stash_diff(
                    backend,
                    index.unwrap_or(0),
                    (!only_untracked, include_untracked || only_untracked),
                )?,
                DiffFormat {
                    patch,
                    name_only,
                    name_status,
                    numstat,
                    stat,
                },
            ),
            Some(StashCmd::Create { message }) => {
                let message = message.join(" ");
                backend
                    .stash_create((!message.is_empty()).then_some(message.as_str()))?
                    .unwrap_or_default()
            }
            Some(StashCmd::Store {
                commit, message, ..
            }) => ok(backend.stash_store(&commit, message.as_deref()))?,
            Some(StashCmd::Branch { name, index }) => {
                let i = index.unwrap_or(0);
                backend.create_branch_at(&name, &format!("stash@{{{i}}}^1"), false)?;
                backend.checkout_branch(&name)?;
                backend.stash_apply_opts(i, true, true)?;
                "ok".to_owned()
            }
            Some(StashCmd::Clear) => {
                for _ in 0..backend.status()?.stashes.len() {
                    backend.stash_drop(0)?;
                }
                "ok".to_owned()
            }
        },
        Command::Tag { names, opts } => {
            if opts.is_listing(&names) {
                let tags = tag_list(backend, &names, &opts)?;
                if let Some(fmt) = &opts.format {
                    crate::plumbing::format_refs(backend, &tags, fmt)?
                } else if tags.is_empty() {
                    "no tags".to_owned()
                } else {
                    tags.iter()
                        .map(|t| tag_with_message(t, opts.lines))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            } else if opts.delete {
                if names.is_empty() {
                    return Err(CliError::usage("tag -d needs a tag name"));
                }
                let failed: Vec<String> = names
                    .iter()
                    .filter_map(|n| backend.delete_tag(n).err().map(|e| format!("{n}: {e}")))
                    .collect();
                if !failed.is_empty() {
                    anyhow::bail!("could not delete {}", failed.join("; "));
                }
                "ok".to_owned()
            } else if opts.verify {
                if names.is_empty() {
                    return Err(CliError::usage("tag -v needs a tag name"));
                }
                backend.verify_tags(&names)?
            } else {
                create_tag(backend, &names, opts, interactive)?
            }
        }
        Command::Remote { cmd, verbose } => match cmd {
            None if verbose => {
                let mut lines = Vec::new();
                for r in backend.remotes()? {
                    let url = backend.remote_urls(&r.name, false)?.into_iter().next();
                    lines.push(format!("{}\t{} (fetch)", r.name, url.unwrap_or(r.url)));
                    for url in backend.remote_urls(&r.name, true)? {
                        lines.push(format!("{}\t{url} (push)", r.name));
                    }
                }
                if lines.is_empty() {
                    "no remotes".to_owned()
                } else {
                    lines.join("\n")
                }
            }
            None => render::remotes(&backend.remotes()?),
            Some(RemoteCmd::Add {
                name,
                url,
                fetch,
                track,
                master,
                mirror,
                tags,
                no_tags,
            }) => {
                use rgit_git::ConfigScope::Local;
                let specs: Vec<String> = match mirror.as_deref() {
                    Some("fetch" | "both") => vec!["+refs/*:refs/*".to_owned()],
                    Some("push") => Vec::new(),
                    Some(other) => {
                        return Err(CliError::usage(format!(
                            "unknown --mirror value {other}; use fetch or push"
                        )));
                    }
                    None => track
                        .iter()
                        .map(|b| format!("+refs/heads/{b}:refs/remotes/{name}/{b}"))
                        .collect(),
                };
                backend.add_remote(&name, &url)?;
                if !specs.is_empty() {
                    set_fetch_specs(backend, &name, &specs, false)?;
                }
                if matches!(mirror.as_deref(), Some("push" | "both")) {
                    backend.config_write(Local, &format!("remote.{name}.mirror"), "true", false)?;
                }
                if tags || no_tags {
                    let opt = if tags { "--tags" } else { "--no-tags" };
                    backend.config_write(Local, &format!("remote.{name}.tagOpt"), opt, false)?;
                }
                if fetch {
                    backend.fetch(Some(&name), &[], &rgit_git::FetchArgs::default(), &|_| {})?;
                }
                if let Some(m) = master {
                    backend.set_symbolic_ref(
                        &format!("refs/remotes/{name}/HEAD"),
                        &format!("refs/remotes/{name}/{m}"),
                        Some("remote add"),
                    )?;
                }
                "ok".to_owned()
            }
            Some(RemoteCmd::Remove { name }) => ok(backend.remove_remote(&name))?,
            Some(RemoteCmd::SetUrl {
                name,
                url,
                old,
                push,
                add,
                delete,
            }) => ok(backend.edit_remote_urls(&name, &url, old.as_deref(), push, add, delete))?,
            Some(RemoteCmd::GetUrl { name, push, all }) => {
                let urls = backend.remote_urls(&name, push)?;
                if all {
                    urls.join("\n")
                } else {
                    urls.into_iter().next().unwrap_or_default()
                }
            }
            Some(RemoteCmd::Rename { old, new }) => ok(backend.rename_remote(&old, &new))?,
            Some(RemoteCmd::Prune { names, dry_run }) => {
                let mut pruned = Vec::new();
                for name in &names {
                    if dry_run {
                        let (heads, _) = backend.remote_heads(name)?;
                        let stale = remote_branches(backend, name, &heads)?.stale;
                        pruned.extend(stale.into_iter().map(|r| {
                            r.strip_prefix("refs/remotes/")
                                .map_or(r.clone(), str::to_owned)
                        }));
                    } else {
                        pruned.extend(backend.prune_remote(name)?);
                    }
                }
                match (pruned.is_empty(), dry_run) {
                    (true, _) => "nothing to prune".to_owned(),
                    (false, true) => format!("would prune {}", pruned.join(", ")),
                    (false, false) => format!("pruned {}", pruned.join(", ")),
                }
            }
            Some(RemoteCmd::Show { names, no_query }) => {
                let names = if names.is_empty() {
                    backend.remotes()?.into_iter().map(|r| r.name).collect()
                } else {
                    names
                };
                let mut out = Vec::new();
                for name in &names {
                    out.push(remote_show(backend, name, !no_query)?);
                }
                out.join("\n")
            }
            Some(RemoteCmd::Update { names, prune }) => {
                let group = |g: &str| -> anyhow::Result<Option<Vec<String>>> {
                    Ok(backend
                        .config_get(&format!("remotes.{g}"))?
                        .map(|v| v.split_whitespace().map(str::to_owned).collect()))
                };
                let remotes: Vec<String> = backend.remotes()?.into_iter().map(|r| r.name).collect();
                let mut targets = Vec::new();
                if names.is_empty() {
                    match group("default")? {
                        Some(g) => targets = g,
                        None => {
                            for r in &remotes {
                                let key = format!("remote.{r}.skipDefaultUpdate");
                                if backend.config_get(&key)?.as_deref() != Some("true") {
                                    targets.push(r.clone());
                                }
                            }
                        }
                    }
                }
                for n in names {
                    match group(&n)? {
                        Some(g) => targets.extend(g),
                        None if remotes.contains(&n) => targets.push(n),
                        None => anyhow::bail!("no such remote or remote group: {n}"),
                    }
                }
                let args = rgit_git::FetchArgs {
                    prune,
                    ..rgit_git::FetchArgs::default()
                };
                let mut out = Vec::new();
                for t in targets {
                    backend.fetch(Some(&t), &[], &args, &|_| {})?;
                    out.push(format!("Fetching {t}"));
                }
                out.join("\n")
            }
            Some(RemoteCmd::SetHead {
                name,
                branch,
                auto: _,
                delete,
            }) => {
                let head = format!("refs/remotes/{name}/HEAD");
                if delete {
                    backend.update_ref(&head, None, None, true, None)?;
                    return Ok(format!("deleted {name}/HEAD"));
                }
                let branch = match branch {
                    Some(b) => b,
                    None => backend
                        .remote_heads(&name)?
                        .1
                        .ok_or_else(|| anyhow::anyhow!("Cannot determine remote HEAD"))?,
                };
                let target = format!("refs/remotes/{name}/{branch}");
                if backend.rev_parse(&target).is_err() {
                    anyhow::bail!("Not a valid ref: {target}");
                }
                backend.set_symbolic_ref(&head, &target, Some("remote set-head"))?;
                format!("{name}/HEAD set to {branch}")
            }
            Some(RemoteCmd::SetBranches {
                name,
                branches,
                add,
            }) => {
                backend.remote_urls(&name, false)?;
                let specs: Vec<String> = branches
                    .iter()
                    .map(|b| format!("+refs/heads/{b}:refs/remotes/{name}/{b}"))
                    .collect();
                set_fetch_specs(backend, &name, &specs, add)?;
                "ok".to_owned()
            }
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
            None => worktree_list(backend, false, false, false)?,
            Some(WorktreeCmd::List {
                porcelain,
                z,
                verbose,
            }) => worktree_list(backend, porcelain, z, verbose)?,
            Some(WorktreeCmd::Add {
                path,
                commitish,
                new_branch,
                reset_branch,
                detach,
                orphan,
                force,
                no_checkout,
                lock,
                reason,
                track,
                no_track,
                quiet: _,
            }) => {
                let args = rgit_git::WorktreeAddArgs {
                    reset: reset_branch.is_some(),
                    new_branch: new_branch.or(reset_branch),
                    detach,
                    orphan,
                    force,
                    no_checkout,
                    lock,
                    reason,
                    track: (track || no_track).then_some(track),
                };
                ok(backend.worktree_add(&path, commitish.as_deref(), &args))?
            }
            Some(WorktreeCmd::Repair { paths }) => backend.repair_worktrees(&paths)?.join("\n"),
            Some(WorktreeCmd::Remove { name, force }) => ok(backend.remove_worktree(&name, force))?,
            Some(WorktreeCmd::Lock { name, reason }) => {
                ok(backend.worktree_lock(&name, reason.as_deref()))?
            }
            Some(WorktreeCmd::Unlock { name }) => ok(backend.worktree_unlock(&name))?,
            Some(WorktreeCmd::Move { name, new_path }) => {
                ok(backend.worktree_move(&name, &new_path))?
            }
            Some(WorktreeCmd::Prune { dry_run, .. }) => {
                let pruned: Vec<String> = if dry_run {
                    let list = backend.worktrees()?.into_iter();
                    list.filter(|w| w.prunable.is_some() && !w.locked)
                        .map(|w| w.name)
                        .collect()
                } else {
                    backend.prune_worktrees()?
                };
                match (pruned.is_empty(), dry_run) {
                    (true, _) => "nothing to prune".to_owned(),
                    (false, true) => format!("would prune {}", pruned.join(", ")),
                    (false, false) => format!("pruned {}", pruned.join(", ")),
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
            quiet,
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
                if quiet {
                    String::new()
                } else {
                    "ok".to_owned()
                }
            }
        }
        Command::Rm {
            paths,
            cached,
            recursive,
            force,
            dry_run,
            quiet,
            ignore_unmatch,
        } => {
            let paths = if paths.is_empty() {
                vec![resolve(None, "a path", &|| {
                    crate::interactive::pick_file(backend, "Remove which file?")
                })?]
            } else {
                paths
            };
            let opts = rgit_git::RmOptions {
                cached,
                recursive,
                force,
                dry_run,
                ignore_unmatch,
            };
            let removed = backend.remove_paths(&paths, opts)?;
            if quiet {
                String::new()
            } else {
                removed
                    .iter()
                    .map(|p| format!("rm '{p}'"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        Command::Mv {
            paths,
            force,
            skip_errors,
            dry_run,
            verbose,
        } => {
            let (to, from) = paths.split_last().expect("clap requires two paths");
            if from.len() > 1 && !backend.workdir().join(to).is_dir() {
                anyhow::bail!("destination '{to}' is not a directory");
            }
            let mut out = Vec::new();
            for f in from {
                match backend.move_path(f, to, force, dry_run) {
                    Ok(dest) => {
                        if dry_run {
                            out.push(format!("Checking rename of '{f}' to '{dest}'"));
                        }
                        if dry_run || verbose {
                            out.push(format!("Renaming {f} to {dest}"));
                        }
                    }
                    Err(_) if skip_errors => {}
                    Err(e) => return Err(e.into()),
                }
            }
            if dry_run || verbose {
                out.join("\n")
            } else {
                "ok".to_owned()
            }
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
        Command::Submodule { cmd } => submodule(backend, cmd, interactive)?,
        command @ Command::LsRemote { .. } => {
            crate::plumbing::ls_remote(Some(backend.workdir()), command, true)?.text
        }
        Command::Git { args } => backend.git(&args)?,
        Command::Init { .. }
        | Command::Clone { .. }
        | Command::Mcp
        | Command::Serve { .. }
        | Command::Forge { .. } => unreachable!("handled before dispatch"),
    })
}

/// `git submodule status` lines for `list`: the recorded commit with
/// `cached`, else the checked-out one.
pub fn submodule_status(list: &[rgit_git::SubmoduleInfo], cached: bool) -> String {
    list.iter()
        .map(|s| {
            let commit = match cached {
                true => s.recorded.as_ref(),
                false => s.checked_out.as_ref().or(s.recorded.as_ref()),
            };
            let name = match (&s.describe, s.state) {
                (Some(d), s) if s != '-' => format!(" ({d})"),
                _ => String::new(),
            };
            format!(
                "{}{} {}{name}",
                s.state,
                commit.map_or("0".repeat(40), String::clone),
                s.path
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The submodules under `paths` (all when empty).
pub fn chosen_submodules(
    backend: &Arc<dyn GitBackend>,
    recursive: bool,
    paths: &[String],
) -> anyhow::Result<Vec<rgit_git::SubmoduleInfo>> {
    let paths: Vec<String> = paths
        .iter()
        .map(|p| p.trim_end_matches('/').to_owned())
        .collect();
    Ok(backend
        .submodules(recursive)?
        .into_iter()
        .filter(|s| paths.is_empty() || rgit_git::pathspec_matches(&paths, &s.path))
        .collect())
}

fn submodule(
    backend: &Arc<dyn GitBackend>,
    cmd: Option<SubmoduleCmd>,
    interactive: bool,
) -> anyhow::Result<String> {
    use rgit_git::SubmoduleOp as Op;
    let (op, quiet) = match cmd {
        None => return Ok(submodule_status(&backend.submodules(false)?, false)),
        Some(SubmoduleCmd::Status {
            cached,
            recursive,
            quiet,
            paths,
        }) => {
            let text = submodule_status(&chosen_submodules(backend, recursive, &paths)?, cached);
            return Ok(if quiet { String::new() } else { text });
        }
        Some(SubmoduleCmd::Foreach {
            recursive,
            quiet,
            command,
        }) => return submodule_foreach(backend, recursive, quiet, &command),
        Some(SubmoduleCmd::Add {
            url,
            path,
            branch,
            name,
            quiet,
        }) => (
            Op::Add {
                url,
                path,
                branch,
                name,
            },
            quiet,
        ),
        Some(SubmoduleCmd::Init { quiet, paths }) => (Op::Init { paths }, quiet),
        Some(SubmoduleCmd::Update {
            init,
            recursive,
            remote,
            quiet,
            paths,
            ..
        }) => (
            Op::Update {
                paths,
                init,
                recursive,
                remote,
            },
            quiet,
        ),
        Some(SubmoduleCmd::Sync {
            recursive,
            quiet,
            paths,
        }) => (Op::Sync { paths, recursive }, quiet),
        Some(SubmoduleCmd::Deinit {
            force,
            all,
            quiet,
            paths,
        }) => (Op::Deinit { paths, force, all }, quiet),
        Some(SubmoduleCmd::Summary { args }) => (Op::Summary { args }, false),
        Some(SubmoduleCmd::SetUrl { path, url }) => (Op::SetUrl { path, url }, false),
        Some(SubmoduleCmd::SetBranch { branch, path, .. }) => {
            (Op::SetBranch { path, branch }, false)
        }
        Some(SubmoduleCmd::Absorbgitdirs { paths }) => (Op::AbsorbGitDirs { paths }, false),
    };
    let out = net(interactive, "submodule", |r| backend.submodule(&op, r))?;
    Ok(if quiet { String::new() } else { out })
}

/// `submodule foreach`: run `command` in each checked-out submodule with
/// git's variables set, printing `Entering '<path>'` before its output.
fn submodule_foreach(
    backend: &Arc<dyn GitBackend>,
    recursive: bool,
    quiet: bool,
    command: &[String],
) -> anyhow::Result<String> {
    let top = backend.workdir().to_path_buf();
    let mut out = String::new();
    for s in backend.submodules(recursive)? {
        if s.state == '-' {
            continue;
        }
        if !quiet {
            out.push_str(&format!("Entering '{}'\n", s.path));
        }
        // ponytail: nested submodules get the top-level $toplevel and a
        // top-relative $sm_path; git gives their immediate superproject's.
        let mut cmd = match command {
            [one] => {
                let mut sh = std::process::Command::new("sh");
                sh.args(["-c", one]);
                sh
            }
            [program, args @ ..] => {
                let mut c = std::process::Command::new(program);
                c.args(args);
                c
            }
            [] => anyhow::bail!("foreach needs a command"),
        };
        let result = cmd
            .current_dir(top.join(&s.path))
            .env("name", &s.name)
            .env("sm_path", &s.path)
            .env("displaypath", &s.path)
            .env("sha1", s.checked_out.as_deref().unwrap_or_default())
            .env("toplevel", &top)
            .output()?;
        out.push_str(&String::from_utf8_lossy(&result.stdout));
        if !result.status.success() {
            return Err(CliError {
                message: format!(
                    "{}run_command returned non-zero status for {}",
                    String::from_utf8_lossy(&result.stderr),
                    s.path
                ),
                help: None,
                code: result.status.code().unwrap_or(1),
            }
            .into());
        }
    }
    Ok(out.trim_end().to_owned())
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

/// Run a bisect subcommand, checked here and stepped by git's bisect. Returns
/// the text and, for `visualize`, the commits still in the search.
pub(crate) fn bisect(
    backend: &Arc<dyn GitBackend>,
    cmd: BisectCmd,
) -> anyhow::Result<(String, Option<Vec<rgit_git::LogEntry>>)> {
    let dir = backend.git_dir();
    let terms = std::fs::read_to_string(dir.join("BISECT_TERMS")).unwrap_or_default();
    let mut terms = terms.lines();
    let bad = terms.next().unwrap_or("bad").to_owned();
    let good = terms.next().unwrap_or("good").to_owned();
    let mut args = cmd.git_args();
    match cmd {
        BisectCmd::Mark(words) => {
            let term = words.first().map(String::as_str).unwrap_or("");
            if term != bad && term != good {
                return Err(CliError::usage(format!(
                    "unknown bisect subcommand '{term}'; the terms are {bad} and {good}"
                )));
            }
        }
        BisectCmd::Replay { file } => {
            let path = std::fs::canonicalize(&file)
                .map_err(|e| anyhow::anyhow!("could not read {file}: {e}"))?;
            args[1] = path.display().to_string();
        }
        BisectCmd::Visualize => {
            let refs = backend.ref_details()?;
            let Some(tip) = refs.iter().find(|r| r.name == format!("refs/bisect/{bad}")) else {
                anyhow::bail!("no {bad} commit marked yet; run `rgit bisect {bad} <rev>`");
            };
            let mut revs = vec![tip.id.clone()];
            let old = format!("refs/bisect/{good}-");
            revs.extend(
                refs.iter()
                    .filter(|r| r.name.starts_with(&old))
                    .map(|r| format!("^{}", r.id)),
            );
            let names = std::fs::read_to_string(dir.join("BISECT_NAMES")).unwrap_or_default();
            let entries = backend.log(&LogOptions {
                limit: usize::MAX,
                revs,
                paths: names
                    .split('\'')
                    .skip(1)
                    .step_by(2)
                    .map(str::to_owned)
                    .collect(),
                ..Default::default()
            })?;
            let text = entries
                .iter()
                .map(|e| format!("{} {}", e.short_id, e.summary))
                .collect::<Vec<_>>()
                .join("\n");
            return Ok((text, Some(entries)));
        }
        _ => {}
    }
    let out = backend.bisect(&args)?;
    Ok((if out.is_empty() { "ok".to_owned() } else { out }, None))
}

/// Cherry-pick or revert `revs`, or continue/skip/abort/quit a stopped sequence.
fn pick(
    backend: &Arc<dyn GitBackend>,
    mut revs: Vec<String>,
    opts: &rgit_git::PickOptions,
    (cont, skip, abort, quit): (bool, bool, bool, bool),
    interactive: bool,
) -> anyhow::Result<String> {
    if abort {
        let warning = backend.pick_abort()?;
        return Ok(if warning.is_empty() {
            "ok".to_owned()
        } else {
            warning
        });
    }
    if quit {
        return ok(backend.pick_quit());
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

/// Split git's `<rev> <paths>...` and `[<rev>] -- <paths>...`: without `--`,
/// the first word is a revision only when `is_rev` says so, else a path.
fn rev_and_paths(
    first: Option<String>,
    mut rest: Vec<String>,
    after_dashes: Vec<String>,
    is_rev: impl Fn(&str) -> bool,
) -> (Option<String>, Vec<String>) {
    let dashes = !after_dashes.is_empty();
    rest.extend(after_dashes);
    match first {
        Some(f) if !dashes && !is_rev(&f) => {
            rest.insert(0, f);
            (None, rest)
        }
        first => (first, rest),
    }
}

/// The one remote-tracking branch named `<remote>/<name>`, which git's
/// checkout and switch turn into a local tracking branch.
fn guess_remote(backend: &Arc<dyn GitBackend>, name: &str) -> Option<String> {
    let hits: Vec<String> = backend
        .remote_branches()
        .ok()?
        .into_iter()
        .filter(|r| r.split_once('/').is_some_and(|(_, b)| b == name))
        .collect();
    match <[String; 1]>::try_from(hits) {
        Ok([one]) => Some(one),
        Err(_) => None,
    }
}

/// Branch-switch flags shared by `checkout` and `switch`.
struct SwitchOpts {
    detach: bool,
    track: bool,
    no_track: bool,
    guess: bool,
    mode: rgit_git::CheckoutMode,
    detach_ok: bool,
}

/// The checkout mode for -f/-m and `--conflict`, which implies -m.
fn checkout_mode(force: bool, merge: bool, conflict: Option<&str>) -> rgit_git::CheckoutMode {
    if force {
        rgit_git::CheckoutMode::Force
    } else if merge || conflict.is_some() {
        rgit_git::CheckoutMode::Merge {
            diff3: conflict == Some("diff3"),
        }
    } else {
        rgit_git::CheckoutMode::Safe
    }
}

/// Switch branches for `checkout` and `switch`: create `new` at `rev`
/// (resetting it when forced), detach at `rev`, or switch to the branch `rev`,
/// guessing a remote one. `-` is the previous branch. Only checkout
/// (`detach_ok`) detaches at a non-branch revision unasked.
fn switch(
    backend: &Arc<dyn GitBackend>,
    rev: Option<String>,
    new: Option<(String, bool)>,
    o: &SwitchOpts,
) -> anyhow::Result<String> {
    let prev = rev.as_deref() == Some("-");
    let rev = if prev {
        Some(backend.previous_checkout()?)
    } else {
        rev
    };
    let safe = o.mode == rgit_git::CheckoutMode::Safe;
    let create = |name: &str, start: &str, force: bool, track: bool| -> anyhow::Result<()> {
        if safe {
            backend.branch_from(name, start, force, track)?;
        } else {
            backend.create_branch_at(name, start, force)?;
            if track {
                backend.set_upstream(name, Some(start))?;
            }
            backend.checkout_with(name, true, o.mode)?;
        }
        if o.no_track && backend.branch_upstream(name)?.is_some() {
            backend.set_upstream(name, None)?;
        }
        Ok(())
    };
    if let Some((name, force)) = new {
        create(&name, rev.as_deref().unwrap_or("HEAD"), force, o.track)?;
        return Ok("ok".to_owned());
    }
    let rev = rev.unwrap_or_else(|| "HEAD".to_owned());
    let tracked = |name: &str, start: &str| -> anyhow::Result<String> {
        create(name, start, false, !o.no_track)?;
        Ok(if o.no_track {
            format!("created branch {name} from {start}")
        } else {
            format!("created branch {name} tracking {start}")
        })
    };
    let guess = || o.guess.then(|| guess_remote(backend, &rev)).flatten();
    if o.detach {
        backend.checkout_with(&rev, false, o.mode)?;
    } else if backend.local_branches()?.contains(&rev) {
        backend.checkout_with(&rev, true, o.mode)?;
    } else if let Some((_, name)) = rev.split_once('/').filter(|_| o.track) {
        return tracked(name, &rev);
    } else if o.detach_ok && backend.rev_parse(&rev).is_ok() {
        backend.checkout_with(&rev, false, o.mode)?;
    } else if let Some(remote) = guess() {
        return tracked(&rev, &remote);
    } else if o.detach_ok {
        backend.checkout_with(&rev, false, o.mode)?;
    } else if backend.rev_parse(&rev).is_ok() {
        return Err(anyhow::Error::new(CliError {
            message: format!("a branch is expected, got '{rev}'"),
            help: Some(format!(
                "Run `rgit switch --detach {rev}` to check it out as a detached HEAD"
            )),
            code: 1,
        }));
    } else {
        anyhow::bail!("invalid reference: {rev}");
    }
    Ok(if prev {
        format!("checked out {rev}")
    } else {
        "ok".to_owned()
    })
}

/// One branch in a `branch` listing. `id`, `summary` and the upstream fields
/// are filled only when asked for with -v / -vv.
pub(crate) struct BranchRow {
    pub name: String,
    pub current: bool,
    pub id: String,
    pub summary: String,
    pub upstream: Option<(String, usize, usize)>,
    /// A remote-tracking branch.
    pub remote: bool,
    /// Where a symbolic ref (`origin/HEAD`) points, shortened.
    pub symref: Option<String>,
    pub detail: rgit_git::RefDetail,
}

impl BranchRow {
    /// The name as `git branch` prints it: `remotes/` in front of a remote
    /// branch with -a, and `-> <target>` after a symbolic one.
    fn label(&self, all: bool) -> String {
        let mut s = if self.remote && all {
            format!("remotes/{}", self.name)
        } else {
            self.name.clone()
        };
        if let Some(t) = &self.symref {
            s.push_str(&format!(" -> {t}"));
        }
        s
    }
}

/// The branches `rgit branch` lists under `opts`' filters, in `--sort` order.
pub(crate) fn branch_rows(
    backend: &Arc<dyn GitBackend>,
    opts: &BranchOpts,
) -> anyhow::Result<Vec<BranchRow>> {
    let head = backend.symbolic_ref("HEAD").ok().flatten();
    let mut sort = opts.sort.clone();
    if sort.is_empty() {
        sort.extend(backend.config_get("branch.sort").ok().flatten());
    }
    if sort.is_empty() && opts.ignore_case {
        sort.push("refname".to_owned());
    }
    let refs =
        crate::plumbing::sort_refs(backend, backend.ref_details()?, &sort, opts.ignore_case)?;
    let fold = |s: &str| {
        if opts.ignore_case {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    };
    let patterns: Vec<String> = opts.args.iter().map(|p| fold(p)).collect();
    let at = opts
        .points_at
        .as_ref()
        .map(|r| backend.rev_parse(r))
        .transpose()?;
    let mut rows = Vec::new();
    for detail in refs {
        let (name, remote) = if let Some(n) = detail.name.strip_prefix("refs/heads/") {
            (n.to_owned(), false)
        } else if let Some(n) = detail.name.strip_prefix("refs/remotes/") {
            (n.to_owned(), true)
        } else {
            continue;
        };
        let full = detail.name.as_str();
        let keep = if remote {
            opts.all || opts.remotes
        } else {
            !opts.remotes
        } && (patterns.is_empty()
            || rgit_git::pathspec_matches(&patterns, &fold(&name)))
            && at.as_ref().is_none_or(|at| *at == detail.id)
            && opts
                .merged
                .as_ref()
                .map_or(Ok(true), |r| backend.is_ancestor(full, r))?
            && opts
                .no_merged
                .as_ref()
                .map_or(Ok(true), |r| backend.is_ancestor(full, r).map(|m| !m))?
            && opts
                .contains
                .as_ref()
                .map_or(Ok(true), |r| backend.is_ancestor(r, full))?
            && opts
                .no_contains
                .as_ref()
                .map_or(Ok(true), |r| backend.is_ancestor(r, full).map(|c| !c))?;
        if !keep {
            continue;
        }
        let mut row = BranchRow {
            current: head.as_deref() == Some(full),
            name,
            id: String::new(),
            summary: String::new(),
            upstream: None,
            remote,
            symref: detail
                .symref
                .as_deref()
                .map(|s| s.strip_prefix("refs/remotes/").unwrap_or(s).to_owned()),
            detail,
        };
        if opts.verbose > 0 && row.symref.is_none() {
            if let Some(tip) = backend
                .log(&LogOptions {
                    limit: 1,
                    revs: vec![row.detail.name.clone()],
                    ..LogOptions::default()
                })?
                .into_iter()
                .next()
            {
                row.id = tip.short_id;
                row.summary = tip.summary;
            }
            if !remote {
                row.upstream = backend.branch_upstream(&row.name)?;
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Branches as `git branch` prints them: plainly, with -v / -vv, or in a
/// for-each-ref `--format`.
fn render_branch_rows(
    backend: &Arc<dyn GitBackend>,
    rows: &[BranchRow],
    opts: &BranchOpts,
) -> anyhow::Result<String> {
    let all = opts.all && !opts.remotes;
    if let Some(fmt) = &opts.format {
        return crate::plumbing::format_refs(backend, rows.iter().map(|r| &r.detail), fmt);
    }
    if opts.verbose == 0 {
        let labels: Vec<String> = rows.iter().map(|r| r.label(all)).collect();
        let current = rows.iter().find(|r| r.current).map(|r| r.name.as_str());
        return Ok(render::branches(&labels, current));
    }
    if rows.is_empty() {
        return Ok("no branches".to_owned());
    }
    let width = rows
        .iter()
        .filter(|r| r.symref.is_none())
        .map(|r| r.label(all).len())
        .max()
        .unwrap_or(0);
    Ok(rows
        .iter()
        .map(|r| {
            let mark = if r.current { "*" } else { " " };
            if r.symref.is_some() {
                return format!("{mark} {}", r.label(all));
            }
            let track = match &r.upstream {
                Some((up, ahead, behind)) => {
                    let mut counts = Vec::new();
                    if *ahead > 0 {
                        counts.push(format!("ahead {ahead}"));
                    }
                    if *behind > 0 {
                        counts.push(format!("behind {behind}"));
                    }
                    let counts = counts.join(", ");
                    match (opts.verbose > 1, counts.is_empty()) {
                        (true, true) => format!("[{up}] "),
                        (true, false) => format!("[{up}: {counts}] "),
                        (false, false) => format!("[{counts}] "),
                        (false, true) => String::new(),
                    }
                }
                None => String::new(),
            };
            format!(
                "{mark} {:<width$} {} {track}{}",
                r.label(all),
                r.id,
                r.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Delete each of `names`, reporting every failure rather than stopping at
/// the first.
fn delete_branches(
    backend: &Arc<dyn GitBackend>,
    names: &[String],
    force: bool,
) -> anyhow::Result<String> {
    let failed: Vec<String> = names
        .iter()
        .filter_map(|n| {
            backend
                .delete_branch(n, force)
                .err()
                .map(|e| format!("{n}: {e}"))
        })
        .collect();
    if !failed.is_empty() {
        anyhow::bail!("could not delete {}", failed.join("; "));
    }
    Ok(format!("deleted branch {}", names.join(", ")))
}

/// `rgit branch` with git's flags that change branches: create, delete,
/// rename, copy, set or unset the upstream, or print the current branch.
fn branch_change(
    backend: &Arc<dyn GitBackend>,
    o: BranchOpts,
    interactive: bool,
) -> anyhow::Result<String> {
    let current = || {
        backend
            .status()?
            .head
            .branch
            .ok_or_else(|| anyhow::anyhow!("HEAD is detached; name the branch"))
    };
    let target = |what: &str| match o.args.as_slice() {
        [] => current(),
        [name] => Ok(name.clone()),
        _ => Err(CliError::usage(format!("{what} takes at most one branch"))),
    };
    if o.show_current {
        return Ok(backend.status()?.head.branch.unwrap_or_default());
    }
    if o.edit_description {
        let name = target("--edit-description")?;
        if !interactive {
            return Err(CliError::usage(format!(
                "--edit-description opens an editor and needs a terminal; set it with `rgit config branch.{name}.description <text>`"
            )));
        }
        return edit_description(backend, &name);
    }
    if let Some(up) = &o.set_upstream_to {
        let name = target("--set-upstream-to")?;
        backend.set_upstream(&name, Some(up))?;
        return Ok(format!("branch {name} set up to track {up}"));
    }
    if o.unset_upstream {
        let name = target("--unset-upstream")?;
        backend.set_upstream(&name, None)?;
        return Ok(format!("branch {name} no longer tracks an upstream"));
    }
    if o.delete || o.force_delete {
        if o.args.is_empty() {
            return Err(CliError::usage("branch -d needs a branch name"));
        }
        if o.remotes {
            for name in &o.args {
                let full = format!("refs/remotes/{name}");
                backend
                    .update_ref(&full, None, None, true, None)
                    .map_err(|_| anyhow::anyhow!("remote-tracking branch '{name}' not found"))?;
            }
            return Ok(format!(
                "deleted remote-tracking branch {}",
                o.args.join(", ")
            ));
        }
        return delete_branches(backend, &o.args, o.force_delete || o.force);
    }
    if o.rename || o.force_rename || o.copy || o.force_copy {
        let (old, new) = match o.args.as_slice() {
            [new] => (current()?, new.clone()),
            [old, new] => (old.clone(), new.clone()),
            _ => return Err(CliError::usage("branch -m/-c takes [<old>] <new>")),
        };
        let force = o.force_rename || o.force_copy || o.force;
        if o.copy || o.force_copy {
            backend.copy_branch(&old, &new, force)?;
            return Ok(format!("copied branch {old} to {new}"));
        }
        if force && old != new && backend.local_branches()?.contains(&new) {
            backend.delete_branch(&new, true)?;
        }
        backend.rename_branch(&old, &new)?;
        return Ok(format!("renamed branch {old} to {new}"));
    }
    let (name, start) = match o.args.as_slice() {
        [name] => (name, "HEAD"),
        [name, start] => (name, start.as_str()),
        _ => {
            return Err(CliError::usage(
                "branch takes a new branch name and an optional start point",
            ));
        }
    };
    backend.create_branch_at(name, start, o.force)?;
    let upstream = match o.track.as_deref() {
        Some("inherit") => backend.branch_upstream(start)?.map(|(up, ..)| up),
        Some("direct") => Some(start.to_owned()),
        Some(other) => {
            return Err(CliError::usage(format!(
                "--track takes direct or inherit, got {other}"
            )));
        }
        None => None,
    };
    if o.no_track {
        backend.set_upstream(name, None)?;
    } else if let Some(up) = upstream {
        backend.set_upstream(name, Some(&up))?;
        return Ok(format!("branch {name} set up to track {up}"));
    }
    Ok(format!("created branch {name} at {start}"))
}

/// `git branch --edit-description`: edit `branch.<name>.description` in the
/// editor, dropping `#` lines; an empty text removes it.
fn edit_description(backend: &Arc<dyn GitBackend>, name: &str) -> anyhow::Result<String> {
    use rgit_git::ConfigScope;
    let key = format!("branch.{name}.description");
    let old = backend.config_get(&key)?.unwrap_or_default();
    let text = edit_file(
        backend,
        "EDIT_DESCRIPTION",
        &format!(
            "{old}# Please edit the description for the branch\n#   {name}\n# Lines starting with '#' will be stripped.\n"
        ),
    )?;
    let text = strip_comments(&text);
    let text = text.trim();
    if text.is_empty() {
        let _ = backend.config_unset(ConfigScope::Local, &key, false);
    } else {
        backend.config_write(ConfigScope::Local, &key, &format!("{text}\n"), false)?;
    }
    Ok(format!("edited the description of {name}"))
}

/// Worktrees as `git worktree list` prints them: aligned, with lock and
/// prune reasons under each with `verbose`, or in `porcelain` form
/// (NUL-ended with `z`).
fn worktree_list(
    backend: &Arc<dyn GitBackend>,
    porcelain: bool,
    z: bool,
    verbose: bool,
) -> anyhow::Result<String> {
    let list = backend.worktrees()?;
    let path = |w: &rgit_git::Worktree| w.path.trim_end_matches('/').to_owned();
    let mut out = String::new();
    if porcelain {
        let end = if z { '\0' } else { '\n' };
        for w in &list {
            let zero = "0".repeat(40);
            let mut lines = vec![
                format!("worktree {}", path(w)),
                format!("HEAD {}", w.oid.as_deref().unwrap_or(&zero)),
                match &w.branch {
                    Some(b) => format!("branch refs/heads/{b}"),
                    None => "detached".to_owned(),
                },
            ];
            if w.locked {
                lines.push(match &w.lock_reason {
                    Some(r) => format!("locked {r}"),
                    None => "locked".to_owned(),
                });
            }
            lines.extend(w.prunable.as_ref().map(|p| format!("prunable {p}")));
            for line in lines {
                out.push_str(&line);
                out.push(end);
            }
            out.push(end);
        }
        return Ok(out);
    }
    let width = list.iter().map(|w| path(w).len()).max().unwrap_or(0) + 1;
    let mut lines = Vec::new();
    for w in &list {
        let head = w.head.clone().unwrap_or_else(|| "0000000".to_owned());
        let on = match &w.branch {
            Some(b) => format!("[{b}]"),
            None => "(detached HEAD)".to_owned(),
        };
        let mut line = format!("{:<width$} {head} {on}", path(w));
        let reason = |what: &str, why: &Option<String>| match why {
            Some(r) => format!("\t{what}: {r}"),
            None => format!("\t{what}"),
        };
        if verbose {
            if w.locked {
                line.push('\n');
                line.push_str(&reason("locked", &w.lock_reason));
            }
            if w.prunable.is_some() {
                line.push('\n');
                line.push_str(&reason("prunable", &w.prunable));
            }
        } else {
            if w.locked {
                line.push_str(" locked");
            }
            if w.prunable.is_some() {
                line.push_str(" prunable");
            }
        }
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

/// Set remote `name`'s fetch refspecs to `specs`, or add them with `add`.
fn set_fetch_specs(
    backend: &Arc<dyn GitBackend>,
    name: &str,
    specs: &[String],
    add: bool,
) -> anyhow::Result<()> {
    use rgit_git::ConfigScope::Local;
    let key = format!("remote.{name}.fetch");
    if !add {
        let _ = backend.config_unset(Local, &key, true);
    }
    for spec in specs {
        backend.config_write(Local, &key, spec, true)?;
    }
    Ok(())
}

/// A remote's branches against our tracking refs, as full tracking ref
/// names: fetched, not fetched yet, and gone from the remote.
#[derive(Default)]
struct RemoteBranches {
    tracked: Vec<String>,
    new: Vec<String>,
    stale: Vec<String>,
}

/// Sort `heads` (a remote's refs) against remote `name`'s tracking refs
/// through its fetch refspecs.
fn remote_branches(
    backend: &Arc<dyn GitBackend>,
    name: &str,
    heads: &[(String, String)],
) -> anyhow::Result<RemoteBranches> {
    let specs: Vec<(String, String)> = backend
        .config_entries(
            rgit_git::ConfigScope::Any,
            Some(&format!("remote.{name}.fetch")),
        )?
        .into_iter()
        .filter_map(|(_, v)| {
            let (src, dst) = v.trim_start_matches('+').split_once(':')?;
            Some((src.to_owned(), dst.to_owned()))
        })
        .collect();
    // `from` through a refspec side with at most one `*`, onto the other.
    let through = |from: &str, pat: &str, onto: &str| match pat.split_once('*') {
        Some((pre, suf)) => from
            .strip_prefix(pre)
            .and_then(|r| r.strip_suffix(suf))
            .map(|mid| onto.replacen('*', mid, 1)),
        None => (from == pat).then(|| onto.to_owned()),
    };
    let tracking: Vec<String> = backend
        .ref_details()?
        .into_iter()
        .filter(|r| r.symref.is_none())
        .map(|r| r.name)
        .filter(|n| specs.iter().any(|(s, d)| through(n, d, s).is_some()))
        .collect();
    let mut out = RemoteBranches::default();
    let mut fetched = Vec::new();
    for (head, _) in heads.iter().filter(|(h, _)| h.starts_with("refs/heads/")) {
        let Some(dst) = specs.iter().find_map(|(s, d)| through(head, s, d)) else {
            continue;
        };
        if tracking.contains(&dst) {
            out.tracked.push(dst.clone());
        } else {
            out.new.push(dst.clone());
        }
        fetched.push(dst);
    }
    out.stale = tracking
        .into_iter()
        .filter(|t| !fetched.contains(t))
        .collect();
    Ok(out)
}

/// `git remote show <name>`, querying the remote unless `query` is false.
fn remote_show(backend: &Arc<dyn GitBackend>, name: &str, query: bool) -> anyhow::Result<String> {
    let plural = |n: usize, one: &str, many: &str| if n == 1 { one } else { many }.to_owned();
    let not_queried = if query { "" } else { " (status not queried)" };
    let fetch_urls = backend.remote_urls(name, false)?;
    let mut out = vec![
        format!("* remote {name}"),
        format!(
            "  Fetch URL: {}",
            fetch_urls.first().map_or("(no URL)", String::as_str)
        ),
    ];
    for url in backend.remote_urls(name, true)? {
        out.push(format!("  Push  URL: {url}"));
    }
    let (heads, head) = if query {
        backend.remote_heads(name)?
    } else {
        (Vec::new(), None)
    };
    out.push(format!(
        "  HEAD branch: {}",
        match (query, &head) {
            (false, _) => "(not queried)",
            (true, Some(h)) => h,
            (true, None) => "(unknown)",
        }
    ));

    let prefix = format!("refs/remotes/{name}/");
    let branches = remote_branches(backend, name, &heads)?;
    let mut rows: Vec<(String, String)> = Vec::new();
    if query {
        let short = |r: &String| r.strip_prefix(&prefix).unwrap_or(r).to_owned();
        rows.extend(
            branches
                .tracked
                .iter()
                .map(|r| (short(r), "tracked".to_owned())),
        );
        rows.extend(branches.new.iter().map(|r| {
            (
                short(r),
                format!("new (next fetch will store in remotes/{name})"),
            )
        }));
        rows.extend(branches.stale.iter().map(|r| {
            (
                r.clone(),
                "stale (use 'git remote prune' to remove)".to_owned(),
            )
        }));
    } else {
        rows.extend(branches.stale.iter().map(|r| {
            (
                r.strip_prefix(&prefix).unwrap_or(r).to_owned(),
                String::new(),
            )
        }));
    }
    rows.sort();
    if !rows.is_empty() {
        out.push(format!(
            "  {}:{not_queried}",
            plural(rows.len(), "Remote branch", "Remote branches")
        ));
        let w = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
        for (n, state) in &rows {
            out.push(if state.is_empty() {
                format!("    {n}")
            } else {
                format!("    {n:<w$} {state}")
            });
        }
    }

    let config = backend.config_entries(rgit_git::ConfigScope::Any, None)?;
    let get = |key: &str| {
        config
            .iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    };
    let mut pulls = Vec::new();
    for b in backend.local_branches()? {
        if get(&format!("branch.{b}.remote")) != Some(name) {
            continue;
        }
        let Some(merge) = get(&format!("branch.{b}.merge")) else {
            continue;
        };
        let merge = merge
            .strip_prefix("refs/heads/")
            .unwrap_or(merge)
            .to_owned();
        let how = match get(&format!("branch.{b}.rebase")) {
            Some("interactive" | "i") => "rebases interactively onto remote",
            Some(v) if !matches!(v, "false" | "no" | "off" | "0") => "rebases onto remote",
            _ => " merges with remote",
        };
        pulls.push((b, how, merge));
    }
    if !pulls.is_empty() {
        out.push(format!(
            "  {} configured for 'git pull':",
            plural(pulls.len(), "Local branch", "Local branches")
        ));
        let w = pulls.iter().map(|(b, ..)| b.len()).max().unwrap_or(0);
        for (b, how, merge) in &pulls {
            out.push(format!("    {b:<w$} {how} {merge}"));
        }
    }

    if get(&format!("remote.{name}.mirror")) == Some("true") {
        out.push("  Local refs will be mirrored by 'git push'".to_owned());
        return Ok(out.join("\n"));
    }
    let specs: Vec<&str> = config
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case(&format!("remote.{name}.push")))
        .map(|(_, v)| v.as_str())
        .collect();
    let mut pushes: Vec<(String, String, bool)> = Vec::new();
    if specs.is_empty() && query {
        for b in backend.local_branches()? {
            if heads.iter().any(|(h, _)| *h == format!("refs/heads/{b}")) {
                pushes.push((b.clone(), b, false));
            }
        }
    } else if specs.is_empty() {
        pushes.push(("(matching)".to_owned(), "(matching)".to_owned(), false));
    }
    for spec in specs {
        let forced = spec.starts_with('+');
        let (src, dst) = spec
            .trim_start_matches('+')
            .split_once(':')
            .unwrap_or((spec, spec));
        let short = |s: &str| {
            s.strip_prefix("refs/heads/")
                .unwrap_or(s)
                .trim_start_matches('+')
                .to_owned()
        };
        pushes.push((short(src), short(dst), forced));
    }
    if !pushes.is_empty() {
        out.push(format!(
            "  {} configured for 'git push'{not_queried}:",
            plural(pushes.len(), "Local ref", "Local refs")
        ));
        let w1 = pushes.iter().map(|(s, ..)| s.len()).max().unwrap_or(0);
        let w2 = pushes.iter().map(|(_, d, _)| d.len()).max().unwrap_or(0);
        for (src, dst, forced) in &pushes {
            let verb = if *forced { "forces to" } else { "pushes to" };
            if !query {
                out.push(format!("    {src:<w1$} {verb} {dst}"));
                continue;
            }
            let theirs = heads
                .iter()
                .find(|(h, _)| *h == format!("refs/heads/{dst}"))
                .map(|(_, id)| id.as_str());
            let ours = backend.rev_parse(&format!("refs/heads/{src}")).ok();
            let status = match (theirs, ours) {
                (None, _) => "create",
                (Some(t), Some(o)) if t == o => "up to date",
                (Some(t), Some(o)) if backend.is_ancestor(t, &o).unwrap_or(false) => {
                    "fast-forwardable"
                }
                _ => "local out of date",
            };
            out.push(format!("    {src:<w1$} {verb} {dst:<w2$} ({status})"));
        }
    }
    Ok(out.join("\n"))
}

/// Tags matching any of `patterns` (all when none) and `o`'s filters, in its
/// `--sort` order.
pub(crate) fn tag_list(
    backend: &Arc<dyn GitBackend>,
    patterns: &[String],
    o: &TagOpts,
) -> anyhow::Result<Vec<rgit_git::RefDetail>> {
    let mut sort = o.sort.clone();
    if sort.is_empty() {
        sort.extend(backend.config_get("tag.sort").ok().flatten());
    }
    if sort.is_empty() && o.ignore_case {
        sort.push("refname".to_owned());
    }
    let refs = crate::plumbing::sort_refs(backend, backend.ref_details()?, &sort, o.ignore_case)?;
    let fold = |s: &str| {
        if o.ignore_case {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    };
    let patterns: Vec<String> = patterns.iter().map(|p| fold(p)).collect();
    let at = o
        .points_at
        .as_ref()
        .map(|r| backend.rev_parse(r))
        .transpose()?;
    // Tags of non-commits never pass a commit filter, as in git.
    let reach = |a: &str, b: &str, want: bool| backend.is_ancestor(a, b).is_ok_and(|r| r == want);
    let mut out = Vec::new();
    for t in refs {
        let Some(name) = t.name.strip_prefix("refs/tags/") else {
            continue;
        };
        let full = t.name.as_str();
        if (patterns.is_empty() || rgit_git::pathspec_matches(&patterns, &fold(name)))
            && at
                .as_ref()
                .is_none_or(|at| *at == t.id || t.peeled.as_ref() == Some(at))
            && o.contains.as_ref().is_none_or(|c| reach(c, full, true))
            && o.no_contains.as_ref().is_none_or(|c| reach(c, full, false))
            && o.merged.as_ref().is_none_or(|m| reach(full, m, true))
            && o.no_merged.as_ref().is_none_or(|m| reach(full, m, false))
        {
            out.push(t);
        }
    }
    Ok(out)
}

/// A tag, with up to `n` lines of its message laid out like `git tag -n`.
fn tag_with_message(t: &rgit_git::RefDetail, n: Option<usize>) -> String {
    let name = t.name.strip_prefix("refs/tags/").unwrap_or(&t.name);
    let Some(n) = n else {
        return name.to_owned();
    };
    // A signed tag's signature is not part of its message.
    let message = t.message.split("-----BEGIN ").next().unwrap_or_default();
    let body: Vec<&str> = message.lines().take(n).collect();
    format!("{name:<15} {}", body.join("\n    "))
}

/// `git tag <name> [<rev>]`: a lightweight tag, or an annotated one with a
/// message from -m, -F, the editor (-e) or a prompt, signed with -s / -u or
/// `tag.gpgSign`.
fn create_tag(
    backend: &Arc<dyn GitBackend>,
    names: &[String],
    o: TagOpts,
    interactive: bool,
) -> anyhow::Result<String> {
    let (name, rev) = match names {
        [name] => (name, "HEAD"),
        [name, rev] => (name, rev.as_str()),
        _ => {
            return Err(CliError::usage(format!(
                "tag takes a name and an optional revision, got {}",
                names.join(" ")
            )));
        }
    };
    let mut message = match o.file.as_deref() {
        Some("-") => Some(std::io::read_to_string(std::io::stdin())?),
        Some(f) => Some(
            std::fs::read_to_string(f).map_err(|e| anyhow::anyhow!("could not read {f}: {e}"))?,
        ),
        None => o.message,
    };
    let signed = o.sign || o.local_user.is_some();
    let annotated = message.is_some() || o.annotate || signed;
    if o.edit || message.is_none() && annotated {
        if !interactive {
            let what = if o.edit {
                "-e"
            } else {
                "an annotation message (-m)"
            };
            return Err(CliError::usage(format!("{what} needs a terminal")));
        }
        let template = format!(
            "{}\n#\n# Write a message for tag:\n#   {name}\n# Lines starting with '#' will be ignored.\n",
            message.as_deref().unwrap_or_default()
        );
        let text = edit_file(backend, "TAG_EDITMSG", &template)?;
        let text = strip_comments(&text);
        if text.trim().is_empty() {
            anyhow::bail!("no tag message?");
        }
        message = Some(text);
    }
    let gpg_sign = backend
        .config_get("tag.gpgSign")?
        .is_some_and(|v| matches!(v.to_lowercase().as_str(), "true" | "yes" | "on" | "1"));
    let key = match o.local_user {
        Some(k) => Some(k),
        None if o.sign || annotated && gpg_sign && !o.no_sign => Some(String::new()),
        None => None,
    };
    ok(backend.tag_with(
        name,
        rev,
        message.as_deref(),
        o.cleanup.as_deref().unwrap_or("strip"),
        key.as_deref(),
        o.force,
    ))
}

/// `text` without its `#` lines.
fn strip_comments(text: &str) -> String {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Write `text` to `<git dir>/<file>`, open it in git's editor on the
/// terminal and return what was saved.
fn edit_file(backend: &Arc<dyn GitBackend>, file: &str, text: &str) -> anyhow::Result<String> {
    let path = backend.git_dir().join(file);
    std::fs::write(&path, text)?;
    let editor = std::env::var("GIT_EDITOR")
        .ok()
        .or_else(|| backend.config_get("core.editor").ok().flatten())
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vi".to_owned());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()?;
    if !status.success() {
        anyhow::bail!("the editor {editor} failed");
    }
    Ok(std::fs::read_to_string(&path)?)
}

/// The changes stash `index` records, `(tracked, untracked)`: its base
/// commit against its tree, and the untracked files it saved.
pub(crate) fn stash_diff(
    backend: &Arc<dyn GitBackend>,
    index: usize,
    (tracked, untracked): (bool, bool),
) -> anyhow::Result<Vec<rgit_git::FileDiff>> {
    let stash = format!("stash@{{{index}}}");
    let mut files = if tracked {
        backend.diff_refs(&format!("{stash}^1"), &stash)?
    } else {
        Vec::new()
    };
    let saved = format!("{stash}^3");
    if untracked && backend.rev_parse(&saved).is_ok() {
        files.extend(backend.diff(&rgit_git::DiffSpec {
            to: Some(saved),
            ..rgit_git::DiffSpec::default()
        })?);
        files.sort_by(|a, b| a.path.cmp(&b.path));
    }
    Ok(files)
}

/// Paths listed in `file` (`-` for stdin), one per line or NUL-separated.
fn pathspec_from_file(file: &str, nul: bool) -> anyhow::Result<Vec<String>> {
    let text = if file == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(file).map_err(|e| anyhow::anyhow!("could not read {file}: {e}"))?
    };
    Ok(text
        .split(if nul { '\0' } else { '\n' })
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// `git stash list --format`: each stash in a log format.
fn stash_log(
    backend: &Arc<dyn GitBackend>,
    format: &str,
    max: Option<usize>,
) -> anyhow::Result<String> {
    if backend.status()?.stashes.is_empty() {
        return Ok(String::new());
    }
    let mut out = Vec::new();
    for (i, item) in backend
        .reflog("refs/stash")?
        .into_iter()
        .enumerate()
        .take(max.unwrap_or(usize::MAX))
    {
        let Some(c) = backend
            .rev_walk(&rgit_git::RevWalk {
                revs: vec![item.id.clone()],
                max: Some(1),
                ..rgit_git::RevWalk::default()
            })?
            .into_iter()
            .next()
        else {
            continue;
        };
        let mut line = String::new();
        let mut chars = format.chars();
        while let Some(ch) = chars.next() {
            if ch != '%' {
                line.push(ch);
                continue;
            }
            let date = crate::plumbing::format_date;
            let a = &c.author;
            let cm = &c.committer;
            let code: String = match chars.next() {
                Some(c2 @ ('a' | 'c' | 'g')) => [c2].into_iter().chain(chars.next()).collect(),
                Some(c2) => c2.to_string(),
                None => String::new(),
            };
            line.push_str(&match code.as_str() {
                "gd" => format!("stash@{{{i}}}"),
                "gs" => item.message.clone(),
                "H" => c.id.clone(),
                "h" => backend.abbrev_id(&c.id, 7)?,
                "s" => c.summary.clone(),
                "an" => a.name.clone(),
                "ae" => a.email.clone(),
                "ad" => date(a.time, a.offset, "default"),
                "ar" => date(a.time, a.offset, "relative"),
                "cn" => cm.name.clone(),
                "ce" => cm.email.clone(),
                "cd" => date(cm.time, cm.offset, "default"),
                "cr" => date(cm.time, cm.offset, "relative"),
                "n" => "\n".to_owned(),
                "%" => "%".to_owned(),
                other => format!("%{other}"),
            });
        }
        out.push(line);
    }
    Ok(out.join("\n"))
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
pub fn from_cwd(mut command: Command, backend: &Arc<dyn GitBackend>) -> Command {
    let Some((root, prefix)) = std::env::current_dir()
        .and_then(|cwd| cwd.canonicalize())
        .ok()
        .zip(backend.workdir().canonicalize().ok())
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
        | Command::Archive { paths, .. }
        | Command::Add { paths, .. }
        | Command::Restore { paths, .. }
        | Command::Commit { paths, .. }
        | Command::Clean { paths, .. }
        | Command::Rm { paths, .. }
        | Command::Mv { paths, .. }
        | Command::Stash {
            cmd: Some(StashCmd::Push { paths, .. }),
            ..
        }
        | Command::Stash {
            cmd: None, paths, ..
        } => paths.iter_mut().for_each(fix),
        Command::Reset {
            rev,
            pathspec,
            paths,
            ..
        }
        | Command::Checkout {
            rev,
            pathspec,
            paths,
            ..
        } => {
            // A first word naming a file here, even a deleted one git still
            // tracks, is a path, not a revision.
            let tracked = |r: &str| {
                let p = repo_path(&root, &prefix, r);
                backend
                    .index_entries()
                    .is_ok_and(|e| e.iter().any(|e| e.path == p))
                    || backend.read_blob("HEAD", &p).is_ok()
            };
            if let Some(r) = rev
                .as_mut()
                .filter(|r| root.join(&prefix).join(r).exists() || tracked(r))
            {
                fix(r);
            }
            pathspec.iter_mut().chain(paths).for_each(fix);
        }
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
        // A trailing `/` means "inside this folder" to ls-tree and pathspecs.
        Command::Plumbing(
            Plumbing::LsFiles { paths, .. }
            | Plumbing::LsTree {
                paths,
                full_tree: false,
                ..
            }
            | Plumbing::Grep { paths, .. }
            | Plumbing::RevList { paths, .. },
        ) => {
            for p in paths {
                let dir = p.ends_with('/');
                fix(p);
                if dir && !p.ends_with('/') {
                    p.push('/');
                }
            }
        }
        _ => {}
    }
    command
}

/// `p` typed in the repo subfolder `prefix`, as a path from the repo `root`.
/// `:/x` names `x` at the root, as in git.
pub(crate) fn repo_path(root: &Path, prefix: &Path, p: &str) -> String {
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
