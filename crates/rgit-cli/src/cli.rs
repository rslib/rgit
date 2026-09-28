//! The command-line surface. With no subcommand rgit launches the TUI; each
//! subcommand drives the same `GitBackend` and prints git's human text, or
//! TOON/JSON for agents with `--toon`/`--json`.
//!
//! When a required argument is missing and stdout is a real terminal, the
//! missing value is prompted for (our own widgets); with `--no-input` or a non-TTY
//! (an agent, a pipe, CI) it errors instead, so scripted use stays predictable.

use std::path::{Path, PathBuf};
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
                  git backend and print git's human text; agents pass --toon (or --json) for \
                  structured output.",
    after_help = "git's global options go before the command: -C <path>, -c <name>=<value>, \
                  --config-env=<name>=<env>, --git-dir, --work-tree, --bare, --namespace, \
                  -p/--paginate, -P/--no-pager, --literal/--noglob/--icase-pathspecs, \
                  --no-optional-locks, --no-advice, --exec-path. An unknown command runs \
                  alias.<name>, or rgit-<name>/git-<name> from PATH."
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

    /// Print TOON for agents: no color, spinners, or prompts. Agents should
    /// always pass it (or --json); human text is the default.
    #[arg(long, visible_alias = "axi", global = true, conflicts_with = "human")]
    pub toon: bool,

    /// Print human text (the default).
    #[arg(long, alias = "text", global = true)]
    pub human: bool,

    /// Print rgit's compact human forms of status, diff, log, show and blame
    /// instead of git's (config rgit.compact).
    #[arg(long, global = true)]
    pub compact: bool,

    /// Disable ANSI color even on a terminal.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Color output: always, never or auto (a terminal); overrides the
    /// color.* config. Bare `--color` means always.
    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "always"
    )]
    pub color: Option<String>,

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
    pub fn output_mode(&self) -> OutputMode {
        if self.json {
            OutputMode::Json
        } else if self.toon {
            OutputMode::Porcelain
        } else {
            OutputMode::Text
        }
    }
}

/// git's function-context and word-diff options of `log`, `diff` and `show`.
#[derive(clap::Args, Clone, Default)]
pub struct WordDiffArgs {
    /// Show the whole function around each change as context (git's -W).
    #[arg(short = 'W', long)]
    pub function_context: bool,
    /// Diff by words: `plain` (`[-old-]{+new+}`, the default), `color`,
    /// `porcelain` or `none`.
    #[arg(
        long,
        value_name = "MODE",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "plain",
        value_parser = ["plain", "color", "porcelain", "none"]
    )]
    pub word_diff: Option<String>,
    /// What a word is for --word-diff (implies it): an extended regex.
    #[arg(long, value_name = "REGEX")]
    pub word_diff_regex: Option<String>,
    /// Word diff in color only (git's --word-diff=color), words matching the
    /// optional regex.
    #[arg(
        long,
        value_name = "REGEX",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub color_words: Option<String>,
}

impl WordDiffArgs {
    /// The word-diff style and regex asked for, if any.
    fn words(&self) -> Option<(rgit_git::userdiff::WordStyle, Option<String>)> {
        use rgit_git::userdiff::WordStyle;
        if let Some(re) = &self.color_words {
            return Some((WordStyle::Color, Some(re.clone()).filter(|r| !r.is_empty())));
        }
        let style = match self.word_diff.as_deref() {
            Some("none") => return None,
            Some("color") => WordStyle::Color,
            Some("porcelain") => WordStyle::Porcelain,
            Some(_) => WordStyle::Plain,
            None if self.word_diff_regex.is_some() => WordStyle::Plain,
            None => return None,
        };
        Some((style, self.word_diff_regex.clone()))
    }
}

static FUNCTION_CONTEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this run's diffs take `-W`.
fn function_context() -> bool {
    FUNCTION_CONTEXT.load(std::sync::atomic::Ordering::Relaxed)
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
    /// Only the diffstat's last line: files changed, insertions, deletions.
    #[arg(long)]
    pub shortstat: bool,
    /// git's raw format: `:<modes> <ids> <status>\t<path>` per file.
    #[arg(long)]
    pub raw: bool,
}

impl DiffFormat {
    pub(crate) fn any(self) -> bool {
        self.raw
            || crate::diffopts::formats()
            || self.patch
            || self.stat
            || self.name_only
            || self.name_status
            || self.numstat
            || self.shortstat
    }
}

const MERGE_DIFF_FLAGS: [&str; 7] = [
    "separate",
    "combined",
    "dense_combined",
    "first_parent_diff",
    "diff_merges",
    "no_diff_merges",
    "remerge_diff",
];

/// Which diff `log` and `show` print for a merge (git's --diff-merges); the
/// last one given wins.
#[derive(clap::Args, Clone, Default)]
pub struct MergeDiffArgs {
    /// Diff each merge against every parent in turn, once -p is given (git's -m).
    #[arg(short = 'm', overrides_with_all = MERGE_DIFF_FLAGS)]
    pub separate: bool,
    /// git's combined diff of merges against all parents (implies -p).
    #[arg(short = 'c', overrides_with_all = MERGE_DIFF_FLAGS)]
    pub combined: bool,
    /// git's dense combined diff: only the hunks that differ from every
    /// parent (implies -p; the default for show).
    #[arg(long = "cc", overrides_with_all = MERGE_DIFF_FLAGS)]
    pub dense_combined: bool,
    /// Diff merges against their first parent (implies -p).
    #[arg(long = "dd", overrides_with_all = MERGE_DIFF_FLAGS)]
    pub first_parent_diff: bool,
    /// How to diff merges: off, first-parent, separate, combined,
    /// dense-combined or remerge (implies -p unless off).
    #[arg(long, value_name = "FORMAT", overrides_with_all = MERGE_DIFF_FLAGS)]
    pub diff_merges: Option<String>,
    /// Print no diff for merges.
    #[arg(long, overrides_with_all = MERGE_DIFF_FLAGS)]
    pub no_diff_merges: bool,
    /// Diff each merge against git's own re-merge of its parents, conflict
    /// markers and all (implies -p).
    #[arg(long, overrides_with_all = MERGE_DIFF_FLAGS)]
    pub remerge_diff: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeDiff {
    Off,
    FirstParent,
    Separate,
    Combined,
    Dense,
    Remerge,
}

impl MergeDiffArgs {
    /// The merge diff asked for, else `default`, and whether it turns on -p.
    pub(crate) fn resolve(&self, default: MergeDiff) -> anyhow::Result<(MergeDiff, bool)> {
        use MergeDiff::*;
        Ok(if let Some(v) = &self.diff_merges {
            let mode = match v.as_str() {
                "off" | "none" => return Ok((Off, false)),
                "on" | "m" | "separate" => Separate,
                "first-parent" | "1" => FirstParent,
                "combined" | "c" => Combined,
                "dense-combined" | "cc" => Dense,
                "remerge" | "r" => Remerge,
                _ => {
                    return Err(CliError::usage(format!(
                        "invalid value for '--diff-merges': '{v}'"
                    )));
                }
            };
            (mode, true)
        } else if self.no_diff_merges {
            (Off, false)
        } else if self.separate {
            (Separate, false)
        } else if self.combined {
            (Combined, true)
        } else if self.dense_combined {
            (Dense, true)
        } else if self.first_parent_diff {
            (FirstParent, true)
        } else if self.remerge_diff {
            (Remerge, true)
        } else {
            (default, false)
        })
    }
}

/// How `blame` prints each line.
#[derive(clap::Args, Clone, Default)]
pub struct BlameFormat {
    /// git's machine-readable format, each commit's details given once.
    #[arg(short = 'p', long, conflicts_with = "line_porcelain")]
    pub porcelain: bool,
    /// git's machine-readable format with the details on every line.
    #[arg(long)]
    pub line_porcelain: bool,
    /// The author's email instead of the name (git's -e).
    #[arg(short = 'e', long)]
    pub show_email: bool,
    /// Leave out the author (git's -s).
    #[arg(short = 's')]
    pub no_author: bool,
    /// Full commit ids (git's -l).
    #[arg(short = 'l')]
    pub long_ids: bool,
    /// git's format with raw timestamps (git's -t).
    #[arg(short = 't')]
    pub raw_time: bool,
    /// git's format with dates in this style (default iso, or blame.date).
    #[arg(long, value_name = "STYLE")]
    pub date: Option<String>,
    /// git's format with each line's file name (git's -f).
    #[arg(short = 'f', long)]
    pub show_name: bool,
    /// git's format with each line's number in its commit (git's -n).
    #[arg(short = 'n', long)]
    pub show_number: bool,
    /// git annotate's format (git's -c).
    #[arg(short = 'c')]
    pub annotate: bool,
    /// Blank ids for boundary commits (git's -b).
    #[arg(short = 'b')]
    pub blank_boundary: bool,
    /// git's format with ids this many digits long.
    #[arg(long, value_name = "N")]
    pub abbrev: Option<usize>,
    /// git's format, then the work done: blobs read, patches, commits.
    #[arg(long)]
    pub show_stats: bool,
    /// git's machine format, each group of lines as blame settles it.
    #[arg(long, conflicts_with_all = ["porcelain", "line_porcelain"])]
    pub incremental: bool,
    /// git's default format; set unless --compact.
    #[arg(skip)]
    pub git_format: bool,
}

impl BlameFormat {
    /// Whether to print git's own blame format.
    fn git(&self, opts: &BlameArgs) -> bool {
        self.git_format
            || self.raw_time
            || self.date.is_some()
            || self.show_name
            || self.show_number
            || self.annotate
            || self.blank_boundary
            || self.abbrev.is_some()
            || self.show_stats
            || opts.root
    }
}

/// Which lines `blame` follows, and how far.
#[derive(clap::Args, Clone, Default)]
pub struct BlameArgs {
    /// Ignore whitespace when comparing a commit with its parents (git's -w).
    #[arg(short = 'w')]
    pub ignore_whitespace: bool,
    /// Find lines moved or copied within the file (git's -M[score]).
    #[arg(short = 'M')]
    pub moves: bool,
    #[arg(long, hide = true)]
    pub move_score: Option<u32>,
    /// Find lines copied from files changed in the same commit (git's
    /// -C[score]); twice: from any file when the file is created; three
    /// times: from any file in any commit.
    #[arg(short = 'C', action = clap::ArgAction::Count)]
    pub copies: u8,
    #[arg(long, hide = true)]
    pub copy_score: Option<u32>,
    /// Pass this revision's changes through to the lines before them.
    #[arg(long, value_name = "REV")]
    pub ignore_rev: Vec<String>,
    /// Ignore the revisions listed in this file (after blame.ignoreRevsFile;
    /// an empty name forgets the files before it).
    #[arg(long, value_name = "FILE")]
    pub ignore_revs_file: Vec<String>,
    /// For each line, the last commit it was still in (walk `A..B` forward).
    #[arg(long)]
    pub reverse: bool,
    /// Follow only the first parent of merges.
    #[arg(long)]
    pub first_parent: bool,
    /// Blame root commits too, not as boundaries (and print git's format).
    #[arg(long)]
    pub root: bool,
    /// Blame this file's contents (`-` for stdin) as the working tree's
    /// version, on top of the revision given (default HEAD).
    #[arg(long, value_name = "FILE")]
    pub contents: Option<String>,
    /// Report progress on a terminal (accepted; blame runs in-process).
    #[arg(long, overrides_with = "no_progress")]
    pub progress: bool,
    #[arg(long, hide = true)]
    pub no_progress: bool,
    /// The encoding of author names and summaries (UTF-8, or `none`).
    #[arg(long, value_name = "ENCODING")]
    pub encoding: Option<String>,
}

/// How `log` and `rev-list` walk history, as git's revision options.
#[derive(clap::Args, Clone, Default)]
pub struct WalkArgs {
    /// Mark each commit with the side of a symmetric range it is on (`<`, `>`).
    #[arg(long)]
    pub left_right: bool,
    /// Leave out commits whose change the other side of a symmetric range has too.
    #[arg(long)]
    pub cherry_pick: bool,
    /// Mark commits whose change the other side has too `=`, the others `+`.
    #[arg(long)]
    pub cherry_mark: bool,
    /// The right side's commits, marking those the left side has too
    /// (`--right-only --cherry-mark --no-merges`).
    #[arg(long)]
    pub cherry: bool,
    /// Only the left side of a symmetric range.
    #[arg(long, conflicts_with = "right_only")]
    pub left_only: bool,
    /// Only the right side of a symmetric range.
    #[arg(long)]
    pub right_only: bool,
    /// After the commits, the excluded ones they have as parents, marked `-`.
    #[arg(long)]
    pub boundary: bool,
    /// Only commits that descend from the range's excluded end.
    #[arg(long)]
    pub ancestry_path: bool,
    /// Only commits that a branch or tag points at.
    #[arg(long)]
    pub simplify_by_decoration: bool,
    /// With paths, leave out merges the shown history does not need.
    #[arg(long)]
    pub simplify_merges: bool,
    /// With paths, follow every parent of a merge.
    #[arg(long)]
    pub full_history: bool,
    /// With paths, show only commits that change them (the default).
    #[arg(long)]
    pub dense: bool,
    /// With paths, show every commit walked.
    #[arg(long, overrides_with = "dense")]
    pub sparse: bool,
    /// Name the starting ref that reached each commit.
    #[arg(long)]
    pub source: bool,
    /// During a conflicted merge, the commits on either side that touch the
    /// conflicted paths.
    #[arg(long)]
    pub merge: bool,
    /// Show only the given commits, newest first (`=unsorted`: as given).
    #[arg(
        long,
        value_name = "sorted|unsorted",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "sorted"
    )]
    pub no_walk: Option<String>,
    /// Walk the history (undoes --no-walk).
    #[arg(long)]
    pub do_walk: bool,
    /// Never show a parent before all of its children, keeping branches together.
    #[arg(long)]
    pub topo_order: bool,
    /// Never show a parent before all of its children, else by commit date.
    #[arg(long)]
    pub date_order: bool,
    /// Never show a parent before all of its children, else by author date.
    #[arg(long)]
    pub author_date_order: bool,
    /// Keep commits matching every --grep, not any.
    #[arg(long)]
    pub all_match: bool,
    /// Keep commits whose message matches no --grep.
    #[arg(long)]
    pub invert_grep: bool,
}

impl WalkArgs {
    /// Set what these options ask of a walk.
    pub(crate) fn apply(&self, o: &mut LogOptions) {
        if self.cherry {
            o.side = Some(false);
            o.cherry = Some(false);
            o.merges = Some(false);
        }
        if self.left_only {
            o.side = Some(true);
        } else if self.right_only {
            o.side = Some(false);
        }
        if self.cherry_pick {
            o.cherry = Some(true);
        } else if self.cherry_mark {
            o.cherry.get_or_insert(false);
        }
        o.boundary = self.boundary;
        o.ancestry_path = self.ancestry_path;
        o.simplify_by_decoration = self.simplify_by_decoration;
        o.simplify_merges = self.simplify_merges;
        o.full_history = self.full_history;
        o.sparse = self.sparse;
        o.source = self.source;
        o.merge = self.merge;
        o.no_walk = match &self.no_walk {
            Some(v) if !self.do_walk => Some(v != "unsorted"),
            _ => None,
        };
        o.order = if self.topo_order {
            rgit_git::LogOrder::Topo
        } else if self.date_order {
            rgit_git::LogOrder::Date
        } else if self.author_date_order {
            rgit_git::LogOrder::AuthorDate
        } else {
            o.order
        };
        o.all_match = self.all_match;
        o.invert_grep = self.invert_grep;
    }

    /// The mark shown before a commit (git's get_revision_mark), given the
    /// walk's own mark for it.
    pub(crate) fn mark(&self, mark: Option<char>) -> Option<char> {
        match mark {
            Some(m @ ('-' | '=')) => Some(m),
            m if self.left_right => Some(m.unwrap_or('>')),
            _ if self.cherry_mark || self.cherry => Some('+'),
            _ => None,
        }
    }
}

/// `rev-list`'s output options beyond the walk.
#[derive(clap::Args, Clone, Default)]
pub struct RevListArgs {
    /// With --objects, print only the ids, not the objects' paths.
    #[arg(long, overrides_with = "object_names")]
    pub no_object_names: bool,
    /// With --objects, print each object's path after its id (the default).
    #[arg(long)]
    pub object_names: bool,
    /// Leave objects out of --objects: `blob:none`, `blob:limit=<n>[kmg]`,
    /// `tree:<depth>`, `object:type=<type>` or `combine:<a>+<b>`.
    #[arg(long, value_name = "SPEC")]
    pub filter: Vec<String>,
    /// Forget the --filter options before it.
    #[arg(long)]
    pub no_filter: bool,
    /// Also print the objects the filter left out, as `~<id>`.
    #[arg(long)]
    pub filter_print_omitted: bool,
    /// Print the total size on disk of what would be listed instead
    /// (`=human` for KiB, MiB, GiB).
    #[arg(
        long,
        value_name = "human",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub disk_usage: Option<String>,
    /// Print each commit's children after it.
    #[arg(long)]
    pub children: bool,
    /// Print each commit's committer timestamp before it.
    #[arg(long)]
    pub timestamp: bool,
    /// Each commit in raw form, NUL-terminated.
    #[arg(long)]
    pub header: bool,
    /// Print each commit in this format (`%h %s`), after a `commit <id>` line.
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<String>,
    /// Print each commit in a named format (oneline, short, medium, full,
    /// fuller, raw) or `format:<string>`.
    #[arg(
        long,
        value_name = "FORMAT",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "medium"
    )]
    pub pretty: Option<String>,
    /// `--pretty=oneline --abbrev-commit`.
    #[arg(long)]
    pub oneline: bool,
    /// The date style for the formats (`%ad`).
    #[arg(long, value_name = "STYLE")]
    pub date: Option<String>,
    /// With --format, leave out the `commit <id>` lines.
    #[arg(long, overrides_with = "commit_header")]
    pub no_commit_header: bool,
    /// With --format, print the `commit <id>` lines (the default).
    #[arg(long)]
    pub commit_header: bool,
    /// Print the commit that halves the range (bisection's next step).
    #[arg(long)]
    pub bisect: bool,
    /// Print shell variables for bisection's next step.
    #[arg(long)]
    pub bisect_vars: bool,
    /// Print every commit with its distance from the range's ends, best first.
    #[arg(long)]
    pub bisect_all: bool,
    /// Print nothing; only the exit status says whether the walk worked.
    #[arg(long)]
    pub quiet: bool,
    /// Abbreviate ids to at least N hex digits.
    #[arg(long, value_name = "N")]
    pub abbrev: Option<usize>,
    /// Read more revisions (and, after a `--` line, paths) from stdin.
    #[arg(long)]
    pub stdin: bool,
    /// Leave refs matching this glob out of --all, --branches, --tags and --remotes.
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,
}

/// git's own `status` formats; any of them prints git's output instead of
/// rgit's compact view.
#[derive(clap::Args, Clone, Default)]
pub struct StatusArgs {
    /// git's script format, `v1` (default) or `v2`. Agents should prefer `--toon`.
    #[arg(
        long,
        value_name = "VERSION",
        num_args = 0..=1,
        require_equals = true,
        value_parser = ["v1", "v2"],
        default_missing_value = "v1"
    )]
    pub porcelain: Option<String>,
    /// git's short format.
    #[arg(short, long)]
    pub short: bool,
    /// git's long format (git's default).
    #[arg(long)]
    pub long: bool,
    /// Add branch and tracking info.
    #[arg(short, long, overrides_with = "no_branch")]
    pub branch: bool,
    #[arg(long, hide = true)]
    pub no_branch: bool,
    /// Terminate entries with NUL.
    #[arg(short = 'z', long = "null")]
    pub z: bool,
    /// Also show the staged diff; repeat for the unstaged one.
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Show the number of stashes.
    #[arg(long, overrides_with = "no_show_stash")]
    pub show_stash: bool,
    #[arg(long, hide = true)]
    pub no_show_stash: bool,
    /// Count commits ahead of and behind the upstream (git's default).
    #[arg(long, overrides_with = "no_ahead_behind")]
    pub ahead_behind: bool,
    /// Only say whether the branch differs from its upstream.
    #[arg(long)]
    pub no_ahead_behind: bool,
    /// Ignore submodule changes: `none`, `untracked`, `dirty` or `all` (default).
    #[arg(
        long,
        value_name = "WHEN",
        num_args = 0..=1,
        require_equals = true,
        value_parser = ["none", "untracked", "dirty", "all"],
        default_missing_value = "all"
    )]
    pub ignore_submodules: Option<String>,
    /// List untracked files in columns (git's column.status options).
    #[arg(
        long,
        value_name = "OPTIONS",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub column: Option<String>,
    #[arg(long, hide = true)]
    pub no_column: bool,
    /// Do not detect renames.
    #[arg(long)]
    pub no_renames: bool,
    /// Detect renames, optionally at this similarity (git's -M).
    #[arg(
        short = 'M',
        long,
        value_name = "N",
        num_args = 0..=1,
        default_missing_value = "",
        value_parser = |v: &str| {
            let score = v.trim_end_matches('%');
            (score.is_empty() || score.parse::<f64>().is_ok())
                .then(|| v.to_owned())
                .ok_or_else(|| format!("invalid argument to -M: {v}"))
        }
    )]
    pub find_renames: Option<String>,
}

impl StatusArgs {
    /// Whether any git status flag asks for git's own output.
    pub fn any(&self) -> bool {
        self.porcelain.is_some()
            || self.short
            || self.long
            || self.branch
            || self.no_branch
            || self.z
            || self.verbose > 0
            || self.show_stash
            || self.no_show_stash
            || self.ahead_behind
            || self.no_ahead_behind
            || self.ignore_submodules.is_some()
            || self.column.is_some()
            || self.no_column
            || self.no_renames
            || self.find_renames.is_some()
    }

    /// The options for git's status renderer.
    pub fn opts(
        &self,
        untracked: Option<&str>,
        ignored: Option<&str>,
        paths: &[String],
    ) -> rgit_git::StatusOpts {
        use rgit_git::StatusFormat;
        let flag = |on: bool, off: bool| (on || off).then_some(on);
        rgit_git::StatusOpts {
            format: match self.porcelain.as_deref() {
                Some("v2") => Some(StatusFormat::PorcelainV2),
                Some(_) => Some(StatusFormat::Porcelain),
                None if self.short => Some(StatusFormat::Short),
                None if self.long => Some(StatusFormat::Long),
                None => None,
            },
            branch: flag(self.branch, self.no_branch),
            null: self.z,
            show_stash: flag(self.show_stash, self.no_show_stash),
            ahead_behind: flag(self.ahead_behind, self.no_ahead_behind),
            untracked: untracked.map(str::to_owned),
            ignored: ignored.map(str::to_owned),
            ignore_submodules: self.ignore_submodules.clone(),
            column: if self.no_column {
                Some("never".to_owned())
            } else {
                self.column.clone()
            },
            renames: self.no_renames.then_some(false),
            find_renames: self.find_renames.clone(),
            verbose: self.verbose,
            paths: paths.to_vec(),
            ..Default::default()
        }
    }
}

/// git's commit formats for `log` and `show`, printed byte for byte as git does.
#[derive(clap::Args, Clone, Default)]
pub struct PrettyArgs {
    /// Print commits in git's format: oneline, short, medium, full, fuller,
    /// raw, reference, or `format:<string>` / `tformat:<string>` with
    /// placeholders such as %H %h %s %b %an %ae %ad %ar %d %n.
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<String>,
    /// The same as --format; a bare `--pretty` is medium.
    #[arg(
        long,
        value_name = "FORMAT",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "medium"
    )]
    pub pretty: Option<String>,
    /// git's `<short sha> <subject>` lines.
    #[arg(long)]
    pub oneline: bool,
    /// Dates in the formats: default, relative, local, iso, iso-strict, rfc,
    /// short, raw, unix or format:<strftime> (`-local` for local time).
    #[arg(long, value_name = "STYLE")]
    pub date: Option<String>,
    /// Draw the commit graph in ASCII, as git does.
    #[arg(long)]
    pub graph: bool,
    /// Name the refs at each commit (default: only on a terminal).
    #[arg(
        long,
        value_name = "STYLE",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "short"
    )]
    pub decorate: Option<String>,
    /// Never name refs at commits.
    #[arg(long)]
    pub no_decorate: bool,
    /// Abbreviate the commit ids in the format's header.
    #[arg(long)]
    pub abbrev_commit: bool,
    /// Check each commit's signature and show the verifier's report.
    #[arg(long)]
    pub show_signature: bool,
    /// Show the notes of the default notes refs, or of this notes ref
    /// (repeatable; any format).
    #[arg(
        long,
        value_name = "REF",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub notes: Vec<String>,
    /// Show no notes (a later --notes=<ref> still shows that ref).
    #[arg(long)]
    pub no_notes: bool,
}

impl PrettyArgs {
    pub(crate) fn any(&self) -> bool {
        self.format.is_some()
            || self.pretty.is_some()
            || self.oneline
            || self.graph
            || self.show_signature
            || !self.notes.is_empty()
            || self.no_notes
    }
}

/// git's `-S[<keyid>]` / `--no-gpg-sign`.
#[derive(clap::Args, Default, Clone)]
pub struct SignArgs {
    /// Sign the commit with gpg, gpgsm or ssh-keygen (per gpg.format), with
    /// user.signingKey or `-S<KEYID>` (git's -S).
    #[arg(
        short = 'S',
        long = "gpg-sign",
        value_name = "KEYID",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub gpg_sign: Option<String>,
    /// Do not sign, even with commit.gpgSign set.
    #[arg(long, overrides_with = "gpg_sign")]
    pub no_gpg_sign: bool,
}

/// git's --pathspec-from-file and --pathspec-file-nul.
#[derive(clap::Args, Clone, Default)]
pub struct PathspecFile {
    /// Read the pathspecs from this file, one per line (`-` for stdin).
    #[arg(long = "pathspec-from-file", value_name = "FILE")]
    pub pathspec_from_file: Option<String>,
    /// With --pathspec-from-file, the pathspecs are NUL-separated.
    #[arg(long = "pathspec-file-nul", requires = "pathspec_from_file")]
    pub pathspec_file_nul: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Working-tree status as git prints it (`--compact` for rgit's short form).
    /// Agents: add `--toon` for a structured table. `--porcelain`, `--short`,
    /// `--branch`, `-z`, `-v` and git's other status flags print git's own
    /// formats byte for byte, for scripts.
    Status {
        #[command(flatten)]
        fmt: StatusArgs,
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
        /// Also list ignored files: `traditional` (default), `matching` or `no`.
        #[arg(
            long,
            value_name = "MODE",
            num_args = 0..=1,
            require_equals = true,
            value_parser = ["traditional", "matching", "no"],
            default_missing_value = "traditional"
        )]
        ignored: Option<String>,
        /// Limit to these paths: files, folders or globs.
        paths: Vec<String>,
    },
    /// Commits in git's medium format (`--compact` for `sha subject` lines), or
    /// in git's other formats with `--oneline`, `--format`, `--pretty` or `--graph`.
    #[command(visible_alias = "whatchanged")]
    Log {
        /// Maximum number of commits to show (git's -n or -<n>; default 20, or
        /// all in a git format).
        #[arg(
            short = 'n',
            short_alias = 'l',
            long = "max-count",
            visible_alias = "limit"
        )]
        limit: Option<usize>,
        /// Skip this many commits before showing any.
        #[arg(long, value_name = "N", default_value_t = 0)]
        skip: usize,
        /// Walk every ref, not just HEAD.
        #[arg(long)]
        all: bool,
        /// Keep only commits whose author name/email contains this.
        #[arg(long)]
        author: Option<String>,
        /// Keep only commits whose committer name/email contains this.
        #[arg(long)]
        committer: Option<String>,
        /// Keep only commits that change how often this string occurs (git's -S).
        #[arg(short = 'S', value_name = "STRING")]
        occurrences: Option<String>,
        /// Keep only commits that add or remove a line matching this regex (git's -G).
        #[arg(short = 'G', value_name = "REGEX")]
        changes_matching: Option<String>,
        /// Only commits at or after this date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS).
        #[arg(long, visible_alias = "after")]
        since: Option<String>,
        /// Only commits at or before this date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS).
        #[arg(long, visible_alias = "before")]
        until: Option<String>,
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
        /// Trace the history of lines `START,END:FILE` (`/regex/`, `+N`
        /// allowed) or function `:NAME:FILE`, with their diffs (git's -L).
        #[arg(short = 'L', value_name = "RANGE:FILE")]
        line_ranges: Vec<String>,
        /// Walk reflog entries (of HEAD, or the refs given) instead of history.
        #[arg(short = 'g', long)]
        walk_reflogs: bool,
        /// Print each commit's parents after it (the nearest shown ones under
        /// path limits).
        #[arg(long)]
        parents: bool,
        #[command(flatten)]
        walk: WalkArgs,
        #[command(flatten)]
        format: DiffFormat,
        #[command(flatten)]
        words: Box<WordDiffArgs>,
        #[command(flatten)]
        diff_opts: Box<crate::diffopts::DiffOptArgs>,
        #[command(flatten)]
        merge_diff: MergeDiffArgs,
        #[command(flatten)]
        pretty: PrettyArgs,
        /// Revisions to walk (`main`, `^main`, `A..B`, `A...B`; default HEAD),
        /// then paths: `rgit log <rev>...` or `rgit log <rev> -- <path>...`.
        #[arg(value_name = "REV_OR_PATH")]
        revs: Vec<String>,
        /// Limit to commits touching these paths (after `--`).
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Patch of unstaged changes (`--cached` for staged), against a revision,
    /// or between two revisions (`A B`, `A..B`, `A...B`); `--compact` for a diffstat.
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
        #[command(flatten)]
        words: Box<WordDiffArgs>,
        #[command(flatten)]
        diff_opts: Box<crate::diffopts::DiffOptArgs>,
        /// Lines of context around each change (git's -U, default 3).
        #[arg(short = 'U', long = "unified", value_name = "N")]
        unified: Option<u32>,
        /// Ignore whitespace when comparing lines.
        #[arg(short = 'w', long = "ignore-all-space")]
        ignore_all_space: bool,
        /// Ignore changes in the amount of whitespace.
        #[arg(short = 'b', long = "ignore-space-change")]
        ignore_space_change: bool,
        /// Exit 1 when there are differences, 0 when there are none.
        #[arg(long)]
        exit_code: bool,
        /// Print nothing; only exit 1 when there are differences.
        #[arg(long)]
        quiet: bool,
        /// Compare two files on disk (`--no-index <a> <b>`); exits 1 when they differ.
        #[arg(long)]
        no_index: bool,
        /// Swap the two sides (git's -R).
        #[arg(short = 'R')]
        reverse: bool,
        /// Only files whose status letter is in this set (`AM`); lowercase
        /// letters leave those out (`d`), as git's --diff-filter.
        #[arg(long, value_name = "ACDMRT")]
        diff_filter: Option<String>,
    },
    /// A commit as git shows it (`--compact` for header and diffstat), or a file
    /// (`rev:path`) or folder at a revision.
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
        #[command(flatten)]
        words: Box<WordDiffArgs>,
        #[command(flatten)]
        diff_opts: Box<crate::diffopts::DiffOptArgs>,
        #[command(flatten)]
        merge_diff: MergeDiffArgs,
        #[command(flatten)]
        pretty: PrettyArgs,
        /// Only the header, no changed files (git's -s).
        #[arg(short = 's', long = "no-patch")]
        no_patch: bool,
        /// Diff a merge against its first parent only.
        #[arg(long)]
        first_parent: bool,
    },
    /// Blame a file in git's format, with its display flags (`-t`, `--date`,
    /// `-f`, `-n`, `-c`, `--root`...); `--compact` for `sha author line`.
    #[command(visible_alias = "annotate")]
    Blame {
        /// `[REV] PATH`: the file to annotate, as it is in the working tree or
        /// at REV (`rgit blame <rev> -- <path>` also works); `A..B` or `^A`
        /// stops at A.
        #[arg(value_name = "REV_OR_PATH", required = true, num_args = 1..)]
        args: Vec<String>,
        /// Limit to line ranges (repeatable): `START,END`, `START,+COUNT`,
        /// `/regex/`, `/regex/,+COUNT` or `:funcname` (git's -L).
        #[arg(short = 'L', value_name = "START,END")]
        lines: Vec<String>,
        #[command(flatten)]
        format: BlameFormat,
        #[command(flatten)]
        opts: BlameArgs,
    },
    #[command(flatten)]
    Plumbing(Plumbing),
    #[command(flatten)]
    Extra(crate::extra::Extra),
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
        #[command(flatten)]
        pathspec_file: PathspecFile,
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
        /// Pick what to stage from git's interactive menu (needs a terminal).
        #[arg(short = 'i', long)]
        interactive: bool,
        /// Edit the unstaged diff in the editor and stage what is left.
        #[arg(short = 'e', long)]
        edit: bool,
        /// Stage tracked files again with the clean filters and line
        /// endings applied afresh (implies -u).
        #[arg(long)]
        renormalize: bool,
        /// Set the executable bit of the added files in the index: `+x` or `-x`.
        #[arg(
            long,
            value_name = "(+|-)x",
            value_parser = ["+x", "-x"],
            allow_hyphen_values = true
        )]
        chmod: Option<String>,
        /// Also update paths outside the sparse checkout.
        #[arg(long)]
        sparse: bool,
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
        #[arg(required_unless_present_any = ["patch", "pathspec_from_file"])]
        paths: Vec<String>,
        #[command(flatten)]
        pathspec_file: PathspecFile,
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
        /// Recreate the merge conflicts of these paths (git's -m).
        #[arg(short = 'm', long, conflicts_with_all = ["source", "staged"])]
        merge: bool,
        /// Recreate the conflicts with this marker style: merge, diff3 or zdiff3.
        #[arg(
            long,
            value_name = "STYLE",
            value_parser = ["merge", "diff3", "zdiff3"],
            conflicts_with_all = ["source", "staged"]
        )]
        conflict: Option<String>,
        /// Keep files the source lacks instead of removing them.
        #[arg(long, overrides_with = "no_overlay")]
        overlay: bool,
        /// Remove files the source lacks (the default).
        #[arg(long, hide = true)]
        no_overlay: bool,
        /// Pick hunks to restore, one by one (needs a terminal).
        #[arg(short = 'p', long)]
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
        /// Show what would be committed, without committing: git's long
        /// status, or `--short`, `--porcelain`, `-z`.
        #[arg(long)]
        dry_run: bool,
        /// A dry run in git's short status format.
        #[arg(long)]
        short: bool,
        /// A dry run in git's porcelain status format.
        #[arg(long)]
        porcelain: bool,
        /// A dry run in git's long status format.
        #[arg(long)]
        long: bool,
        /// A dry run with NUL-terminated entries (porcelain unless --short).
        #[arg(short = 'z', long = "null")]
        null: bool,
        /// Show the branch in a --short or --porcelain dry run.
        #[arg(long)]
        branch: bool,
        /// Untracked files in the dry run or template: `no`, `normal` or `all`.
        #[arg(
            short = 'u',
            long = "untracked-files",
            value_name = "MODE",
            num_args = 0..=1,
            value_parser = ["no", "normal", "all"],
            default_missing_value = "all"
        )]
        untracked: Option<String>,
        /// Put the status in the editor's message template (git's default;
        /// commit.status).
        #[arg(long, overrides_with = "no_status")]
        status: bool,
        /// Leave the status out of the message template.
        #[arg(long)]
        no_status: bool,
        /// Start the message from this file (commit.template).
        #[arg(short = 't', long, value_name = "FILE")]
        template: Option<String>,
        /// Stage the given paths too, then commit the whole index (git's -i).
        #[arg(short = 'i', long, requires = "paths")]
        include: bool,
        /// Commit only the given paths (the default with paths; git's -o).
        #[arg(short = 'o', long, hide = true)]
        only: bool,
        /// Show the diff to commit below the message template, and in a dry
        /// run; repeat for the unstaged diff too (commit.verbose).
        #[arg(short = 'v', long, action = clap::ArgAction::Count)]
        verbose: u8,
        /// Do not show the diff in the template, whatever commit.verbose says.
        #[arg(long)]
        no_verbose: bool,
        /// Pick the hunks to commit first, as `add -p` does.
        #[arg(short = 'p', long)]
        patch: bool,
        /// Pick what to commit first from `add -i`'s menu.
        #[arg(long)]
        interactive: bool,
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
        #[command(flatten)]
        sign: SignArgs,
        /// Commit only these paths, as they are in the working tree; other
        /// staged changes stay staged.
        paths: Vec<String>,
        #[command(flatten)]
        pathspec_file: PathspecFile,
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
        /// Name each removed object.
        #[arg(short, long)]
        verbose: bool,
        /// Only objects older than this (default: all).
        #[arg(long, value_name = "DATE")]
        expire: Option<String>,
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
        /// Fetch up to N remotes at once (default: `fetch.parallel`).
        #[arg(short = 'j', long, value_name = "N")]
        jobs: Option<usize>,
        /// Add to FETCH_HEAD instead of replacing it.
        #[arg(short = 'a', long)]
        append: bool,
        /// Leave out objects, e.g. `blob:none` or `tree:0`, and make the
        /// remote a promisor that later reads fetch them from.
        #[arg(long, value_name = "SPEC")]
        filter: Option<String>,
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
        /// Submodule commits the pushed history records: `check` refuses
        /// unpushed ones, `on-demand` pushes them first, `only` pushes just
        /// them, `no` ignores them (default: `push.recurseSubmodules`).
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
        #[arg(long, value_name = "STYLE", value_parser = ["merge", "diff3", "zdiff3"])]
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
        #[command(flatten)]
        pathspec_file: PathspecFile,
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
        #[arg(long, value_name = "STYLE", value_parser = ["merge", "diff3", "zdiff3"])]
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
        #[command(flatten)]
        sign: SignArgs,
        #[command(flatten)]
        rerere: RerereFlags,
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
        /// Stash local changes first and reapply them after (default: merge.autoStash).
        #[arg(long)]
        autostash: bool,
        /// Do not stash local changes first (overrides merge.autoStash).
        #[arg(long = "no-autostash", conflicts_with = "autostash")]
        no_autostash: bool,
        /// Word the default message as merging into this branch.
        #[arg(long = "into-name", value_name = "BRANCH")]
        into_name: Option<String>,
        /// How to clean up the message: strip, whitespace, verbatim, scissors or default.
        #[arg(long, value_name = "MODE",
              value_parser = ["strip", "whitespace", "verbatim", "scissors", "default"])]
        cleanup: Option<String>,
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
        /// Interactive rebase: opens the todo editor (needs a terminal, or
        /// GIT_SEQUENCE_EDITOR set to a script).
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
        /// Edit the todo list of the in-progress rebase (needs a terminal, or
        /// GIT_SEQUENCE_EDITOR set to a script).
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
    /// Reuse recorded conflict resolutions (`git rerere`, in git's
    /// .git/rr-cache): no argument records and replays, or `status`,
    /// `remaining`, `diff`, `forget <paths>`, `clear` or `gc`.
    Rerere {
        /// The subcommand and its paths.
        args: Vec<String>,
        #[command(flatten)]
        rerere: RerereFlags,
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
        #[command(flatten)]
        pathspec_file: PathspecFile,
    },
    /// Cherry-pick commits onto HEAD, or continue/skip/abort a stopped one.
    CherryPick {
        /// Commits or ranges `A..B` to apply in order (prompted for if omitted on a terminal).
        revs: Vec<String>,
        #[command(flatten)]
        sign: SignArgs,
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
        #[command(flatten)]
        more: PickFlags,
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
        #[command(flatten)]
        sign: SignArgs,
        /// Apply the inverse without committing (git's -n/--no-commit).
        #[arg(short = 'n', long = "no-commit")]
        no_commit: bool,
        /// For a merge commit, the parent number (from 1) to revert to.
        #[arg(short = 'm', long = "mainline", value_name = "PARENT")]
        mainline: Option<u32>,
        /// Take this side on conflicting hunks (git's -X).
        #[arg(short = 'X', long = "strategy-option", value_parser = ["ours", "theirs"])]
        strategy_option: Option<String>,
        #[command(flatten)]
        more: PickFlags,
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
    Config(ConfigArgs),
    /// Apply a patch to the working tree, the index, or both.
    Apply(ApplyArgs),
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
        #[arg(required_unless_present = "stdin")]
        name: Option<String>,
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
        /// Write a reflog even for refs outside refs/heads, remotes and notes.
        #[arg(long)]
        create_reflog: bool,
        /// Read `update`, `create`, `delete`, `verify`, `symref-update`,
        /// `symref-create`, `symref-delete`, `symref-verify`, `option
        /// no-deref` and `start`/`prepare`/`commit`/`abort` lines from stdin,
        /// applied all or nothing.
        #[arg(long, conflicts_with_all = ["name", "delete"])]
        stdin: bool,
        /// With --stdin, NUL-separated fields and commands.
        #[arg(short = 'z', requires = "stdin")]
        z: bool,
        /// With --stdin, apply the updates that pass their checks and print
        /// `rejected <ref> <new> <old> <reason>` for the others.
        #[arg(short = '0', long, requires = "stdin")]
        batch_updates: bool,
    },
    /// Print the object id of files or stdin; -w stores them.
    HashObject(HashObjectArgs),
    /// Write commits as mbox patch files (`-<n>`, `<since>` or `<a>..<b>`).
    FormatPatch(Box<FormatPatchArgs>),
    /// Apply mbox patches (from format-patch) as commits.
    Am(AmArgs),
    /// Write a tar or zip of a revision's files.
    Archive(ArchiveArgs),
    /// Pack the object database and prune unreachable objects.
    Gc {
        /// Prune loose objects older than this date (default 2 weeks ago).
        #[arg(long, value_name = "DATE", num_args = 0..=1, require_equals = true,
              default_missing_value = "")]
        prune: Option<String>,
        /// Keep every loose object.
        #[arg(long, conflicts_with = "prune")]
        no_prune: bool,
        /// Repack more thoroughly (slow).
        #[arg(long)]
        aggressive: bool,
        /// Only run when enough loose objects have piled up.
        #[arg(long)]
        auto: bool,
        /// Run even if another gc may be running.
        #[arg(long)]
        force: bool,
        /// Leave the largest pack as it is.
        #[arg(long)]
        keep_largest_pack: bool,
        /// Put unreachable objects in a cruft pack instead of loose files
        /// (the default, per gc.cruftPacks).
        #[arg(long, overrides_with = "no_cruft")]
        cruft: bool,
        /// Loosen unreachable objects instead of a cruft pack.
        #[arg(long)]
        no_cruft: bool,
        /// Print nothing.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Check commits' gpg, x509 or ssh signatures (`git verify-commit`).
    VerifyCommit {
        /// Print each commit's contents too.
        #[arg(short, long)]
        verbose: bool,
        /// Print the verifier's status lines instead of its report.
        #[arg(long)]
        raw: bool,
        /// The commits to check.
        #[arg(required = true)]
        commits: Vec<String>,
    },
    /// Check annotated tags' gpg, x509 or ssh signatures (`git verify-tag`).
    VerifyTag {
        /// Print each tag's contents too.
        #[arg(short, long)]
        verbose: bool,
        /// Print the verifier's status lines instead of its report.
        #[arg(long)]
        raw: bool,
        /// The tags to check.
        #[arg(required = true)]
        tags: Vec<String>,
    },
    /// Check the object database for corruption and dangling objects.
    Fsck {
        /// Also check packed objects (the default).
        #[arg(long)]
        full: bool,
        /// Only check loose objects' contents.
        #[arg(long, conflicts_with = "full")]
        no_full: bool,
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
        /// Write dangling objects to .git/lost-found/{commit,other}.
        #[arg(long)]
        lost_found: bool,
        /// Name each object by how it is reached (e.g. `HEAD~2^{tree}`).
        #[arg(long)]
        name_objects: bool,
        /// Report root commits.
        #[arg(long)]
        root: bool,
        /// Report tags.
        #[arg(long)]
        tags: bool,
        /// Also check the index.
        #[arg(long)]
        cache: bool,
        /// Do not treat reflog entries as reachable.
        #[arg(long)]
        no_reflogs: bool,
        /// Only these objects (default: the refs, index and reflogs).
        objects: Vec<String>,
    },
    /// Pack the repository's objects (`git repack`).
    Repack {
        /// Put everything in one pack.
        #[arg(short = 'a')]
        all: bool,
        /// Like -a, but loosen unreachable objects instead of dropping them.
        #[arg(short = 'A', conflicts_with = "all")]
        all_loosen: bool,
        /// Delete the packs and loose objects made redundant.
        #[arg(short = 'd')]
        delete: bool,
        /// Recompute deltas (git's -f).
        #[arg(short = 'f')]
        no_reuse_delta: bool,
        /// Recompress every object (git's -F).
        #[arg(short = 'F')]
        no_reuse_object: bool,
        /// Only local objects, not those of alternates.
        #[arg(short = 'l', long)]
        local: bool,
        /// Keep unreachable objects in the pack.
        #[arg(short = 'k', long)]
        keep_unreachable: bool,
        /// Write a reachability bitmap (with -a; default
        /// repack.writeBitmaps).
        #[arg(short = 'b', long, overrides_with = "no_write_bitmap_index")]
        write_bitmap_index: bool,
        /// Write no bitmap, whatever repack.writeBitmaps says.
        #[arg(long)]
        no_write_bitmap_index: bool,
        /// Put unreachable objects in a cruft pack.
        #[arg(long)]
        cruft: bool,
        /// Keep a geometric progression of pack sizes with this factor.
        #[arg(short = 'g', long, value_name = "FACTOR")]
        geometric: Option<u32>,
        /// Delta window size.
        #[arg(long, value_name = "N")]
        window: Option<u32>,
        /// Maximum delta depth.
        #[arg(long, value_name = "N")]
        depth: Option<u32>,
        /// Bytes the delta window may hold (k, m and g suffixes).
        #[arg(long, value_name = "SIZE", value_parser = parse_size)]
        window_memory: Option<u64>,
        /// Delta search threads (0: one per CPU).
        #[arg(long, value_name = "N")]
        threads: Option<u32>,
        /// Leave this pack (`pack-<hash>.pack`) as it is; repeatable.
        #[arg(long, value_name = "PACK")]
        keep_pack: Vec<String>,
        /// With --cruft, drop unreachable objects older than this.
        #[arg(long, value_name = "DATE")]
        cruft_expiration: Option<String>,
        /// With -A, only loosen unreachable objects newer than this.
        #[arg(long, value_name = "DATE")]
        unpack_unreachable: Option<String>,
        /// Do not update objects/info/packs.
        #[arg(short = 'n')]
        no_update_server_info: bool,
        /// Print nothing.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Move loose refs into packed-refs (`git pack-refs`).
    PackRefs {
        /// Pack every ref, not only tags and refs already packed.
        #[arg(long)]
        all: bool,
        /// Keep the loose ref files.
        #[arg(long)]
        no_prune: bool,
        /// Only when enough loose refs have piled up.
        #[arg(long)]
        auto: bool,
    },
    /// Run a repository hook as git would (`git hook run`).
    Hook {
        #[command(subcommand)]
        cmd: HookCmd,
    },
    /// Run an rgit command in every repository a multi-valued config key
    /// lists (`git for-each-repo`), e.g. `--config=maintenance.repo`.
    ForEachRepo {
        /// The config key naming the repositories.
        #[arg(long, value_name = "KEY", required = true)]
        config: String,
        /// Go on after a repository fails (exit 1 at the end).
        #[arg(long)]
        keep_going: bool,
        /// The rgit command and its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Write or check the commit-graph file or chain (`git commit-graph`).
    CommitGraph {
        #[command(subcommand)]
        cmd: CommitGraphCmd,
    },
    /// Write, check, expire or repack the multi-pack-index
    /// (`git multi-pack-index`).
    MultiPackIndex {
        #[command(subcommand)]
        cmd: MidxCmd,
    },
    /// Background upkeep (`git maintenance`): run tasks now, schedule them,
    /// or (un)register this repository.
    Maintenance {
        #[command(subcommand)]
        cmd: MaintenanceCmd,
    },
    /// Get, save or drop credentials through the configured helpers (`git
    /// credential`), with git's `key=value` lines on stdin and stdout.
    Credential {
        /// fill, approve, reject or capability.
        action: Option<String>,
    },
    /// Keep credentials in a plain-text file (`git credential-store`), the
    /// `store` credential helper.
    CredentialStore {
        /// The file to use instead of ~/.git-credentials and
        /// $XDG_CONFIG_HOME/git/credentials.
        #[arg(long, value_name = "PATH")]
        file: Option<String>,
        /// get, store or erase.
        action: Option<String>,
    },
    /// Keep credentials in memory for a while (`git credential-cache`), the
    /// `cache` credential helper; a background daemon holds them.
    CredentialCache {
        /// Seconds to keep a credential.
        #[arg(long, value_name = "SECONDS", default_value_t = 900)]
        timeout: u64,
        /// The daemon's socket.
        #[arg(long, value_name = "PATH")]
        socket: Option<String>,
        /// get, store, erase or exit.
        action: Option<String>,
    },
    /// The daemon behind `credential-cache` (`git credential-cache--daemon`).
    #[command(name = "credential-cache--daemon", hide = true)]
    CredentialCacheDaemon {
        /// Stay in the foreground output-wise: keep stderr.
        #[arg(long)]
        debug: bool,
        /// The socket to listen on.
        socket: String,
    },
    /// Large-repository setup and upkeep (git's `scalar`): recommended
    /// config, background maintenance and the registered repo list.
    Scalar {
        #[command(subcommand)]
        cmd: ScalarCmd,
    },
    /// Commits not yet upstream (`git cherry`): `+ <id>` for each, `- <id>`
    /// when upstream already has an equivalent change.
    Cherry {
        /// The branch to compare with (default: the upstream of HEAD).
        upstream: Option<String>,
        /// The commits to look at (default HEAD).
        head: Option<String>,
        /// Leave out the commits up to this one.
        limit: Option<String>,
        /// Add each commit's subject.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Move history as one file (`git bundle`): create, verify, list-heads,
    /// unbundle.
    Bundle {
        #[command(subcommand)]
        cmd: BundleCmd,
    },
    /// Summarize changes for a pull request by mail (`git request-pull`):
    /// what `url` holds beyond `start`.
    RequestPull {
        /// Where the changes start (e.g. origin/main).
        start: String,
        /// The repository to pull from.
        url: String,
        /// What to pull: a branch or tag pushed to `url` (default HEAD);
        /// `local:remote` when the names differ.
        end: Option<String>,
        /// Add the patch after the diffstat.
        #[arg(short = 'p')]
        patch: bool,
    },
    /// Compare two versions of a series (`git range-diff`): `<base> <old>
    /// <new>`, `<old-range> <new-range>` or `<old>...<new>`.
    RangeDiff {
        /// The ranges or revisions.
        #[arg(required = true, num_args = 1..=3)]
        revs: Vec<String>,
        /// Percent of a patch that may change for it still to pair (default 60).
        #[arg(long, value_name = "N", default_value_t = 60)]
        creation_factor: usize,
        /// Only list the pairs, without their diffs.
        #[arg(short = 's', long)]
        no_patch: bool,
        /// Only the commits of the first range (and pairs).
        #[arg(long, conflicts_with = "right_only")]
        left_only: bool,
        /// Only the commits of the second range (and pairs).
        #[arg(long)]
        right_only: bool,
        /// Accepted for git compatibility (rgit prints no color here).
        #[arg(long, hide = true)]
        no_dual_color: bool,
        /// Lines of context in the diffs between pairs (default 3).
        #[arg(short = 'U', long = "unified", value_name = "N")]
        unified: Option<u32>,
        /// Include these notes refs' notes (bare: the default notes too).
        #[arg(long, value_name = "REF", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        notes: Vec<String>,
        /// Leave notes out of the compared patches.
        #[arg(long)]
        no_notes: bool,
        /// Only commits touching these paths (after `--`).
        #[arg(last = true)]
        paths: Vec<String>,
    },
    /// Show changes in the configured diff tool (`git difftool`): diff.tool,
    /// difftool.<tool>.cmd or a known tool (vimdiff, meld, code, ...).
    Difftool(ToolArgs),
    /// Resolve conflicts in the configured merge tool (`git mergetool`):
    /// merge.tool, mergetool.<tool>.cmd or a known tool.
    Mergetool(ToolArgs),
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
        /// Remove untracked folders too, not only files.
        #[arg(short = 'd')]
        dirs: bool,
        /// Really remove (needed while clean.requireForce is true); twice
        /// also removes nested repositories.
        #[arg(short = 'f', long, action = clap::ArgAction::Count)]
        force: u8,
        /// Pick what to remove from git's menu, read from stdin.
        #[arg(short = 'i', long)]
        interactive: bool,
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
        #[command(flatten)]
        pathspec_file: PathspecFile,
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
        /// Also update paths outside the sparse checkout.
        #[arg(long)]
        sparse: bool,
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
        /// Also update paths outside the sparse checkout.
        #[arg(long)]
        sparse: bool,
    },
    /// Describe a revision relative to the nearest tag (default HEAD).
    Describe {
        /// The revision to describe (defaults to HEAD).
        rev: Option<String>,
        /// Use lightweight tags too, not just annotated ones (git's --tags).
        #[arg(long)]
        tags: bool,
        /// Use any ref: branches as `heads/main`, tags as `tags/v1`.
        #[arg(long)]
        all: bool,
        /// Append this mark (default -dirty) when tracked files have changes.
        #[arg(
            long,
            value_name = "MARK",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "-dirty"
        )]
        dirty: Option<String>,
        /// Like --dirty, but append this mark (default -broken) when the
        /// working tree cannot be read.
        #[arg(
            long,
            value_name = "MARK",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "-broken"
        )]
        broken: Option<String>,
        /// Always use the long format (tag-count-oid), even on a tag.
        #[arg(long)]
        long: bool,
        /// Number of hex digits for the abbreviated commit oid (0: the tag only).
        #[arg(long, value_name = "N")]
        abbrev: Option<u32>,
        /// Show the abbreviated commit oid when no tag is found.
        #[arg(long)]
        always: bool,
        /// Follow only the first parent of merges.
        #[arg(long)]
        first_parent: bool,
        /// Consider this many tags (default 10; 0 is --exact-match).
        #[arg(long, value_name = "N")]
        candidates: Option<u32>,
        /// Only consider tags matching this glob (repeatable).
        #[arg(long = "match", value_name = "GLOB")]
        pattern: Vec<String>,
        /// Leave out tags matching this glob (repeatable).
        #[arg(long, value_name = "GLOB")]
        exclude: Vec<String>,
        /// Print the tag only when it points at the revision itself; else fail.
        #[arg(long)]
        exact_match: bool,
        /// Name the revision after the oldest tag that contains it, as
        /// `v1~2` or `v1~1^2` (git's --contains).
        #[arg(long)]
        contains: bool,
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
        /// Like --reference, but go on without it when REPO is not a repository.
        #[arg(long, value_name = "REPO")]
        reference_if_able: Vec<String>,
        /// Accepted for git compatibility; the clone fetches everything itself.
        #[arg(long, value_name = "URI")]
        bundle_uri: Option<String>,
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
    /// Run one MCP tool with its arguments as a JSON object on stdin and print
    /// its TOON result, as the pi and omp extension does.
    Tool {
        /// The tool, e.g. git_status.
        name: String,
    },
    /// Set agent apps (Claude Code, Codex, OpenCode, pi, omp) up for rgit.
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
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
        /// Tag each file with its status: H cached, S skip-worktree, M
        /// unmerged, R removed, C changed, ? other.
        #[arg(short = 't')]
        tags: bool,
        /// Like -t, with lowercase tags for assume-unchanged files.
        #[arg(short = 'v')]
        valid_tags: bool,
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
        /// `%(objectsize)`, `%(objectsize:disk)`, `%(deltabase)`, `%(rest)`).
        #[arg(long, value_name = "FORMAT", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        batch: Option<String>,
        /// Like --batch, without the content.
        #[arg(long = "batch-check", value_name = "FORMAT", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        batch_check: Option<String>,
        /// With --batch or --batch-check, answer for every object instead of stdin.
        #[arg(long = "batch-all-objects")]
        batch_all_objects: bool,
        /// Read `contents <object>`, `info <object>` and (with --buffer)
        /// `flush` commands from stdin, answering as --batch / --batch-check.
        #[arg(long = "batch-command", value_name = "FORMAT", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        batch_command: Option<String>,
        /// With --batch or --batch-check, do not flush after each object.
        #[arg(long)]
        buffer: bool,
        /// With a batch mode, NUL-separate the input and output.
        #[arg(short = 'Z')]
        nul: bool,
        /// With a batch mode, NUL-separate the input.
        #[arg(short = 'z')]
        nul_input: bool,
        /// With a batch mode, follow symlinks within the tree for `<tree>:<path>`.
        #[arg(long)]
        follow_symlinks: bool,
        /// Show a blob through its diff driver's textconv command.
        #[arg(long, conflicts_with = "filters")]
        textconv: bool,
        /// Show a blob as it would be checked out (smudge and eol filters).
        #[arg(long)]
        filters: bool,
        /// The path whose attributes pick the --textconv / --filters driver.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
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
        /// `%(objectsize)`, `%(tree)`, `%(parent)`, `%(describe[:tags,abbrev=N,match=P])`,
        /// `%(ahead-behind:<ref>)`, `%(*subject)` and every other field with `*`
        /// for what an annotated tag points at,
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
        /// Abbreviate commit ids.
        #[arg(long = "abbrev-commit")]
        abbrev_commit: bool,
        /// Only commits at or after this date (`2024-01-05`, `2 weeks ago`...).
        #[arg(long, visible_alias = "after", visible_alias = "max-age")]
        since: Option<String>,
        /// Only commits at or before this date.
        #[arg(long, visible_alias = "before", visible_alias = "min-age")]
        until: Option<String>,
        /// Keep only commits whose author matches this regex.
        #[arg(long)]
        author: Option<String>,
        /// Keep only commits whose committer matches this regex.
        #[arg(long)]
        committer: Option<String>,
        /// Keep only commits whose message matches this regex (repeat for any of several).
        #[arg(long, value_name = "REGEX")]
        grep: Vec<String>,
        /// Match --grep, --author and --committer case-insensitively.
        #[arg(short = 'i', long = "regexp-ignore-case")]
        ignore_case: bool,
        /// After the commits, the trees and blobs they need, with their paths.
        #[arg(long)]
        objects: bool,
        /// --objects, first naming the excluded commits the range starts from (`-<id>`).
        #[arg(long)]
        objects_edge: bool,
        /// What to do about missing objects: error, allow-any, allow-promisor or print.
        #[arg(long, value_name = "ACTION")]
        missing: Option<String>,
        #[command(flatten)]
        walk: WalkArgs,
        #[command(flatten)]
        more: Box<RevListArgs>,
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
    /// Show where a ref pointed over time (`git reflog [show] [REF]`), or
    /// `expire [--expire=<date>] [--expire-unreachable=<date>] [--all]
    /// [--rewrite] [--updateref] [--stale-fix] [-n] [--verbose] [REF...]`,
    /// `delete [--rewrite] [--updateref] [-n] REF@{N}...`, `exists REF`.
    Reflog {
        /// Show at most N entries.
        #[arg(short = 'n', long = "max-count", value_name = "N")]
        max_count: Option<usize>,
        /// `show` (optional) and the ref (default HEAD); or expire, delete or
        /// exists and their arguments.
        #[arg(
            value_name = "[show] REF",
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
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
        /// Patterns are Perl-compatible regexes, with lookaround.
        #[arg(short = 'P', long = "perl-regexp")]
        perl: bool,
        /// A pattern; repeat to match any of several. Combine them with
        /// `--and`, `--or`, `--not` and `(` `)` as git does.
        #[arg(short = 'e', value_name = "PATTERN", allow_hyphen_values = true)]
        patterns: Vec<String>,
        /// Keep only files that match every pattern (every `--or` branch).
        #[arg(long)]
        all_match: bool,
        /// Show the line naming the function around each match.
        #[arg(short = 'p', long)]
        show_function: bool,
        /// Show the whole function around each match.
        #[arg(short = 'W', long)]
        function_context: bool,
        /// Search the index instead of the working tree.
        #[arg(long)]
        cached: bool,
        /// Search untracked files too.
        #[arg(long)]
        untracked: bool,
        /// Search every file under the current folder, tracked or not
        /// (works outside a repository).
        #[arg(long)]
        no_index: bool,
        /// Leave out ignored files with --no-index.
        #[arg(long)]
        exclude_standard: bool,
        /// Search ignored files too with --untracked.
        #[arg(long)]
        no_exclude_standard: bool,
        /// Search the checked-out submodules too.
        #[arg(long)]
        recurse_submodules: bool,
        /// Accepted for git compatibility; the search always runs in parallel.
        #[arg(long, value_name = "N")]
        threads: Option<usize>,
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
    /// Clean up text read from stdin as git cleans a commit message, like
    /// `git stripspace`.
    Stripspace {
        /// Drop lines starting with the comment character too.
        #[arg(short = 's', long = "strip-comments", conflicts_with = "comment_lines")]
        strip_comments: bool,
        /// Prefix each line with the comment character instead.
        #[arg(short = 'c', long = "comment-lines")]
        comment_lines: bool,
        #[arg(skip)]
        input: Option<String>,
    },
    /// Lay out lines read from stdin in columns, like `git column`.
    Column {
        /// Read column.<name> as well as column.ui.
        #[arg(long, value_name = "NAME")]
        command: Option<String>,
        /// The layout: `column`, `row` or `plain`, with `dense`/`nodense`
        /// and `always`/`never`/`auto`.
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,
        /// The layout as git's option bits.
        #[arg(long = "raw-mode", value_name = "N")]
        raw_mode: Option<u32>,
        /// The maximum width (default: the terminal's, less one).
        #[arg(long, value_name = "N")]
        width: Option<usize>,
        /// Text before each row.
        #[arg(long, value_name = "STRING")]
        indent: Option<String>,
        /// Text after each row (default a newline).
        #[arg(long, value_name = "STRING")]
        nl: Option<String>,
        /// Spaces between columns.
        #[arg(long, value_name = "N", default_value_t = 1)]
        padding: usize,
        #[arg(skip)]
        input: Option<String>,
    },
    /// Check that a ref name is valid, like `git check-ref-format`; exit 1
    /// if not.
    CheckRefFormat {
        /// Print the name with repeated and leading slashes removed.
        #[arg(long, visible_alias = "print")]
        normalize: bool,
        /// Accept a name with one level (no slash).
        #[arg(long = "allow-onelevel", overrides_with = "no_allow_onelevel")]
        allow_onelevel: bool,
        /// Require two levels (the default).
        #[arg(long = "no-allow-onelevel")]
        no_allow_onelevel: bool,
        /// Accept one `*`, as in a refspec.
        #[arg(long = "refspec-pattern")]
        refspec_pattern: bool,
        /// Check a branch name (expanding `@{-N}`) and print it.
        #[arg(long)]
        branch: bool,
        /// The ref name.
        name: String,
    },
    /// Print the patch id of each patch read from stdin (`git log -p`,
    /// `format-patch` or `diff` output), like `git patch-id`.
    PatchId {
        /// Sum per-file hashes so file order does not matter.
        #[arg(long, overrides_with = "unstable")]
        stable: bool,
        /// Hash the patch as one stream (the default).
        #[arg(long)]
        unstable: bool,
        /// Keep whitespace in the hash (implies --stable).
        #[arg(long)]
        verbatim: bool,
        #[arg(skip)]
        input: Option<String>,
    },
    /// Name commits by the refs that reach them (`main~2`, `tags/v1^0`), like
    /// `git name-rev`.
    NameRev {
        /// Print only the names.
        #[arg(long = "name-only")]
        name_only: bool,
        /// Use only tags.
        #[arg(long)]
        tags: bool,
        /// Use only refs matching this pattern (repeatable).
        #[arg(long, value_name = "PATTERN")]
        refs: Vec<String>,
        /// Skip refs matching this pattern (repeatable).
        #[arg(long, value_name = "PATTERN")]
        exclude: Vec<String>,
        /// Name every commit reachable from a ref.
        #[arg(long)]
        all: bool,
        /// Append names to the object ids in the text read from stdin.
        #[arg(long = "annotate-stdin", visible_alias = "stdin")]
        annotate_stdin: bool,
        /// Fail on an unnamed commit instead of printing `undefined`.
        #[arg(long = "no-undefined")]
        no_undefined: bool,
        /// Print an abbreviated id for an unnamed commit.
        #[arg(long)]
        always: bool,
        /// Name tags by the commit they point at.
        #[arg(long = "peel-tag")]
        peel_tag: bool,
        /// The commits to name.
        revs: Vec<String>,
        #[arg(skip)]
        input: Option<String>,
    },
    /// Print the gitattributes of paths, like `git check-attr`: `ATTR PATH...`,
    /// `ATTR... -- PATH...` or `-a PATH...`.
    CheckAttr {
        /// Every attribute that is set, unset or has a value.
        #[arg(short = 'a', long)]
        all: bool,
        /// Read .gitattributes from the index only.
        #[arg(long)]
        cached: bool,
        /// Read the paths from stdin, one per line.
        #[arg(long)]
        stdin: bool,
        /// NUL-separated input and output.
        #[arg(short = 'z')]
        z: bool,
        /// Attributes, then paths.
        #[arg(value_name = "ATTR_OR_PATH")]
        items: Vec<String>,
        /// The paths, after `--`.
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Compare two trees, or a commit with its parent, like `git diff-tree`.
    DiffTree {
        #[command(flatten)]
        format: RawDiffArgs,
        /// Show a root commit's files as added.
        #[arg(long)]
        root: bool,
        /// Do not print the commit id before its changes.
        #[arg(long = "no-commit-id")]
        no_commit_id: bool,
        /// Read `<commit> [<parent>]` or `<tree> <tree>` lines from stdin.
        #[arg(long)]
        stdin: bool,
        /// Show a merge's combined diff against all its parents.
        #[arg(short = 'c')]
        combined: bool,
        /// Show a merge's dense combined diff (a patch unless another format
        /// is asked for).
        #[arg(long = "cc")]
        dense_combined: bool,
        /// One commit or two tree-ishes, then paths.
        #[arg(value_name = "TREE-ISH")]
        args: Vec<String>,
        /// Limit to these paths.
        #[arg(last = true)]
        paths: Vec<String>,
    },
    /// Compare a tree with the working tree or the index, like `git diff-index`.
    DiffIndex {
        #[command(flatten)]
        format: RawDiffArgs,
        /// Compare with the index, not the working tree.
        #[arg(long)]
        cached: bool,
        /// The tree-ish, then paths.
        #[arg(value_name = "TREE-ISH", required = true)]
        args: Vec<String>,
        /// Limit to these paths.
        #[arg(last = true)]
        paths: Vec<String>,
    },
    /// Compare the index with the working tree, like `git diff-files`.
    DiffFiles {
        #[command(flatten)]
        format: RawDiffArgs,
        /// Limit to these paths.
        paths: Vec<String>,
    },
    /// Merge two commits without touching the index or working tree, like
    /// `git merge-tree --write-tree`: prints the merged tree, conflicted
    /// files and messages; exits 1 on conflicts. With three trees (base,
    /// ours, theirs) it is git's old trivial merge report.
    MergeTree {
        /// A real merge of two commits (the default with two arguments).
        #[arg(long = "write-tree", conflicts_with = "trivial_merge")]
        write_tree: bool,
        /// The old trivial merge of three trees (the default with three).
        #[arg(long = "trivial-merge")]
        trivial_merge: bool,
        /// Print nothing; only the exit status tells whether it is clean.
        #[arg(long)]
        quiet: bool,
        /// Merge each `[<base> -- ]<branch1> <branch2>` line of stdin.
        #[arg(long)]
        stdin: bool,
        /// A merge strategy option (ours, theirs, no-renames,
        /// find-renames[=<n>], ignore-space-change, ...).
        #[arg(short = 'X', long = "strategy-option", value_name = "OPTION")]
        xopts: Vec<String>,
        /// List only the names of conflicted files.
        #[arg(long = "name-only")]
        name_only: bool,
        /// Print the informational messages even on a clean merge.
        #[arg(long, overrides_with = "no_messages")]
        messages: bool,
        /// Do not print the informational messages.
        #[arg(long = "no-messages")]
        no_messages: bool,
        /// NUL-terminated output.
        #[arg(short = 'z')]
        z: bool,
        /// Merge even without a common ancestor.
        #[arg(long = "allow-unrelated-histories")]
        allow_unrelated_histories: bool,
        /// Use this commit or tree as the merge base.
        #[arg(long = "merge-base", value_name = "TREE-ISH")]
        merge_base: Option<String>,
        /// `<branch1> <branch2>`, or `<base> <branch1> <branch2>`.
        #[arg(value_name = "COMMIT")]
        args: Vec<String>,
    },
    /// Print history as a fast-import stream, like `git fast-export`:
    /// revisions and ranges (`--all`, `A..B`, `^A`), `--signed-tags=`,
    /// `--signed-commits=`, `--tag-of-filtered-object=`, `--reencode=`,
    /// `--export-marks=`, `--import-marks[-if-exists]=`, `--no-data`,
    /// `--full-tree`, `--use-done-feature`, `--fake-missing-tagger`,
    /// `--refspec`, `--reference-excluded-parents`, `--show-original-ids`,
    /// `--mark-tags`, then `-- <paths>`.
    FastExport {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
        args: Vec<String>,
    },
    /// Read a fast-import stream on stdin and write its objects and refs,
    /// like `git fast-import` (`--quiet`, `--force`, `--date-format=`,
    /// `--export-marks=`, `--import-marks[-if-exists]=`, `--done`).
    FastImport {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
        args: Vec<String>,
    },
    /// Replay commits onto a new base without touching the working tree,
    /// like `git replay`: prints `update <ref> <new> <old>` lines for
    /// `rgit update-ref --stdin`; exits 1 on a conflict.
    Replay {
        /// Replay onto this commit, updating the branches given.
        #[arg(long, value_name = "REVISION")]
        onto: Option<String>,
        /// Replay onto this branch and advance it.
        #[arg(long, value_name = "BRANCH")]
        advance: Option<String>,
        /// Update every branch inside the replayed range.
        #[arg(long)]
        contained: bool,
        /// The commits to replay (`base..branch`).
        #[arg(value_name = "REVISION-RANGE", allow_hyphen_values = true)]
        revs: Vec<String>,
    },
    /// Three-way merge of files into the first, like `git merge-file`; exits
    /// with the number of conflicts.
    MergeFile {
        /// Labels for current, base and other (up to three -L).
        #[arg(short = 'L', value_name = "LABEL", action = clap::ArgAction::Append)]
        labels: Vec<String>,
        /// Print the result instead of writing the current file.
        #[arg(short = 'p', long)]
        stdout: bool,
        /// Resolve conflicts with our side.
        #[arg(long, conflicts_with_all = ["theirs", "union"])]
        ours: bool,
        /// Resolve conflicts with their side.
        #[arg(long, conflicts_with = "union")]
        theirs: bool,
        /// Keep both sides of conflicts.
        #[arg(long)]
        union: bool,
        /// Show the base version in conflicts.
        #[arg(long, conflicts_with = "zdiff3")]
        diff3: bool,
        /// Like --diff3, with common lines moved out of the conflict.
        #[arg(long)]
        zdiff3: bool,
        /// Conflict marker length.
        #[arg(long = "marker-size", value_name = "N")]
        marker_size: Option<u16>,
        /// Do not warn about conflicts.
        #[arg(short = 'q', long)]
        quiet: bool,
        /// The arguments are blob ids; write the result as a blob and print its id.
        #[arg(long = "object-id")]
        object_id: bool,
        current: String,
        base: String,
        other: String,
    },
    /// Write a commit object for a tree and print its id, like `git
    /// commit-tree`; the message comes from -m, -F or stdin.
    CommitTree {
        /// A parent commit (repeat for a merge).
        #[arg(short = 'p', value_name = "PARENT")]
        parents: Vec<String>,
        /// A message paragraph (repeatable).
        #[arg(short = 'm', value_name = "MESSAGE")]
        message: Vec<String>,
        /// Read the message from a file (`-` is stdin).
        #[arg(short = 'F', value_name = "FILE")]
        file: Vec<String>,
        /// The tree (e.g. `HEAD^{tree}`).
        tree: String,
    },
    /// Write the index as a tree and print its id, like `git write-tree`.
    WriteTree {
        /// Allow entries whose objects are missing.
        #[arg(long = "missing-ok")]
        missing_ok: bool,
        /// Write only the subtree at this folder.
        #[arg(long, value_name = "PREFIX")]
        prefix: Option<String>,
    },
    /// Read trees into the index, like `git read-tree`: one tree replaces it,
    /// -m merges one, two (switch) or three (base, ours, theirs) trees; with
    /// more, all but the last two are merge bases.
    ReadTree {
        /// Merge the trees into the index instead of replacing it.
        #[arg(short = 'm')]
        merge: bool,
        /// Like -m, but drop unmerged entries and ignore local changes.
        #[arg(long)]
        reset: bool,
        /// Update the working tree files to match (with -m, --reset or --prefix).
        #[arg(short = 'u')]
        update: bool,
        /// Merge in the index only, without checking the working tree.
        #[arg(short = 'i')]
        index_only: bool,
        /// Check the merge without writing the index.
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Also resolve removals and identical additions in a 3-way merge.
        #[arg(long)]
        aggressive: bool,
        /// Fail if the 3-way merge needs a file-level merge.
        #[arg(long)]
        trivial: bool,
        /// Write the resulting index to this file instead.
        #[arg(long = "index-output", value_name = "FILE")]
        index_output: Option<PathBuf>,
        /// Accepted for git compatibility (rgit applies no sparse checkout).
        #[arg(long = "no-sparse-checkout")]
        no_sparse_checkout: bool,
        /// Read the tree under this folder of the index.
        #[arg(long, value_name = "PREFIX")]
        prefix: Option<String>,
        /// Empty the index.
        #[arg(long)]
        empty: bool,
        /// Accepted for git compatibility.
        #[arg(short = 'v', hide = true)]
        verbose: bool,
        /// The trees (commits or tree ids).
        trees: Vec<String>,
    },
    /// Change index entries, like `git update-index`: options apply to the
    /// paths after them (`--add`, `--remove`, `--force-remove`, `--cacheinfo
    /// <mode>,<sha1>,<path>`, `--chmod=(+|-)x`, `--[no-]assume-unchanged`,
    /// `--[no-]skip-worktree`, `--info-only`, `--refresh`, `--really-refresh`,
    /// `-q`, `--unmerged`, `--ignore-missing`, `--verbose`, and last
    /// `--index-info` or `--stdin`, with `-z`).
    UpdateIndex {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
        args: Vec<String>,
    },
    /// Check out only some folders, like `git sparse-checkout`: `set`, `add`,
    /// `list`, `init`, `reapply`, `disable` and `check-rules`, with `--cone`,
    /// `--no-cone`, `--[no-]sparse-index`, `--skip-checks`, `--stdin`, and
    /// check-rules' `-z` and `--rules-file <file>`.
    SparseCheckout {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
        args: Vec<String>,
    },
    /// Copy files from the index to the working tree, like `git
    /// checkout-index`.
    CheckoutIndex {
        /// Every file in the index.
        #[arg(short = 'a', long)]
        all: bool,
        /// Overwrite existing files.
        #[arg(short = 'f', long)]
        force: bool,
        /// Update the index's stat data for the written files.
        #[arg(short = 'u', long = "index")]
        index: bool,
        /// Say nothing about existing or unknown files.
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Only update files that exist.
        #[arg(short = 'n', long = "no-create")]
        no_create: bool,
        /// Prepend this to each path written: a folder when it ends in `/`,
        /// else a file name prefix.
        #[arg(long, value_name = "PREFIX")]
        prefix: Option<String>,
        /// Write the entries of this stage (1, 2, 3, or all with --temp).
        #[arg(long, value_name = "N", value_parser = ["1", "2", "3", "all"])]
        stage: Option<String>,
        /// Write temporary files and print their names with each path.
        #[arg(long)]
        temp: bool,
        /// Read the paths from stdin.
        #[arg(long)]
        stdin: bool,
        /// NUL-separated paths with --stdin, and NUL-terminated --temp lines.
        #[arg(short = 'z')]
        z: bool,
        /// The files to write.
        paths: Vec<String>,
    },
    /// Build a tree from `ls-tree` lines on stdin and print its id, like `git
    /// mktree`.
    Mktree {
        /// NUL-terminated lines.
        #[arg(short = 'z')]
        z: bool,
        /// Allow objects that are missing.
        #[arg(long)]
        missing: bool,
        /// Build a tree per blank-line-separated group.
        #[arg(long)]
        batch: bool,
    },
    /// Check a tag object on stdin and store it, like `git mktag`.
    Mktag {
        /// Allow what git's fsck only warns about (a missing tagger, a bad
        /// tag name).
        #[arg(long = "no-strict")]
        no_strict: bool,
        /// The default: refuse any fsck problem.
        #[arg(long, hide = true)]
        strict: bool,
    },
    /// Print the commit id `git archive` stored in the tar read from stdin,
    /// like `git get-tar-commit-id`; exit 1 when it has none.
    GetTarCommitId,
    /// Write a merge commit's message from FETCH_HEAD-style lines on stdin,
    /// like `git fmt-merge-msg`.
    FmtMergeMsg {
        /// Start with this text instead of the "Merge ..." title.
        #[arg(short = 'm', long, value_name = "TEXT")]
        message: Option<String>,
        /// Add a shortlog of at most N commits per merged head (default 20).
        #[arg(
            long,
            visible_alias = "summary",
            value_name = "N",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "20"
        )]
        log: Option<usize>,
        /// No shortlog, whatever merge.log says.
        #[arg(long = "no-log", visible_alias = "no-summary")]
        no_log: bool,
        /// Name this branch as the one merged into.
        #[arg(long = "into-name", value_name = "NAME")]
        into_name: Option<String>,
        /// Read the lines from this file instead of stdin.
        #[arg(short = 'F', long, value_name = "FILE")]
        file: Option<String>,
        #[arg(skip)]
        input: Option<String>,
    },
    /// Split mboxes or Maildirs into one numbered file per mail, like `git
    /// mailsplit`; prints the number of the last mail.
    Mailsplit {
        /// The folder to write the mails to.
        #[arg(short = 'o', value_name = "DIR", required = true)]
        dir: String,
        /// Take a file that does not start with a `From ` line as one mail.
        #[arg(short = 'b')]
        bare: bool,
        /// Number the mails after N (default 0).
        #[arg(short = 'f', value_name = "N", default_value_t = 0)]
        start: usize,
        /// Digits in the file names (default 4).
        #[arg(short = 'd', value_name = "N", default_value_t = 4, value_parser = clap::value_parser!(u8).range(3..10))]
        prec: u8,
        /// Keep CRLF line endings.
        #[arg(long = "keep-cr")]
        keep_cr: bool,
        /// Unescape `>From ` lines, as in an mboxrd.
        #[arg(long)]
        mboxrd: bool,
        /// The mboxes or Maildirs (default: stdin).
        mboxes: Vec<String>,
    },
    /// Read one mail from stdin, write its message and patch to MSG and
    /// PATCH, and print its author, subject and date, like `git mailinfo`.
    Mailinfo {
        /// Keep the subject as it is.
        #[arg(short = 'k')]
        keep_subject: bool,
        /// Drop only `[PATCH ...]` brackets from the subject.
        #[arg(short = 'b')]
        keep_non_patch: bool,
        /// Reencode to UTF-8 (the default).
        #[arg(short = 'u')]
        utf8: bool,
        /// Add the Message-ID header to the message.
        #[arg(short = 'm', long = "message-id")]
        message_id: bool,
        /// Drop everything above a scissors line (`-- >8 --`).
        #[arg(long, overrides_with = "no_scissors")]
        scissors: bool,
        /// Ignore scissors lines (overrides mailinfo.scissors).
        #[arg(long = "no-scissors")]
        no_scissors: bool,
        /// Where to write the message.
        msg: String,
        /// Where to write the patch.
        patch: String,
        #[arg(skip)]
        input: Option<String>,
    },
    /// Add or parse trailers (`Signed-off-by: ...`) in a commit message read
    /// from files or stdin, like `git interpret-trailers`. Takes git's
    /// options in order: `--trailer <key>[(=|:)<value>]`, `--where`,
    /// `--if-exists`, `--if-missing` (and their `--no-` forms, applying to
    /// the trailers after them), `--in-place`, `--trim-empty`,
    /// `--only-trailers`, `--only-input`, `--unfold`, `--parse`,
    /// `--no-divider`, then the files.
    InterpretTrailers {
        /// Options and files, as for git.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
        args: Vec<String>,
        #[arg(skip)]
        input: Option<String>,
    },
    /// Show branches side by side with the commits each has, down to where
    /// they meet, like `git show-branch`.
    ShowBranch {
        /// Show local and remote-tracking branches.
        #[arg(short = 'a', long)]
        all: bool,
        /// Show remote-tracking branches.
        #[arg(short = 'r', long)]
        remotes: bool,
        /// Add the current branch if it is not listed.
        #[arg(long)]
        current: bool,
        /// Parents after all their children (the default).
        #[arg(long = "topo-order")]
        topo_order: bool,
        /// Newest first, parents still after their children.
        #[arg(long = "date-order")]
        date_order: bool,
        /// Keep merges reachable from only one branch.
        #[arg(long)]
        sparse: bool,
        /// Show N more commits past the common ancestor.
        #[arg(
            long,
            value_name = "N",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "1"
        )]
        more: Option<i32>,
        /// List only the branches and their tips.
        #[arg(long)]
        list: bool,
        /// Print the merge bases of the branches.
        #[arg(long = "merge-base")]
        merge_base: bool,
        /// Print the tips not reachable from any other.
        #[arg(long)]
        independent: bool,
        /// Omit the names.
        #[arg(long = "no-name")]
        no_name: bool,
        /// Name commits by abbreviated id instead of `branch~n`.
        #[arg(long = "sha1-name")]
        sha1_name: bool,
        /// Only commits not on the first branch.
        #[arg(long)]
        topics: bool,
        /// Show a branch's last N reflog entries (default 4), from BASE.
        #[arg(
            short = 'g',
            long,
            value_name = "N[,BASE]",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = ""
        )]
        reflog: Option<String>,
        /// Color the columns: always, never or auto.
        #[arg(
            long,
            value_name = "WHEN",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "always"
        )]
        color: Option<String>,
        /// Branches, revisions or ref globs (default: every local branch).
        revs: Vec<String>,
    },
    /// Diff the pairs of git's raw diff format on stdin (NUL-separated, as
    /// `diff-tree -r -z` prints them), like `git diff-pairs -z`; an empty
    /// record ends a batch.
    DiffPairs {
        /// Records are NUL-separated (required, as in git).
        #[arg(short = 'z')]
        z: bool,
        /// Print patches (the default).
        #[arg(short, long)]
        patch: bool,
        /// Print nothing.
        #[arg(short = 's', long)]
        no_patch: bool,
        /// Print a diffstat.
        #[arg(long)]
        stat: bool,
        /// Only the diffstat's summary line.
        #[arg(long)]
        shortstat: bool,
        /// Added and deleted line counts per file.
        #[arg(long)]
        numstat: bool,
        /// Only the changed paths.
        #[arg(long)]
        name_only: bool,
        /// The changed paths with their status letters.
        #[arg(long)]
        name_status: bool,
        /// Lines of context around each change.
        #[arg(short = 'U', long = "unified", value_name = "N")]
        unified: Option<u32>,
    },
    /// Fetch refs' objects from another repository (a path or file:// URL)
    /// without updating any ref, printing `<id> <ref>` for each, like
    /// `git fetch-pack`.
    FetchPack {
        /// Fetch every ref the remote has.
        #[arg(long)]
        all: bool,
        /// Read more ref names from stdin, one per line.
        #[arg(long)]
        stdin: bool,
        /// Deepen no further than N commits (a shallow fetch).
        #[arg(long, value_name = "N")]
        depth: Option<i32>,
        /// Quiet; accepted for git compatibility.
        #[arg(short, long)]
        quiet: bool,
        /// Keep the pack; accepted for git compatibility.
        #[arg(short, long)]
        keep: bool,
        /// Accepted for git compatibility.
        #[arg(long)]
        thin: bool,
        /// Accepted for git compatibility.
        #[arg(long)]
        include_tag: bool,
        /// Accepted for git compatibility.
        #[arg(long)]
        no_progress: bool,
        /// Accepted for git compatibility.
        #[arg(short, long)]
        verbose: bool,
        /// Accepted for git compatibility.
        #[arg(long, value_name = "PROGRAM")]
        upload_pack: Option<String>,
        /// The repository to fetch from.
        repository: String,
        /// Full ref names to fetch (`refs/heads/main`, `HEAD`).
        refs: Vec<String>,
    },
    /// Push refs to another repository (a path or file:// URL) with no remote
    /// config, like `git send-pack`; the report goes to stderr.
    SendPack {
        /// Push every branch.
        #[arg(long)]
        all: bool,
        /// Make every remote ref match a local one, deleting the rest.
        #[arg(long)]
        mirror: bool,
        /// Report what would be pushed without pushing.
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Update refs even when that loses commits.
        #[arg(short, long)]
        force: bool,
        /// Update every ref or none.
        #[arg(long)]
        atomic: bool,
        /// List up-to-date refs too.
        #[arg(short, long)]
        verbose: bool,
        /// Quiet; accepted for git compatibility.
        #[arg(short, long)]
        quiet: bool,
        /// Accepted for git compatibility.
        #[arg(long)]
        thin: bool,
        /// Accepted for git compatibility.
        #[arg(long, visible_alias = "exec", value_name = "PROGRAM")]
        receive_pack: Option<String>,
        /// The repository to push to.
        repository: String,
        /// Refspecs: `main`, `src:dst`, `+src:dst`, `:dst` (delete).
        refs: Vec<String>,
    },
}

/// Output options shared by `diff-tree`, `diff-index` and `diff-files`.
#[derive(clap::Args, Clone, Default)]
pub struct RawDiffArgs {
    /// Show patches instead of the raw listing.
    #[arg(short = 'p', short_alias = 'u', long = "patch")]
    pub patch: bool,
    /// Show no diff (only diff-tree's commit id).
    #[arg(short = 's', long = "no-patch")]
    pub no_patch: bool,
    /// Show the raw listing (the default).
    #[arg(long)]
    pub raw: bool,
    /// Show only the names of changed files.
    #[arg(long = "name-only")]
    pub name_only: bool,
    /// Show the names and status letters of changed files.
    #[arg(long = "name-status")]
    pub name_status: bool,
    /// NUL-terminated output.
    #[arg(short = 'z')]
    pub z: bool,
    /// Recurse into subtrees.
    #[arg(short = 'r')]
    pub recursive: bool,
    /// Show changed trees too while recursing (implies -r).
    #[arg(short = 't')]
    pub trees: bool,
    /// Exit 1 when there are changes.
    #[arg(long = "exit-code")]
    pub exit_code: bool,
    /// Print nothing; exit 1 when there are changes.
    #[arg(long)]
    pub quiet: bool,
    /// Show a diffstat, optionally this many columns wide.
    #[arg(long, value_name = "WIDTH", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    pub stat: Option<String>,
    /// Show created, deleted, renamed and mode-changed files.
    #[arg(long)]
    pub summary: bool,
    /// Detect renames, optionally at this similarity (git's -M[<n>]).
    #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    pub find_renames: Option<String>,
    /// Detect copies as well as renames (git's -C[<n>]); twice is
    /// --find-copies-harder.
    #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    pub find_copies: Vec<String>,
    /// Look for copy sources among unmodified files too.
    #[arg(long)]
    pub find_copies_harder: bool,
    /// Abbreviate raw object ids, to this many digits if given.
    #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "0")]
    pub abbrev: Option<usize>,
}

#[derive(Subcommand)]
pub enum HookCmd {
    /// Run the hook `name` from the hooks directory (`core.hooksPath`) with
    /// the arguments after `--`; exit with its status.
    Run {
        /// Succeed quietly when there is no such hook.
        #[arg(long)]
        ignore_missing: bool,
        /// Feed the hook this file on stdin.
        #[arg(long, value_name = "PATH")]
        to_stdin: Option<String>,
        /// The hook, e.g. pre-commit.
        name: String,
        /// Arguments for the hook.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// What agent session hooks run: the repository state as TOON.
    #[command(hide = true)]
    SessionStart,
}

#[derive(Subcommand)]
pub enum AgentCmd {
    /// Install or repair the rgit skill and session hook in agent apps (the
    /// ones found on this machine by default); with --mcp also rgit's MCP
    /// tools (for pi and omp, as native tools).
    Install {
        /// Apps to set up; by default every one found on PATH or by its config folder.
        #[arg(value_enum)]
        apps: Vec<crate::agent::App>,
        /// This project's config instead of your user config (codex and opencode only).
        #[arg(long)]
        project: bool,
        /// Also add rgit's MCP tools: `rgit mcp` for Claude Code, Codex and
        /// OpenCode, native tools running `rgit tool` for pi and omp.
        #[arg(long, overrides_with = "no_mcp")]
        mcp: bool,
        /// Add no MCP tools (the default); tools already installed stay.
        #[arg(long, overrides_with = "mcp")]
        no_mcp: bool,
    },
    /// Show each app's plugin or extension, skill, hook and MCP state.
    Status,
    /// Remove what `rgit agent install` added (from every app by default).
    Uninstall {
        /// Apps to remove rgit from; by default all of them.
        #[arg(value_enum)]
        apps: Vec<crate::agent::App>,
        /// This project's config instead of your user config (codex and opencode only).
        #[arg(long)]
        project: bool,
        /// Remove only the skill.
        #[arg(long, group = "only")]
        skill_only: bool,
        /// Remove only the session hook.
        #[arg(long, group = "only")]
        hook_only: bool,
        /// Remove only the MCP tools.
        #[arg(long, group = "only")]
        mcp_only: bool,
    },
    /// Print the rgit SKILL.md, or its command reference.
    Skill {
        /// Print references/commands.md instead of SKILL.md.
        #[arg(long)]
        reference: bool,
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
    /// Lay the list out in columns: always, never, auto, column, row, plain,
    /// dense, nodense (default `column.branch`, `column.ui`).
    #[arg(long, value_name = "OPTIONS", num_args = 0..=1, require_equals = true, default_missing_value = "always")]
    pub column: Option<String>,
    /// List one branch per line.
    #[arg(long = "no-column")]
    pub no_column: bool,
    /// With -v, show object names in at least N hex digits.
    #[arg(long, value_name = "N", require_equals = true)]
    pub abbrev: Option<usize>,
    /// With -v, show full object names.
    #[arg(long = "no-abbrev")]
    pub no_abbrev: bool,
    /// With --format, print no newline for a branch that formats empty.
    #[arg(long = "omit-empty")]
    pub omit_empty: bool,
    /// Keep a reflog for the new branch.
    #[arg(long = "create-reflog")]
    pub create_reflog: bool,
    /// Accepted for git compatibility (submodules are left alone).
    #[arg(long = "recurse-submodules")]
    pub recurse_submodules: bool,
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
    /// Lay the list out in columns: always, never, auto, column, row, plain,
    /// dense, nodense (default `column.tag`, `column.ui`).
    #[arg(long, value_name = "OPTIONS", num_args = 0..=1, require_equals = true, default_missing_value = "always")]
    pub column: Option<String>,
    /// List one tag per line.
    #[arg(long = "no-column")]
    pub no_column: bool,
    /// Add a `<token>: <value>` (or `<token>=<value>`) trailer to the
    /// message; implies an annotated tag.
    #[arg(long, value_name = "TOKEN[(=|:)VALUE]")]
    pub trailer: Vec<String>,
    /// Keep a reflog for the new tag.
    #[arg(long = "create-reflog")]
    pub create_reflog: bool,
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
        /// Print nothing on success.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Apply a stash without dropping it.
    Apply {
        /// The stash: `N` or `stash@{N}` (defaults to the most recent).
        #[arg(value_parser = stash_ref)]
        index: Option<usize>,
        /// Restore the staged changes too (git's --index).
        #[arg(long = "index")]
        restore_index: bool,
        /// Print nothing on success.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Drop a stash.
    Drop {
        /// The stash: `N` or `stash@{N}` (defaults to the most recent).
        #[arg(value_parser = stash_ref)]
        index: Option<usize>,
        /// Print nothing on success.
        #[arg(short, long)]
        quiet: bool,
    },
    /// List the stashes, as `git log -g refs/stash` in the `%gd: %gs` format
    /// unless another is given (`%gd`, `%gD` and `%gs` name the entry).
    List {
        #[command(flatten)]
        pretty: PrettyArgs,
        #[command(flatten)]
        diff: DiffFormat,
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
        /// The diffstat, then the patch.
        #[arg(long = "patch-with-stat")]
        patch_with_stat: bool,
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
    #[command(flatten)]
    pub pathspec_file: PathspecFile,
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
    /// Check out the next commit to test (after marking commits by hand).
    Next,
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
                if !paths.is_empty() {
                    args.push("--".into());
                    args.extend(paths.iter().cloned());
                }
                args
            }
            BisectCmd::Bad { revs } => words("bad", revs),
            BisectCmd::Good { revs } => words("good", revs),
            BisectCmd::New { revs } => words("new", revs),
            BisectCmd::Old { revs } => words("old", revs),
            BisectCmd::Skip { revs } => words("skip", revs),
            BisectCmd::Reset { commit } => words("reset", commit.as_slice()),
            BisectCmd::Log => words("log", &[]),
            BisectCmd::Next => words("next", &[]),
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

/// `rebase`'s further flags, as git's.
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
    #[command(flatten)]
    pub sign: SignArgs,
    #[command(flatten)]
    pub rerere: RerereFlags,
}

impl RebaseFlags {
    fn options(&self) -> rgit_git::RebaseOptions {
        rgit_git::RebaseOptions {
            no_autosquash: self.no_autosquash,
            force: self.force_rebase,
            fork_point: match (self.fork_point, self.no_fork_point) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            },
            keep_base: self.keep_base,
            committer_date_is_author_date: self.committer_date_is_author_date,
            reset_author_date: self.reset_author_date,
            rebase_merges: self.rebase_merges.as_deref().map(|m| m == "rebase-cousins"),
            empty: self.empty.clone(),
            reapply_cherry_picks: self.reapply_cherry_picks,
            signoff: self.signoff,
            autostash: self.autostash,
            no_verify: self.no_verify,
            quiet: self.quiet,
            verbose: self.verbose,
            sign: self.sign.gpg_sign.clone(),
            no_sign: self.sign.no_gpg_sign,
            rerere_autoupdate: self.rerere.value(),
            ..Default::default()
        }
    }
}

/// `rgit config`'s arguments, as git's.
#[derive(clap::Args, Default)]
pub struct ConfigArgs {
    /// The key (`section.name`); the section for --rename-section and
    /// --remove-section, a regex for --get-regexp.
    pub key: Option<String>,
    /// The value to set; a value pattern with --get, --get-all, --get-regexp
    /// and --unset; the new name for --rename-section.
    pub value: Option<String>,
    /// Only values matching this regex (`!` negates) are replaced.
    pub value_pattern: Option<String>,
    /// Use the user's global config (~/.gitconfig or $GIT_CONFIG_GLOBAL).
    #[arg(long)]
    pub global: bool,
    /// Use the system config (/etc/gitconfig or $GIT_CONFIG_SYSTEM).
    #[arg(long)]
    pub system: bool,
    /// Use only the repository's config.
    #[arg(long)]
    pub local: bool,
    /// Use the worktree's config (config.worktree when enabled).
    #[arg(long)]
    pub worktree: bool,
    /// Use this config file.
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    pub file: Option<String>,
    /// Print the key's value (the default with just a key).
    #[arg(long)]
    pub get: bool,
    /// Print every value of a multi-valued key.
    #[arg(long)]
    pub get_all: bool,
    /// Print every `key value` whose key matches the regex.
    #[arg(long)]
    pub get_regexp: bool,
    /// Replace every value of a multi-valued key with one.
    #[arg(long)]
    pub replace_all: bool,
    /// Add a value to a multi-valued key instead of replacing it.
    #[arg(long)]
    pub add: bool,
    /// Remove the key.
    #[arg(long)]
    pub unset: bool,
    /// Remove every value of a multi-valued key.
    #[arg(long)]
    pub unset_all: bool,
    /// Rename a section: `--rename-section <old> <new>`.
    #[arg(long)]
    pub rename_section: bool,
    /// Remove a section and every key in it.
    #[arg(long)]
    pub remove_section: bool,
    /// List every `key=value`.
    #[arg(short = 'l', long)]
    pub list: bool,
    /// Open the config file in the editor.
    #[arg(short = 'e', long)]
    pub edit: bool,
    /// Read and write values as this type.
    #[arg(long = "type", value_name = "TYPE",
          value_parser = ["bool", "int", "bool-or-int", "path", "expiry-date", "color"])]
    pub kind: Option<String>,
    /// Same as --type=bool.
    #[arg(long = "bool")]
    pub as_bool: bool,
    /// Same as --type=int (k/m/g suffixes allowed).
    #[arg(long = "int")]
    pub as_int: bool,
    /// Same as --type=bool-or-int.
    #[arg(long = "bool-or-int")]
    pub as_bool_or_int: bool,
    /// Same as --type=path (expands `~/`).
    #[arg(long = "path")]
    pub as_path: bool,
    /// Same as --type=expiry-date.
    #[arg(long = "expiry-date")]
    pub as_expiry_date: bool,
    /// Match the value pattern as a literal string, not a regex.
    #[arg(long)]
    pub fixed_value: bool,
    /// Prefix each value with the file it came from.
    #[arg(long)]
    pub show_origin: bool,
    /// Prefix each value with its scope (system, global, local, worktree).
    #[arg(long)]
    pub show_scope: bool,
    /// Print only the key names (--list, --get-regexp).
    #[arg(long)]
    pub name_only: bool,
    /// End each value with NUL and put a newline between key and value.
    #[arg(short = 'z', long)]
    pub null: bool,
    /// The value to print when the key is not set.
    #[arg(long, value_name = "VALUE")]
    pub default: Option<String>,
    /// Follow include.path and includeIf in reads (default only without a scope).
    #[arg(long, overrides_with = "no_includes")]
    pub includes: bool,
    /// Do not follow include.path and includeIf in reads.
    #[arg(long)]
    pub no_includes: bool,
    /// With the `get`/`set`/`unset` forms: every value (git's --all).
    #[arg(long)]
    pub all: bool,
    /// With `get`: the key is a regex (git's --regexp).
    #[arg(long)]
    pub regexp: bool,
    /// With `get`: print each key before its value.
    #[arg(long)]
    pub show_names: bool,
    /// With the `get`/`set`/`unset` forms: only values matching this pattern.
    #[arg(long = "value", value_name = "PATTERN")]
    pub value_filter: Option<String>,
}

/// `rgit hash-object`'s arguments, as git's.
#[derive(clap::Args, Default)]
pub struct HashObjectArgs {
    /// Files to hash.
    pub paths: Vec<String>,
    /// Store the objects in the repository.
    #[arg(short = 'w')]
    pub write: bool,
    /// Hash stdin (before any files).
    #[arg(long)]
    pub stdin: bool,
    /// Read the paths of the files to hash from stdin, one per line.
    #[arg(long, conflicts_with = "stdin")]
    pub stdin_paths: bool,
    /// The object type: blob (default), tree, commit or tag.
    #[arg(short = 't', value_name = "TYPE")]
    pub kind: Option<String>,
    /// Skip the format check of trees, commits and tags.
    #[arg(long)]
    pub literally: bool,
    /// Filter as if the content were at this path (e.g. for --stdin).
    #[arg(long, value_name = "PATH", conflicts_with = "no_filters")]
    pub path: Option<String>,
    /// Hash the content as is, without the clean filters (crlf, filter drivers).
    #[arg(long)]
    pub no_filters: bool,
}

/// `rgit apply`'s arguments, as git's.
#[derive(clap::Args, Default)]
pub struct ApplyArgs {
    /// Patch files (reads stdin when none or `-`).
    pub patches: Vec<String>,
    /// Apply to the index only, leaving the working tree as it is.
    #[arg(long)]
    pub cached: bool,
    /// Apply to both the index and the working tree.
    #[arg(long, conflicts_with = "cached")]
    pub index: bool,
    /// Only check that the patch applies.
    #[arg(long)]
    pub check: bool,
    /// Undo the patch (apply it in reverse).
    #[arg(short = 'R', long)]
    pub reverse: bool,
    /// Print the patch's diffstat instead of applying it.
    #[arg(long)]
    pub stat: bool,
    /// Print added and deleted line counts per file instead of applying it.
    #[arg(long)]
    pub numstat: bool,
    /// Print created, deleted, renamed and mode-changed files instead of
    /// applying it.
    #[arg(long)]
    pub summary: bool,
    /// Apply even with --stat, --numstat or --summary.
    #[arg(long)]
    pub apply: bool,
    /// Fall back to a three-way merge with the patch's preimage blobs.
    #[arg(short = '3', long = "3way")]
    pub three_way: bool,
    /// Apply the hunks that fit and write the others to `<file>.rej`.
    #[arg(long, conflicts_with = "three_way")]
    pub reject: bool,
    /// Strip this many leading path components (default 1).
    #[arg(short = 'p', value_name = "N")]
    pub strip: Option<usize>,
    /// Prepend this folder to every path in the patch.
    #[arg(long, value_name = "ROOT")]
    pub directory: Option<String>,
    /// Apply only to paths matching this glob.
    #[arg(long, value_name = "GLOB")]
    pub include: Vec<String>,
    /// Skip paths matching this glob.
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// On trailing whitespace in added lines: nowarn, warn, fix, error or
    /// error-all.
    #[arg(long, value_name = "ACTION",
          value_parser = ["nowarn", "warn", "fix", "strip", "error", "error-all"])]
    pub whitespace: Option<String>,
    /// Report each file as it is checked and applied.
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// A patch that changes no files is not an error.
    #[arg(long)]
    pub allow_empty: bool,
    /// With --3way, resolve conflicts to our side.
    #[arg(long, requires = "three_way", conflicts_with_all = ["theirs", "union"])]
    pub ours: bool,
    /// With --3way, resolve conflicts to their side.
    #[arg(long, requires = "three_way", conflicts_with = "union")]
    pub theirs: bool,
    /// With --3way, keep both sides of conflicts.
    #[arg(long, requires = "three_way")]
    pub union: bool,
    /// Count hunk lines instead of trusting the `@@` headers (hand-edited
    /// patches).
    #[arg(long)]
    pub recount: bool,
    /// Print no progress.
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Drop context down to N lines around each hunk when it does not
    /// match otherwise.
    #[arg(short = 'C', value_name = "N")]
    pub context: Option<usize>,
    /// Apply hunks without context where their header says (`diff -U0`).
    #[arg(long)]
    pub unidiff_zero: bool,
    /// Ignore changes in the amount of whitespace when matching context.
    #[arg(long, visible_alias = "ignore-space-change")]
    pub ignore_whitespace: bool,
    /// Tolerate a patch missing newlines at the ends of files.
    #[arg(long)]
    pub inaccurate_eof: bool,
    /// Let hunks overlap lines earlier hunks changed.
    #[arg(long)]
    pub allow_overlap: bool,
    /// Mark new files intent-to-add in the index (working-tree apply only).
    #[arg(short = 'N', long)]
    pub intent_to_add: bool,
    /// Write an index of the preimage blobs to this file instead of
    /// applying.
    #[arg(long, value_name = "FILE")]
    pub build_fake_ancestor: Option<String>,
    /// With --numstat, end each entry with NUL instead of a newline.
    #[arg(short = 'z')]
    pub z: bool,
}

/// `rgit format-patch`'s arguments, as git's.
#[derive(clap::Args, Default)]
pub struct FormatPatchArgs {
    /// `-<n>` for the newest n commits, `<rev>` for the commits after it
    /// up to HEAD, or a range `<a>..<b>`.
    #[arg(allow_negative_numbers = true)]
    pub revs: Vec<String>,
    /// Write the files to this folder.
    #[arg(short = 'o', long = "output-directory", value_name = "DIR")]
    pub output_dir: Option<String>,
    /// Print the patches instead of writing files.
    #[arg(long)]
    pub stdout: bool,
    /// Number the subjects `[PATCH n/m]` even for one patch.
    #[arg(short = 'n', long, conflicts_with = "no_numbered")]
    pub numbered: bool,
    /// Never number the subjects.
    #[arg(short = 'N', long)]
    pub no_numbered: bool,
    /// Number the first patch this (default 1).
    #[arg(long, value_name = "N")]
    pub start_number: Option<usize>,
    /// Name the files 1, 2, ... without a suffix.
    #[arg(long)]
    pub numbered_files: bool,
    /// The file suffix (default .patch).
    #[arg(long, value_name = "SFX")]
    pub suffix: Option<String>,
    /// Keep the subject as is, without `[PATCH]`.
    #[arg(short = 'k', long, conflicts_with = "numbered")]
    pub keep_subject: bool,
    /// Leave out the diffstat.
    #[arg(short = 'p', long)]
    pub no_stat: bool,
    /// The subject prefix instead of PATCH.
    #[arg(long, value_name = "PREFIX")]
    pub subject_prefix: Option<String>,
    /// Prefix the subject with RFC (or this text; `-text` goes after).
    #[arg(long, value_name = "TEXT", num_args = 0..=1, require_equals = true,
          default_missing_value = "RFC")]
    pub rfc: Option<String>,
    /// Add a Signed-off-by trailer for you.
    #[arg(short = 's', long)]
    pub signoff: bool,
    /// Send as this ident (default you), keeping the author in the body.
    #[arg(long, value_name = "IDENT", num_args = 0..=1, require_equals = true,
          default_missing_value = "")]
    pub from: Option<String>,
    /// The longest file name, suffix included (default 64).
    #[arg(long, value_name = "N")]
    pub filename_max_length: Option<usize>,
    /// Show binary files as "Binary files ... differ".
    #[arg(long)]
    pub no_binary: bool,
    /// Leave out commits whose change upstream already has.
    #[arg(long)]
    pub ignore_if_in_upstream: bool,
    /// How the cover letter uses the branch description: message, subject,
    /// auto or none.
    #[arg(long, value_name = "MODE", value_parser = ["message", "subject", "auto", "none"])]
    pub cover_from_description: Option<String>,
    /// Use this file as the branch description.
    #[arg(long, value_name = "FILE")]
    pub description_file: Option<String>,
    /// Add an interdiff against this previous version to the cover letter.
    #[arg(long, value_name = "REV")]
    pub interdiff: Option<String>,
    /// Add a range-diff against this previous version to the cover letter.
    #[arg(long, value_name = "REV")]
    pub range_diff: Option<String>,
    /// Percent for pairing commits in --range-diff (default 60).
    #[arg(long, value_name = "N")]
    pub creation_factor: Option<usize>,
    /// Accepted for git compatibility.
    #[arg(long, hide = true)]
    pub progress: bool,
    /// Mark the series as version N (`[PATCH vN]`, `vN-` file names).
    #[arg(short = 'v', long = "reroll-count", value_name = "N")]
    pub reroll: Option<String>,
    /// Add a cover letter (0000-cover-letter.patch) to fill in.
    #[arg(long)]
    pub cover_letter: bool,
    /// Add Message-ID and In-Reply-To headers: shallow (reply to the first)
    /// or deep (reply to the previous).
    #[arg(long, value_name = "STYLE", num_args = 0..=1, require_equals = true,
          default_missing_value = "shallow", value_parser = ["shallow", "deep"])]
    pub thread: Option<String>,
    /// Make the first mail a reply to this Message-ID.
    #[arg(long, value_name = "ID")]
    pub in_reply_to: Option<String>,
    /// Add a To: header.
    #[arg(long, value_name = "ADDR")]
    pub to: Vec<String>,
    /// Add a Cc: header.
    #[arg(long, value_name = "ADDR")]
    pub cc: Vec<String>,
    /// Add this header line.
    #[arg(long, value_name = "HEADER")]
    pub add_header: Vec<String>,
    /// Record the base commit (`auto`: the upstream's merge base).
    #[arg(long, value_name = "COMMIT")]
    pub base: Option<String>,
    /// Put all zeros in the `From <commit>` line.
    #[arg(long)]
    pub zero_commit: bool,
    /// The signature after `-- ` (default: git's version).
    #[arg(long, value_name = "TEXT", conflicts_with = "no_signature")]
    pub signature: Option<String>,
    /// Read the signature from this file.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["signature", "no_signature"])]
    pub signature_file: Option<String>,
    /// Leave out the signature.
    #[arg(long)]
    pub no_signature: bool,
    /// With one revision, format every commit up to it, from the root.
    #[arg(long)]
    pub root: bool,
    /// Do not print the names of the files written.
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Put each patch in a MIME attachment, with this boundary (default:
    /// git's version).
    #[arg(long, value_name = "BOUNDARY", num_args = 0..=1, require_equals = true,
          default_missing_value = "")]
    pub attach: Option<String>,
    /// Like --attach, as an inline part.
    #[arg(long, value_name = "BOUNDARY", num_args = 0..=1, require_equals = true,
          default_missing_value = "")]
    pub inline: Option<String>,
    /// No MIME attachment (overrides format.attach).
    #[arg(long)]
    pub no_attach: bool,
    /// Add the commit's notes after the `---` line (of this notes ref).
    #[arg(long, value_name = "REF", num_args = 0..=1, require_equals = true,
          default_missing_value = "")]
    pub notes: Vec<String>,
    /// No notes (overrides format.notes).
    #[arg(long)]
    pub no_notes: bool,
    /// RFC 2047-encode non-ASCII From: and Subject: (the default).
    #[arg(long, overrides_with = "no_encode_email_headers")]
    pub encode_email_headers: bool,
    /// Leave non-ASCII From: and Subject: as raw UTF-8.
    #[arg(long)]
    pub no_encode_email_headers: bool,
    /// Write all the patches to this one file.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["stdout", "output_dir"])]
    pub output: Option<String>,
    /// No cover letter (overrides format.coverLetter).
    #[arg(long, conflicts_with = "cover_letter")]
    pub no_cover_letter: bool,
    /// No threading headers (overrides format.thread).
    #[arg(long, conflicts_with = "thread")]
    pub no_thread: bool,
    /// With --from, keep the author's From: in the body even when it is
    /// the sender.
    #[arg(long, overrides_with = "no_force_in_body_from")]
    pub force_in_body_from: bool,
    /// Only put the author's From: in the body when it is not the sender.
    #[arg(long)]
    pub no_force_in_body_from: bool,
    /// Drop the To: addresses of format.to.
    #[arg(long)]
    pub no_to: bool,
    /// Drop the Cc: addresses of format.cc.
    #[arg(long)]
    pub no_cc: bool,
}

/// `cherry-pick` and `revert` flags past the common ones.
#[derive(clap::Args, Default)]
pub struct PickFlags {
    /// The merge strategy; `ours` keeps HEAD's tree.
    #[arg(long, value_name = "STRATEGY", value_parser = ["ort", "recursive", "resolve", "ours"])]
    pub strategy: Option<String>,
    /// How to clean up each message: strip, whitespace, verbatim, scissors or default.
    #[arg(long, value_name = "MODE",
          value_parser = ["strip", "whitespace", "verbatim", "scissors", "default"])]
    pub cleanup: Option<String>,
    #[command(flatten)]
    pub rerere: RerereFlags,
}

/// git's `--[no-]rerere-autoupdate`.
#[derive(clap::Args, Default)]
pub struct RerereFlags {
    /// Stage the files rerere resolved with a recorded resolution.
    #[arg(long = "rerere-autoupdate")]
    pub rerere_autoupdate: bool,
    /// Leave them unstaged, whatever rerere.autoUpdate says.
    #[arg(long = "no-rerere-autoupdate", conflicts_with = "rerere_autoupdate")]
    pub no_rerere_autoupdate: bool,
}

impl RerereFlags {
    pub fn value(&self) -> Option<bool> {
        (self.rerere_autoupdate || self.no_rerere_autoupdate).then_some(self.rerere_autoupdate)
    }
}

/// `rgit am`'s arguments, as git's.
#[derive(clap::Args, Default)]
pub struct AmArgs {
    /// mbox files (reads stdin when none).
    pub mbox: Vec<String>,
    /// Give up and restore the branch as it was.
    #[arg(long, conflicts_with_all = ["cont", "skip", "quit"])]
    pub abort: bool,
    /// Commit the resolved patch and go on.
    #[arg(
        long = "continue",
        visible_alias = "resolved",
        short = 'r',
        conflicts_with = "skip"
    )]
    pub cont: bool,
    /// Skip the current patch.
    #[arg(long)]
    pub skip: bool,
    /// Stop, keeping the branch and index as they are.
    #[arg(long, conflicts_with_all = ["cont", "skip"])]
    pub quit: bool,
    /// Commit the stopped empty patch as an empty commit and go on.
    #[arg(long, conflicts_with_all = ["cont", "skip", "abort", "quit"])]
    pub allow_empty: bool,
    /// Keep CR at the end of lines (default: am.keepcr).
    #[arg(long)]
    pub keep_cr: bool,
    /// Strip CR at the end of lines (overrides am.keepcr).
    #[arg(long, conflicts_with = "keep_cr")]
    pub no_keep_cr: bool,
    /// Print the patch am stopped at (`diff`: only its diff, `raw`: the mail).
    #[arg(long, value_name = "PART", num_args = 0..=1, require_equals = true,
          default_missing_value = "raw", value_parser = ["diff", "raw"])]
    pub show_current_patch: Option<String>,
    /// Fall back to a three-way merge.
    #[arg(short = '3', long = "3way")]
    pub three_way: bool,
    /// Add a Signed-off-by trailer.
    #[arg(short = 's', long)]
    pub signoff: bool,
    /// Keep the subject as is (no stripping of `[PATCH]`).
    #[arg(short = 'k', long)]
    pub keep: bool,
    /// Keep a `[...]` that is not `[PATCH ...]` in the subject.
    #[arg(long)]
    pub keep_non_patch: bool,
    /// Add the Message-ID to the commit message.
    #[arg(short = 'm', long)]
    pub message_id: bool,
    /// Drop everything above a `-- >8 --` scissors line.
    #[arg(short = 'c', long)]
    pub scissors: bool,
    /// Keep a scissors line and what is above it (overrides mailinfo.scissors).
    #[arg(long, conflicts_with = "scissors")]
    pub no_scissors: bool,
    /// Use the author date as the committer date.
    #[arg(long)]
    pub committer_date_is_author_date: bool,
    /// Use the committer date as the author date.
    #[arg(long)]
    pub ignore_date: bool,
    /// Confirm each patch on the terminal before applying it.
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// Skip the pre-applypatch and applypatch-msg hooks.
    #[arg(short = 'n', long)]
    pub no_verify: bool,
    /// On a patch with no changes: stop, drop or keep.
    #[arg(long, value_name = "ACTION", value_parser = ["stop", "drop", "keep"])]
    pub empty: Option<String>,
    /// Strip this many leading path components (as apply's -p).
    #[arg(short = 'p', value_name = "N")]
    pub strip: Option<usize>,
    /// Prepend this folder to every path.
    #[arg(long, value_name = "ROOT")]
    pub directory: Option<String>,
    /// Skip paths matching this glob.
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Apply only paths matching this glob.
    #[arg(long, value_name = "GLOB")]
    pub include: Vec<String>,
    /// Leave the hunks that do not apply in `.rej` files.
    #[arg(long)]
    pub reject: bool,
    /// On trailing whitespace: nowarn, warn, fix, error or error-all.
    #[arg(long, value_name = "ACTION")]
    pub whitespace: Option<String>,
    /// Print nothing but errors.
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// The patches' format (default: detected from the first one).
    #[arg(long, value_name = "FORMAT",
          value_parser = ["mbox", "mboxrd", "stgit", "stgit-series", "hg"])]
    pub patch_format: Option<String>,
    #[command(flatten)]
    pub sign: SignArgs,
    #[command(flatten)]
    pub rerere: RerereFlags,
}

/// `rgit archive`'s arguments, as git's.
#[derive(clap::Args, Default)]
pub struct ArchiveArgs {
    /// The revision (default HEAD); `-0` to `-9` set the compression level.
    #[arg(allow_negative_numbers = true)]
    pub rev: Option<String>,
    /// Limit to these paths.
    #[arg(allow_negative_numbers = true)]
    pub paths: Vec<String>,
    /// tar, tgz, tar.gz, zip or a tar.<format>.command filter (default:
    /// from -o's extension, else tar).
    #[arg(long)]
    pub format: Option<String>,
    /// Write to this file instead of stdout.
    #[arg(short = 'o', long)]
    pub output: Option<String>,
    /// Put every entry under this folder (e.g. `project/`).
    #[arg(long)]
    pub prefix: Option<String>,
    /// Add this untracked file (under the prefix).
    #[arg(long, value_name = "FILE")]
    pub add_file: Vec<String>,
    /// Add a file with this content: `<path>:<content>`.
    #[arg(long, value_name = "PATH:CONTENT")]
    pub add_virtual_file: Vec<String>,
    /// Also honour the working tree's .gitattributes (export-ignore,
    /// export-subst).
    #[arg(long)]
    pub worktree_attributes: bool,
    /// List the formats.
    #[arg(short = 'l', long)]
    pub list: bool,
    /// Archive from this repository instead: a remote name, a local path, or
    /// an ssh:// , git:// or `host:path` URL (git's upload-archive protocol).
    #[arg(long, value_name = "REPO")]
    pub remote: Option<String>,
    /// With --remote, the remote's upload-archive command.
    #[arg(long, value_name = "COMMAND", requires = "remote")]
    pub exec: Option<String>,
    /// The entries' modification time (a date, `now`, `2.weeks.ago`, ...).
    #[arg(long, value_name = "TIME")]
    pub mtime: Option<String>,
    /// Print each archived path on stderr.
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// The current folder below the top (`src/`), set from where rgit runs:
    /// git archives only it.
    #[arg(skip)]
    pub cwd: String,
}

/// `rgit difftool` and `rgit mergetool` arguments, as git's.
#[derive(clap::Args, Default)]
pub struct ToolArgs {
    /// difftool: up to two revisions to compare (default: the index against
    /// the working tree). mergetool: the conflicted paths.
    pub revs: Vec<String>,
    /// Limit to these paths.
    #[arg(last = true)]
    pub paths: Vec<String>,
    /// The tool (default diff.tool / merge.tool).
    #[arg(short = 't', long)]
    pub tool: Option<String>,
    /// difftool: run this command with the two files instead of a tool.
    #[arg(short = 'x', long = "extcmd", value_name = "COMMAND")]
    pub extcmd: Option<String>,
    /// Do not ask before each file.
    #[arg(short = 'y', long = "no-prompt")]
    pub no_prompt: bool,
    /// Ask before each file even if configured not to.
    #[arg(long, conflicts_with = "no_prompt")]
    pub prompt: bool,
    /// difftool: compare the index with HEAD (or the revision).
    #[arg(long, alias = "staged")]
    pub cached: bool,
    /// difftool: compare whole folders at once.
    #[arg(short = 'd', long)]
    pub dir_diff: bool,
    /// Stop when the tool exits with an error.
    #[arg(long)]
    pub trust_exit_code: bool,
    /// List the known tools.
    #[arg(long)]
    pub tool_help: bool,
}

/// A stash as git names it: `N` or `stash@{N}`.
pub(crate) fn stash_ref(s: &str) -> Result<usize, String> {
    s.strip_prefix("stash@{")
        .and_then(|r| r.strip_suffix('}'))
        .unwrap_or(s)
        .parse()
        .map_err(|_| format!("expected N or stash@{{N}}, got {s:?}"))
}

/// Where a note's text comes from, as in git's `notes add`.
#[derive(clap::Args, Default, Clone)]
pub struct NoteMessage {
    /// The note text; several -m become paragraphs.
    #[arg(short, long)]
    pub message: Vec<String>,
    /// Read the note text from this file (`-` for stdin).
    #[arg(short = 'F', long = "file", value_name = "FILE")]
    pub file: Vec<String>,
    /// Reuse this blob (e.g. another note) as the note.
    #[arg(short = 'C', long = "reuse-message", value_name = "OBJECT")]
    pub reuse: Option<String>,
    /// Like -C, then edit it.
    #[arg(short = 'c', long = "reedit-message", value_name = "OBJECT")]
    pub reedit: Option<String>,
    /// Keep an empty note instead of removing it.
    #[arg(long)]
    pub allow_empty: bool,
    /// Edit the text in the editor before saving.
    #[arg(short = 'e', long)]
    pub edit: bool,
    /// Put this line between paragraphs instead of a blank line.
    #[arg(long, value_name = "TEXT", conflicts_with = "no_separator")]
    pub separator: Option<String>,
    /// Put nothing between paragraphs.
    #[arg(long)]
    pub no_separator: bool,
    /// Keep the text as given: no cleanup of blank lines and trailing spaces.
    #[arg(long, overrides_with = "stripspace")]
    pub no_stripspace: bool,
    /// Clean up blank lines and trailing spaces (the default, but for -C).
    #[arg(long)]
    pub stripspace: bool,
}

impl NoteMessage {
    /// What goes after a paragraph before the next, as git's --separator
    /// says (a blank line once the paragraph ends with its newline).
    fn separator(&self) -> String {
        match (&self.separator, self.no_separator) {
            (_, true) => String::new(),
            (Some(s), _) if s.ends_with('\n') => s.clone(),
            (Some(s), _) => format!("{s}\n"),
            (None, _) => "\n".to_owned(),
        }
    }

    /// --stripspace (Some(true)), --no-stripspace (Some(false)) or neither.
    fn strip(&self) -> Option<bool> {
        if self.no_stripspace {
            Some(false)
        } else {
            self.stripspace.then_some(true)
        }
    }
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
    /// Attach a note to an object (default HEAD); opens the editor without
    /// -m, -F or -C.
    Add {
        /// The annotated object.
        rev: Option<String>,
        #[command(flatten)]
        text: NoteMessage,
        /// Replace an existing note.
        #[arg(short, long)]
        force: bool,
    },
    /// Copy the note of one object to another (default HEAD).
    Copy {
        /// The object whose note is copied.
        from: String,
        /// The object that gets it (default HEAD).
        to: Option<String>,
        /// Replace an existing note.
        #[arg(short, long)]
        force: bool,
    },
    /// Add a paragraph to an object's note, creating it if needed.
    Append {
        /// The annotated object.
        rev: Option<String>,
        #[command(flatten)]
        text: NoteMessage,
    },
    /// Edit an object's note in the editor (default HEAD).
    Edit {
        /// The annotated object.
        rev: Option<String>,
        /// Keep an empty note instead of removing it.
        #[arg(long)]
        allow_empty: bool,
    },
    /// Remove the notes of objects (default HEAD).
    #[command(alias = "rm")]
    Remove {
        /// The annotated objects.
        revs: Vec<String>,
        /// Do not fail on an object without a note.
        #[arg(long)]
        ignore_missing: bool,
    },
    /// Remove the notes of objects that no longer exist.
    Prune {
        /// List them without removing (git's -n).
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Name each removed note's object.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Merge another notes ref into the current one, with their merge base.
    Merge {
        /// The notes ref to merge (e.g. `origin` for refs/notes/origin).
        #[arg(required_unless_present_any = ["commit", "abort"])]
        notes_ref: Option<String>,
        /// On conflicting notes: manual (leave them in
        /// .git/NOTES_MERGE_WORKTREE), ours, theirs, union or cat_sort_uniq
        /// (default: notes.<ref>.mergeStrategy, notes.mergeStrategy, manual).
        #[arg(short, long,
              value_parser = ["manual", "ours", "theirs", "union", "cat_sort_uniq"])]
        strategy: Option<String>,
        /// Commit the notes resolved in .git/NOTES_MERGE_WORKTREE.
        #[arg(long, conflicts_with_all = ["notes_ref", "abort", "strategy"])]
        commit: bool,
        /// Abandon a manual notes merge.
        #[arg(long, conflicts_with_all = ["notes_ref", "strategy"])]
        abort: bool,
        /// Say more (repeatable).
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,
        /// Say less.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Print the notes ref in use.
    GetRef,
}

#[derive(Subcommand)]
pub enum MaintenanceCmd {
    /// Run maintenance tasks now.
    Run {
        /// Only this task (prefetch, loose-objects, incremental-repack, gc,
        /// commit-graph, pack-refs, reflog-expire, worktree-prune,
        /// rerere-gc); repeatable.
        #[arg(long, value_name = "TASK")]
        task: Vec<String>,
        /// Only the tasks whose auto conditions are met.
        #[arg(long)]
        auto: bool,
        /// Run the tasks of this schedule (hourly, daily, weekly).
        #[arg(long, value_name = "FREQ")]
        schedule: Option<String>,
        /// Print nothing.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Register this repository and schedule hourly/daily/weekly runs
    /// (launchd on macOS, systemd timers or cron elsewhere).
    Start {
        /// auto, launchctl, systemd-timer or crontab.
        #[arg(long, value_name = "SCHEDULER")]
        scheduler: Option<String>,
    },
    /// Remove the schedule (the repositories stay registered).
    Stop,
    /// Add this repository to the global maintenance.repo list.
    Register,
    /// Remove this repository from the global maintenance.repo list.
    Unregister {
        /// Do not fail when it is not registered.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum ScalarCmd {
    /// Set scalar's config, start maintenance and add the repository to the
    /// global scalar.repo list.
    Register {
        /// Leave background maintenance off.
        #[arg(long)]
        no_maintenance: bool,
        /// The enlistment (default: the current repository).
        enlistment: Option<String>,
    },
    /// Remove the repository from scalar.repo and maintenance.
    Unregister {
        /// The enlistment (default: the current repository).
        enlistment: Option<String>,
    },
    /// Print the registered repositories.
    List,
    /// Run one task now: all, config, commit-graph, fetch, loose-objects or
    /// pack-files.
    Run {
        /// The task.
        task: String,
        /// The enlistment (default: the current repository).
        enlistment: Option<String>,
    },
    /// Set scalar's config again, overwriting the required values.
    Reconfigure {
        /// Every registered repository.
        #[arg(short, long)]
        all: bool,
        /// enable, disable or keep background maintenance.
        #[arg(long, value_name = "MODE")]
        maintenance: Option<String>,
        /// The enlistment (default: the current repository).
        enlistment: Option<String>,
    },
    /// Unregister an enlistment and delete its folder.
    Delete {
        /// The enlistment.
        enlistment: String,
    },
    /// Clone into `<enlistment>/src` and register it.
    Clone {
        /// The repository to clone.
        url: String,
        /// The enlistment folder (default: from the URL).
        enlistment: Option<String>,
        /// Check out this branch.
        #[arg(short, long)]
        branch: Option<String>,
        /// Fetch only the branch checked out.
        #[arg(long)]
        single_branch: bool,
        /// Clone into the enlistment itself, not `src`.
        #[arg(long)]
        no_src: bool,
        /// Fetch no tags.
        #[arg(long)]
        no_tags: bool,
        /// Accepted for scalar; the whole tree is checked out either way.
        #[arg(long)]
        full_clone: bool,
    },
    /// Print the version.
    Version,
}

#[derive(Subcommand)]
pub enum CommitGraphCmd {
    /// Write the commit-graph: the commits in every pack, or those the refs
    /// reach, or those given on stdin.
    Write {
        /// The commits the refs reach.
        #[arg(long, conflicts_with_all = ["stdin_packs", "stdin_commits"])]
        reachable: bool,
        /// The commits in the packs named on stdin.
        #[arg(long, conflicts_with = "stdin_commits")]
        stdin_packs: bool,
        /// The commits named on stdin (and what they reach).
        #[arg(long)]
        stdin_commits: bool,
        /// Keep the commits already in the graph.
        #[arg(long)]
        append: bool,
        /// Add a layer to a split chain: merge layers as git does,
        /// `no-merge` never, `replace` into one new layer.
        #[arg(long, value_name = "STRATEGY", num_args = 0..=1, require_equals = true,
              default_missing_value = "", value_parser = ["", "no-merge", "replace"])]
        split: Option<String>,
        /// Merge a layer that is at most this many times the new one (2).
        #[arg(long, value_name = "N")]
        size_multiple: Option<u64>,
        /// Merge layers while the new one would pass this many commits.
        #[arg(long, value_name = "N")]
        max_commits: Option<usize>,
        /// Only remove unused layer files older than this date.
        #[arg(long, value_name = "DATE")]
        expire_time: Option<String>,
        /// Write changed-path Bloom filters.
        #[arg(long, overrides_with = "no_changed_paths")]
        changed_paths: bool,
        /// Write none, even if the graph has them.
        #[arg(long)]
        no_changed_paths: bool,
        /// Accepted for git compatibility; rgit prints no progress.
        #[arg(long, overrides_with = "progress")]
        no_progress: bool,
        #[arg(long, hide = true)]
        progress: bool,
    },
    /// Check the commit-graph against the commits it lists.
    Verify {
        /// Only the top layer of a split chain.
        #[arg(long)]
        shallow: bool,
        /// Accepted for git compatibility; rgit prints no progress.
        #[arg(long)]
        no_progress: bool,
    },
}

#[derive(Subcommand)]
pub enum MidxCmd {
    /// Index every pack in objects/pack.
    Write {
        /// Take duplicate objects from this pack (`pack-<hash>.pack`).
        #[arg(long, value_name = "PACK")]
        preferred_pack: Option<String>,
        /// Accepted for git compatibility; rgit prints no progress.
        #[arg(long)]
        no_progress: bool,
    },
    /// Check the multi-pack-index against the packs.
    Verify {
        /// Accepted for git compatibility; rgit prints no progress.
        #[arg(long)]
        no_progress: bool,
    },
    /// Delete packs the multi-pack-index takes no object from.
    Expire {
        /// Accepted for git compatibility; rgit prints no progress.
        #[arg(long)]
        no_progress: bool,
    },
    /// Pack the objects of the oldest small packs into one new pack.
    Repack {
        /// How much to repack (k, m, g suffixes; 0: every pack).
        #[arg(long, value_name = "SIZE", value_parser = parse_size, default_value = "0")]
        batch_size: u64,
        /// Accepted for git compatibility; rgit prints no progress.
        #[arg(long)]
        no_progress: bool,
    },
}

#[derive(Subcommand)]
pub enum BundleCmd {
    /// Write a bundle of the refs and ranges given (`--all`, `main`,
    /// `v1..main`, `^old main`).
    Create {
        /// The bundle file.
        file: String,
        /// What to bundle, as for rev-list.
        #[arg(required = true, allow_hyphen_values = true, num_args = 1..)]
        revs: Vec<String>,
        /// Print nothing.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Check that a bundle is valid and applies to this repository.
    Verify {
        /// The bundle file.
        file: String,
        /// Only say whether it is okay.
        #[arg(short, long)]
        quiet: bool,
    },
    /// List the refs in a bundle.
    ListHeads {
        /// The bundle file.
        file: String,
        /// Only these refs.
        refnames: Vec<String>,
    },
    /// Store a bundle's objects here and print its refs (refs are not
    /// changed).
    Unbundle {
        /// The bundle file.
        file: String,
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
        /// Clone and check out this many submodules at once.
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
        /// With no BRANCH, start the new branch from the one remote branch
        /// named after PATH, tracking it (default `worktree.guessRemote`).
        #[arg(long = "guess-remote", overrides_with = "no_guess_remote")]
        guess_remote: bool,
        /// Start the new branch from HEAD even when a remote has its name.
        #[arg(long = "no-guess-remote")]
        no_guess_remote: bool,
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

/// Parse a `--since`/`--until` date as git's approxidate does: full dates
/// in most formats, or forms like `yesterday 5pm`, `last friday`, `noon`,
/// `Jan 5` or `2 weeks 3 days ago`.
pub(crate) fn parse_date(s: &str) -> anyhow::Result<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    crate::date::approxidate_at(s, now).ok_or_else(|| {
        anyhow::anyhow!("bad date {s:?}; use YYYY-MM-DD, YYYY-MM-DD HH:MM:SS or `2 weeks ago`")
    })
}

/// A commit `--date`: `@<unix>`, `<unix>` or an ISO date, each with an
/// optional `+HHMM`, `+HH:MM` or `Z` offset (default UTC), as unix seconds and
/// the offset in minutes.
pub(crate) fn parse_git_date(s: &str) -> anyhow::Result<(i64, i32)> {
    let s = s.trim();
    let (rest, offset) = match s.strip_suffix('Z') {
        Some(rest) => (rest, Some(0)),
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
                    (rest, Some(sign * (h * 60 + m)))
                }
                _ => (s, None),
            }
        }
    };
    let rest = rest.trim();
    let unix = rest.strip_prefix('@').unwrap_or(rest);
    if let Ok(secs) = unix.parse::<i64>() {
        return Ok((secs, offset.unwrap_or(0)));
    }
    let utc = parse_date(&format!("{rest} +0000"))?;
    // A date without a zone is local time, as git reads it.
    let offset = offset.unwrap_or_else(|| {
        let guess = crate::pretty::local_offset(utc);
        crate::pretty::local_offset(utc - i64::from(guess) * 60)
    });
    Ok((utc - i64::from(offset) * 60, offset))
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
            "sparse-checkout",
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
            "branch", "checkout", "switch", "merge", "rebase", "tag", "stash", "bisect", "rerere",
            "prune",
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
            "verify-commit",
            "verify-tag",
            "repack",
            "pack-refs",
            "commit-graph",
            "multi-pack-index",
            "maintenance",
            "for-each-repo",
            "scalar",
            "credential",
            "credential-store",
            "credential-cache",
            "credential-cache--daemon",
            "hook",
            "cherry",
            "bundle",
            "request-pull",
            "range-diff",
            "difftool",
            "mergetool",
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
            "name-rev",
            "check-attr",
            "check-ref-format",
            "patch-id",
            "stripspace",
            "column",
            "diff-tree",
            "diff-index",
            "diff-files",
            "merge-tree",
            "merge-file",
            "fast-export",
            "fast-import",
            "replay",
            "commit-tree",
            "write-tree",
            "read-tree",
            "update-index",
            "checkout-index",
            "mktree",
            "mktag",
            "get-tar-commit-id",
            "fmt-merge-msg",
            "mailsplit",
            "mailinfo",
            "interpret-trailers",
            "show-branch",
            "fetch-pack",
            "send-pack",
            "diff-pairs",
        ],
    ),
    (
        "Object replacement, packs and diagnostics",
        &[
            "replace",
            "check-mailmap",
            "verify-pack",
            "show-index",
            "index-pack",
            "unpack-objects",
            "pack-objects",
            "prune-packed",
            "update-server-info",
            "unpack-file",
            "merge-one-file",
            "merge-index",
            "bugreport",
            "diagnose",
            "backfill",
        ],
    ),
    ("Code search", &["index"]),
    (
        "Repositories and escape hatch",
        &["init", "clone", "submodule", "git"],
    ),
    (
        "Agent integration and servers",
        &["agent", "mcp", "tool", "serve"],
    ),
];

const SKILL_INTRO: &str = r#"---
name: rgit
description: Use for any git work in this repository - inspecting changes, committing, rewriting or undoing history, branches and stacks, and GitHub/GitLab PRs.
---

# rgit

{DESCRIPTION}. Prefer `rgit` over raw `git`. Always pass `--toon` (alias `--axi`) or `--json`: without them rgit prints git's human text. Run `rgit --toon` with no other arguments first: it prints the repo's current state and the next useful commands.

If `rgit` is not on PATH, install it with `cargo install --locked --git https://github.com/rslib/rgit rgit-cli`.

## Start here

"#;

const SKILL_RULES: &str = r#"
## Output

- rgit prints human text by default, as git does, even when stdout is not a terminal. Agents must add `--toon` (alias `--axi`) to every command for structured TOON with no color, spinners, or prompts; `--json` gives the same data as JSON. Other examples in this skill and its reference omit the flag; add it.
- Use `--toon` or `--axi`, not `--porcelain`. In rgit, as in git, `--porcelain` exists only on `status` and prints git's raw script format, with no counts, hints, or schema.
- Output ends with `help` lines naming useful next commands, already carrying `--toon` (or `--json`); follow them. Lists include counts (`count: 20 of 65 total`) and say explicitly when they are empty.
- `--fields a,b` adds table columns; an unknown field lists the valid ones. `--full` disables truncation of patches, commit bodies, and long output.
- Exit codes: 0 success (including no-ops), 1 error, 2 usage error. With `--toon` or `--json`, errors print `error:` and `help:` on stdout.
- rgit never prompts with `--toon` or `--json`, or when stdin or stdout is not a terminal. Pass every value as a flag or argument.

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
        out.push_str(&format!("- {}\n", line.replace("`rgit ", "`rgit --toon ")));
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
        "# rgit command reference\n\nEvery command with what it does and example invocations. Run `rgit <command> --help` for every flag. The examples omit the output flag: agents add `--toon` (or `--json`) to each one for structured output.\n",
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

impl Command {
    /// Whether the command also works outside a repository, as git's does.
    pub fn runs_without_repo(&self) -> bool {
        match self {
            Command::Config(a) => !a.local && !a.worktree,
            Command::HashObject(a) => !a.write,
            Command::Apply(a) => !a.cached && !a.index && !a.three_way,
            Command::Archive(a) => a.list || a.remote.is_some(),
            Command::LsRemote { .. } | Command::Plumbing(Plumbing::GetTarCommitId) => true,
            Command::Plumbing(Plumbing::InterpretTrailers { .. }) => true,
            Command::Plumbing(Plumbing::Grep { no_index, .. }) => *no_index,
            Command::Plumbing(Plumbing::MergeFile { object_id, .. }) => !object_id,
            Command::Plumbing(p) => p.runs_without_repo(),
            Command::Extra(e) => e.runs_without_repo(),
            Command::Bundle {
                cmd: BundleCmd::ListHeads { .. },
            } => true,
            _ => false,
        }
    }

    /// Whether the command can move HEAD, the index or the files, and so
    /// runs on a sparse checkout widened to every file (see
    /// [`rgit_git::sparse_widen`]).
    pub fn moves_worktree(&self) -> bool {
        matches!(
            self,
            Command::Checkout { .. }
                | Command::Switch { .. }
                | Command::Reset { .. }
                | Command::Merge { .. }
                | Command::Pull { .. }
                | Command::Sync
                | Command::Rebase { .. }
                | Command::CherryPick { .. }
                | Command::Revert { .. }
                | Command::Stash { .. }
                | Command::Am(_)
                | Command::Bisect { .. }
                | Command::Undo
                | Command::Redo
                | Command::Discard { .. }
                | Command::Restore { .. }
                | Command::Uncommit { .. }
                | Command::Squash { .. }
                | Command::Move { .. }
                | Command::Split { .. }
                | Command::Absorb
                | Command::Extend
                | Command::Reword { .. }
                | Command::Next
                | Command::Prev
                | Command::Stack { .. }
        )
    }
}

/// Run a command that needs no repository, outside one; `raw` is git's
/// text output rather than the agent form.
pub fn run_without_repo(command: Command, raw: bool) -> anyhow::Result<crate::output::Output> {
    let text = match command {
        command @ Command::LsRemote { .. } => {
            let cwd = std::env::current_dir().ok();
            return crate::plumbing::ls_remote(cwd.as_deref(), command, raw);
        }
        Command::Config(a) => config(None, a),
        Command::HashObject(a) => hash_object(None, a),
        Command::Apply(a) => apply(None, a),
        Command::Archive(a) => archive_cmd(None, a),
        Command::Bundle { cmd } => bundle(None, cmd),
        Command::Plumbing(
            m @ Plumbing::MergeFile {
                object_id: false, ..
            },
        ) => {
            return crate::plumbing::merge_file(None, m, raw);
        }
        Command::Plumbing(Plumbing::GetTarCommitId) => return crate::plumbing::get_tar_commit_id(),
        Command::Plumbing(Plumbing::InterpretTrailers { args, input }) => {
            return crate::plumbing::interpret_trailers(None, args, input);
        }
        Command::Plumbing(c @ Plumbing::Grep { no_index: true, .. }) => {
            return crate::plumbing::grep(None, c, raw);
        }
        Command::Plumbing(p) => return crate::plumbing::repoless(None, p, raw),
        Command::Extra(e) => return crate::extra::packs(None, e, raw),
        _ => Err(CliError::not_a_repo()),
    };
    text.map(Into::into)
}

/// Take a `log`, `diff` or `show` command's `-W` and word-diff options for
/// the diffs this run prints.
pub(crate) fn set_word_diff(backend: &Arc<dyn GitBackend>, command: &Command) {
    let words = match command {
        Command::Log { words, .. } | Command::Diff { words, .. } | Command::Show { words, .. } => {
            (**words).clone()
        }
        _ => WordDiffArgs::default(),
    };
    FUNCTION_CONTEXT.store(words.function_context, std::sync::atomic::Ordering::Relaxed);
    crate::render::set_words(words.words().map(|(style, re)| {
        if style == rgit_git::userdiff::WordStyle::Color {
            crate::render::set_color(true);
        }
        let backend = backend.clone();
        crate::render::Words {
            style,
            regex: Box::new(move |old, new| match &re {
                Some(re) => Some(re.clone().into_bytes()),
                None => backend.word_regex(old, new).ok().flatten(),
            }),
        }
    }));
}

/// A human `status`, `diff`, `log`, `show` or `blame` with no format asked
/// for prints git's default format, unless `compact()` (rgit.compact) says to
/// keep rgit's compact form.
pub fn git_defaults(mut command: Command, compact: impl FnOnce() -> bool) -> Command {
    let wants = match &command {
        Command::Status { fmt, .. } => !fmt.any(),
        Command::Diff {
            format,
            quiet,
            diff_opts,
            ..
        } => {
            !quiet
                && !format.any()
                && diff_opts.dirstat.is_none()
                && diff_opts.dirstat_by_file.is_none()
                && !diff_opts.cumulative
                && !diff_opts.check
                && !diff_opts.compact_summary
        }
        Command::Log {
            pretty,
            walk_reflogs,
            line_ranges,
            ..
        } => !pretty.any() && !walk_reflogs && line_ranges.is_empty(),
        Command::Show { pretty, .. } => !pretty.any(),
        Command::Blame { format, .. } => {
            !(format.porcelain || format.line_porcelain || format.incremental)
        }
        _ => false,
    };
    if !wants || compact() {
        return command;
    }
    match &mut command {
        Command::Status { fmt, .. } => fmt.long = true,
        Command::Diff { format, .. } => format.patch = true,
        Command::Log { pretty, .. } | Command::Show { pretty, .. } => {
            pretty.pretty = Some("medium".to_owned())
        }
        Command::Blame { format, .. } => format.git_format = true,
        _ => {}
    }
    command
}

/// Run a subcommand and return its compact output. When `interactive`, a
/// missing required argument is prompted for; otherwise it errors. `Mcp` is
/// handled by the caller (it takes over the process), so it is unreachable here.
pub fn run(
    backend: &Arc<dyn GitBackend>,
    command: Command,
    interactive: bool,
) -> anyhow::Result<String> {
    let render = apply_diff_opts(backend, &command)?;
    let out = run_command(backend, command, interactive)?;
    let out = crate::diffopts::prefix_lines(&out, &render.line_prefix);
    match render.output {
        Some(file) => {
            let mut text = out;
            if !text.is_empty() && !text_as_is() && !text.ends_with('\n') {
                text.push('\n');
            }
            std::fs::write(&file, text).map_err(|e| CliError {
                message: format!("could not open '{}': {e}", file.display()),
                help: None,
                code: 128,
            })?;
            print_as_is();
            Ok(String::new())
        }
        None => Ok(out),
    }
}

/// Set up the diff options of a `diff`, `log` or `show` command (and clear
/// them for any other), returning the renderer's share.
pub(crate) fn apply_diff_opts(
    backend: &Arc<dyn GitBackend>,
    command: &Command,
) -> anyhow::Result<crate::diffopts::Render> {
    use crate::diffopts::Sides;
    let (opts, sides, porcelain, format) = match command {
        Command::Diff {
            diff_opts,
            revs,
            paths,
            cached,
            format,
            ..
        } => {
            let revs = split_revs(backend, revs, paths)
                .map(|(r, _)| r)
                .unwrap_or_default();
            let sides = match (revs.len(), *cached) {
                (0, false) => Sides::IndexWorktree,
                (0 | 1, true) => Sides::CommitIndex,
                (1, false) if !revs[0].contains("..") => Sides::CommitWorktree,
                _ => Sides::Commits,
            };
            (diff_opts, sides, true, *format)
        }
        Command::Log {
            diff_opts, format, ..
        }
        | Command::Show {
            diff_opts, format, ..
        } => (diff_opts, Sides::Commits, false, *format),
        _ => {
            crate::diffopts::clear();
            return Ok(Default::default());
        }
    };
    let stats = format.stat
        || format.numstat
        || format.shortstat
        || opts.compact_summary
        || opts.dirstat.is_some()
        || opts.dirstat_by_file.is_some();
    opts.apply(backend, sides, porcelain, !stats)?;
    Ok(crate::diffopts::current())
}

fn run_command(
    backend: &Arc<dyn GitBackend>,
    command: Command,
    interactive: bool,
) -> anyhow::Result<String> {
    // On a terminal, let network ops prompt for a password/passphrase when the
    // agent and credential helpers cannot authenticate.
    if interactive {
        backend.set_credential_prompt(Box::new(crate::creds::TerminalPrompt));
    }
    let slot = match &command {
        Command::Branch { .. } => Some("color.branch"),
        Command::Log { .. } | Command::Show { .. } | Command::Diff { .. } => Some("color.diff"),
        Command::Plumbing(Plumbing::Grep { .. }) => Some("color.grep"),
        Command::Status { .. } => Some("color.status"),
        _ => None,
    };
    let cfg = |k: &str| backend.config_get(k).ok().flatten();
    crate::render::set_color(crate::render::want_color(
        slot.and_then(cfg).or_else(|| cfg("color.ui")).as_deref(),
    ));
    set_word_diff(backend, &command);
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
        Command::Agent { .. }
        | Command::Tool { .. }
        | Command::Hook {
            cmd: HookCmd::SessionStart,
        } => unreachable!("handled before dispatch"),
        Command::Status {
            untracked,
            ignored,
            paths,
            ..
        } => render::status(&status_view(
            backend,
            &paths,
            untracked.as_deref(),
            ignored.as_deref(),
        )?),
        Command::Log {
            walk_reflogs: true, ..
        } => reflog_log(backend, &command)?,
        Command::Log {
            ref pretty,
            format,
            ref merge_diff,
            first_parent,
            ref line_ranges,
            ..
        } if pretty.any() || !line_ranges.is_empty() => {
            let graph = pretty.graph;
            let opts = log_options(backend, &command)?;
            if !line_ranges.is_empty() {
                return line_log(backend, &opts, pretty, line_ranges);
            }
            let (mode, format) = merge_mode(
                merge_diff,
                if first_parent {
                    MergeDiff::FirstParent
                } else {
                    MergeDiff::Off
                },
                format,
            )?;
            let (entries, shown) = if graph {
                if opts.reverse {
                    return Err(CliError {
                        message: "options '--reverse' and '--graph' cannot be used together"
                            .to_owned(),
                        help: None,
                        code: 128,
                    }
                    .into());
                }
                // Parents outside the log end their line, but those past -n
                // are still drawn.
                let all = backend.log(&LogOptions {
                    limit: usize::MAX,
                    offset: 0,
                    boundary: false,
                    ..opts.clone()
                })?;
                let mut shown: std::collections::HashSet<String> =
                    all.iter().map(|e| e.oid.clone()).collect();
                let entries: Vec<_> = if opts.boundary {
                    backend.log(&opts)?
                } else {
                    all.into_iter().skip(opts.offset).take(opts.limit).collect()
                };
                shown.extend(entries.iter().map(|e| e.oid.clone()));
                (entries, shown)
            } else {
                (backend.log(&opts)?, Default::default())
            };
            let diffs = log_diffs(backend, &opts, &entries, format, mode)?;
            let mut pretty = crate::pretty::Pretty::new(backend, pretty, None)?.expect("a format");
            if let Command::Log { walk, parents, .. } = &command {
                pretty.walk = walk.clone();
                pretty.parents = *parents;
            }
            let mut commits = Vec::new();
            for e in &entries {
                let mut c = crate::pretty::parse(&backend.read_object(&e.oid)?);
                c.mark = e.mark;
                c.source = e.source.clone();
                if opts.rewrite_parents {
                    c.parents = e.parents.clone();
                }
                commits.push(c);
            }
            if graph {
                let parents: Vec<Vec<String>> = entries
                    .iter()
                    .map(|e| {
                        let take = if opts.first_parent { 1 } else { usize::MAX };
                        e.parents
                            .iter()
                            .take(take)
                            .filter(|p| shown.contains(*p))
                            .cloned()
                            .collect()
                    })
                    .collect();
                graph_log(&pretty, &commits, &parents, &diffs, format)
            } else {
                pretty_log(&pretty, &commits, &diffs, format)
            }
        }
        Command::Log { format, .. } if format.any() => {
            let opts = log_options(backend, &command)?;
            let entries = backend.log(&opts)?;
            let diffs = log_diffs(backend, &opts, &entries, format, MergeDiff::Off)?;
            let mut out = Vec::new();
            for (e, sections) in entries.iter().zip(&diffs) {
                let files = match sections.first() {
                    Some((_, Some(Changes::Files(files)))) => &files[..],
                    _ => &[],
                };
                let mut text = render::log(std::slice::from_ref(e));
                if !files.is_empty() {
                    text.push('\n');
                    text.push_str(diff_out(files, format).trim_end());
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
        Command::Diff { quiet: true, .. } => {
            diff_files(backend, &command)?;
            String::new()
        }
        Command::Diff { format, .. } => diff_out(&diff_files(backend, &command)?.0, format),
        Command::Show {
            revs,
            paths,
            format,
            diff_opts: _,
            merge_diff,
            pretty,
            no_patch,
            first_parent,
            ..
        } => {
            let revs = if revs.is_empty() {
                vec!["HEAD".to_owned()]
            } else {
                revs
            };
            if let Some(pretty) = crate::pretty::Pretty::new(backend, &pretty, None)?
                && revs.iter().all(|r| {
                    backend
                        .read_object(r)
                        .is_ok_and(|o| o.kind == "commit" || o.kind == "tag")
                })
            {
                // git's show: each commit in the format, then its patch.
                let (mode, format) = merge_mode(
                    &merge_diff,
                    if first_parent {
                        MergeDiff::FirstParent
                    } else {
                        MergeDiff::Dense
                    },
                    format,
                )?;
                let format = if format.any() {
                    format
                } else {
                    DiffFormat {
                        patch: true,
                        ..DiffFormat::default()
                    }
                };
                let mut out = String::new();
                for rev in &revs {
                    // An annotated tag shows first, as git does.
                    let mut obj = backend.read_object(rev)?;
                    while obj.kind == "tag" {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(&pretty.tag(&obj));
                        obj = backend.read_object(&format!("{}^{{}}", obj.id))?;
                    }
                    let id = backend.rev_parse(&obj.id)?;
                    let c = crate::pretty::parse(&backend.read_object(&id)?);
                    let sections = if no_patch {
                        vec![(None, None)]
                    } else {
                        commit_changes(backend, &id, &c.parents, &paths, mode, format)?
                    };
                    if !out.is_empty() && !pretty.terminator {
                        out.push('\n');
                    }
                    out.push_str(&pretty_log(&pretty, &[c], &[sections], format));
                }
                return Ok(out);
            }
            let mut out = Vec::new();
            for rev in &revs {
                out.push(show_one(backend, rev, &paths, format, no_patch)?);
            }
            out.join("\n\n")
        }
        Command::Blame {
            args,
            lines,
            format,
            opts,
        } => {
            let result = blame(backend, &args, &lines, &opts)?;
            if let Some(enc) = &opts.encoding
                && !matches!(enc.to_ascii_lowercase().as_str(), "utf-8" | "utf8" | "none")
            {
                return Err(CliError::usage(format!(
                    "blame --encoding={enc}: rgit prints UTF-8 only"
                )));
            }
            if format.incremental {
                return blame_incremental(backend, &result, opts.contents.as_deref());
            }
            if format.porcelain || format.line_porcelain {
                return blame_porcelain(
                    backend,
                    &result.lines,
                    format.line_porcelain,
                    opts.contents.as_deref(),
                );
            }
            if format.git(&opts) {
                return blame_git(backend, &result, &args[args.len() - 1], &format);
            }
            let mut selected = result.lines;
            for b in &mut selected {
                if format.long_ids && !b.id.is_empty() {
                    b.short_id = b.id.clone();
                }
                if format.show_email && !b.id.is_empty() {
                    b.author = format!("<{}>", b.email);
                }
            }
            render::blame(&selected, !format.no_author)
        }
        Command::Plumbing(c) => crate::plumbing::run(backend, c, true)?.text,
        Command::Extra(c) => crate::extra::run(backend, c, true)?.text,
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
            pathspec_file: _,
            paths,
            all,
            update,
            force,
            dry_run,
            verbose,
            intent_to_add,
            ignore_errors,
            patch,
            interactive: add_interactive,
            edit,
            renormalize,
            chmod,
            sparse,
        } => {
            let named = !paths.is_empty();
            let mut paths = paths;
            let refused = if sparse {
                // A skip-worktree file has nothing on disk to add.
                let outside = rgit_git::outside_only(&backend.git_dir(), &paths, false)?;
                if let Some(p) = outside.iter().find(|p| !backend.workdir().join(p).exists()) {
                    return Err(anyhow::Error::new(CliError {
                        message: format!("pathspec '{p}' did not match any files"),
                        help: None,
                        code: 128,
                    }));
                }
                rgit_git::sparse_everywhere();
                Vec::new()
            } else {
                sparse_refused(backend, &mut paths, !update)?
            };
            if !refused.is_empty() && named && paths.is_empty() {
                return Err(sparse_advice(&refused));
            }
            if patch {
                return crate::interactive::patch(
                    backend,
                    interactive,
                    crate::interactive::PatchMode::Add,
                    None,
                    &paths,
                );
            }
            if add_interactive {
                if !interactive {
                    anyhow::bail!("add -i needs a terminal; stage with `rgit add <paths>`");
                }
                crate::add_interactive::run(backend, &paths)?;
                return Ok(String::new());
            }
            if edit {
                return add_edit(backend, &paths);
            }
            if renormalize {
                backend.renormalize(&paths)?;
                return Ok("ok".to_owned());
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
                if let Some(flip) = chmod.as_deref().filter(|_| !paths.is_empty()) {
                    let skipped = backend.index_chmod(&paths, flip == "+x")?;
                    if !skipped.is_empty() {
                        let sign = &flip[..1];
                        anyhow::bail!(
                            "{}",
                            skipped
                                .iter()
                                .map(|p| format!("cannot chmod {sign}x '{p}'"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        );
                    }
                }
            }
            if !refused.is_empty() {
                return Err(sparse_advice(&refused));
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
            merge,
            conflict,
            overlay,
            patch,
            ..
        } => {
            if merge || conflict.is_some() {
                return merge_paths(backend, &paths, conflict.as_deref());
            }
            if patch {
                use crate::interactive::PatchMode;
                // git's `restore --staged` defaults its source to HEAD.
                let source = source.or_else(|| staged.then(|| "HEAD".to_owned()));
                let mode = match (staged, worktree || !staged) {
                    (true, true) => PatchMode::Checkout,
                    (true, false) => PatchMode::Reset,
                    _ => PatchMode::Worktree,
                };
                return crate::interactive::patch(
                    backend,
                    interactive,
                    mode,
                    source.as_deref(),
                    &paths,
                );
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
            pathspec_file: _,
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
            short,
            porcelain,
            long,
            null,
            branch,
            untracked,
            status,
            no_status,
            template,
            include,
            quiet,
            only: _,
            verbose,
            no_verbose,
            patch,
            interactive: menu,
            sign,
            mut paths,
        } => {
            if menu {
                if !interactive {
                    anyhow::bail!("commit --interactive needs a terminal");
                }
                crate::add_interactive::run(backend, &paths)?;
                paths.clear();
            }
            if patch {
                crate::interactive::patch(
                    backend,
                    interactive,
                    crate::interactive::PatchMode::Add,
                    None,
                    &paths,
                )?;
                paths.clear();
            }
            let config_verbose = || {
                let v = backend.config_get("commit.verbose").ok().flatten()?;
                match v.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" => Some(1),
                    "false" | "no" | "off" => Some(0),
                    n => n.parse::<u8>().ok(),
                }
            };
            let verbose = match (no_verbose, verbose) {
                (true, _) => 0,
                (false, 0) => config_verbose().unwrap_or(0),
                (false, n) => n,
            };
            let mut opts = rgit_git::StatusOpts {
                untracked: untracked.clone(),
                verbose,
                amend,
                commit: true,
                commit_all: all,
                commit_include: include,
                commit_paths: paths.clone(),
                ..Default::default()
            };
            if dry_run || short || porcelain || long || null {
                use rgit_git::StatusFormat;
                opts.format = if short {
                    Some(StatusFormat::Short)
                } else if porcelain {
                    Some(StatusFormat::Porcelain)
                } else if long || !null {
                    Some(StatusFormat::Long)
                } else {
                    None
                };
                opts.branch = branch.then_some(true);
                opts.null = null;
                let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
                status_env(backend, &mut opts, tty && !render::color_on(), tty);
                let report = backend.status_text(&opts)?;
                set_exit(!report.committable);
                print_as_is();
                return Ok(String::from_utf8_lossy(&report.text).into_owned());
            }
            let reuse = reuse_message.as_ref().or(reedit_message.as_ref());
            let given = file.is_some() || reuse.is_some() || !message.is_empty();
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
            let fixing = target.is_some();
            if let Some((kind, rev)) = target {
                let old = backend.commit_overview(&rev)?.message;
                let head = format!("{kind}! {}", old.lines().next().unwrap_or(""));
                text = if text.is_empty() {
                    head
                } else {
                    format!("{head}\n\n{text}")
                };
            }
            // -e always opens the editor, as does a commit with no message
            // on a terminal; -c only on a terminal, as git would have no one
            // to edit for.
            let use_editor = edit
                || (!no_edit && reedit_message.is_some() && interactive)
                || (!given && !no_edit && !fixing && interactive);
            let git_dir = backend.git_dir();
            let read = |name: &str| std::fs::read_to_string(git_dir.join(name)).ok();
            let template =
                template.or_else(|| backend.config_get("commit.template").ok().flatten());
            let mut template_text = None;
            if text.is_empty() && amend {
                text = backend.head_message().unwrap_or_default();
            } else if text.is_empty() && !fixing {
                // Like git: a merge, squash or stopped pick prepared the message.
                match read("MERGE_MSG").or_else(|| read("SQUASH_MSG")) {
                    Some(m) => text = m,
                    None => {
                        if let Some(t) = &template {
                            let path = expand_home(t);
                            let t = std::fs::read_to_string(&path).map_err(|_| {
                                anyhow::anyhow!("could not read '{}'", path.display())
                            })?;
                            text = t.clone();
                            template_text = Some(t);
                        }
                    }
                }
            }
            if use_editor {
                let include_status = if status || no_status {
                    status
                } else {
                    backend
                        .config_get("commit.status")
                        .ok()
                        .flatten()
                        .is_none_or(|v| !matches!(v.as_str(), "false" | "no" | "off" | "0"))
                };
                let mut body = if template_text.is_some() {
                    text.clone()
                } else {
                    clean_message(&text, false)
                };
                if include_status {
                    let reuse_from = reuse.cloned().or(amend.then(|| "HEAD".to_owned()));
                    body.push_str(&commit_template_status(
                        backend,
                        &mut opts,
                        author.as_deref(),
                        reuse_from.filter(|_| !reset_author).as_deref(),
                        date.as_deref(),
                    )?);
                }
                text = crate::interactive::edit_message(backend, &body, verbose > 0)?;
                if text.is_empty() {
                    anyhow::bail!("Aborting commit due to empty commit message.");
                }
                if template_text.is_some_and(|t| clean_message(&t, true) == text) {
                    anyhow::bail!("Aborting commit; you did not edit the message.");
                }
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
            let committed = backend.commit_with(
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
                    sign: sign.gpg_sign,
                    no_sign: sign.no_gpg_sign,
                },
            );
            // git's commit prints the status on stdout when there is nothing to commit.
            if matches!(committed, Err(rgit_git::GitError::NothingToCommit)) && render::text_mode()
            {
                opts.format = Some(rgit_git::StatusFormat::Long);
                let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
                status_env(backend, &mut opts, tty && !render::color_on(), tty);
                set_exit(true);
                print_as_is();
                return Ok(String::from_utf8_lossy(&backend.status_text(&opts)?.text).into_owned());
            }
            committed?;
            if render::text_mode() {
                if quiet {
                    return Ok(String::new());
                }
                let show_date = (amend || reuse_message.is_some() || reedit_message.is_some())
                    && !reset_author
                    || date.is_some();
                return commit_summary(backend, "HEAD", show_date);
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
        Command::Prune {
            dry_run,
            verbose,
            expire,
        } => backend.prune_objects(expire.as_deref(), dry_run, verbose)?,
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
            jobs,
            append,
            filter,
        } => {
            let multiple = multiple.then(|| {
                let names = remote.iter().chain(&repository).chain(&refspecs);
                names.cloned().collect::<Vec<_>>()
            });
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
                remotes: multiple.clone().unwrap_or_default(),
                jobs: jobs.unwrap_or(0),
                append,
                filter,
            };
            let out = if multiple.is_some() {
                net(interactive, "fetch", |r| backend.fetch(None, &[], &args, r))?
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
            let old = head_or_zero(backend);
            let rebasing = args.rebase.unwrap_or_else(|| {
                let current = backend
                    .symbolic_ref("HEAD")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let key = format!(
                    "branch.{}.rebase",
                    current.trim_start_matches("refs/heads/")
                );
                [key.as_str(), "pull.rebase"]
                    .iter()
                    .find_map(|k| backend.config_get(k).ok().flatten())
                    .is_some_and(|v| !matches!(v.as_str(), "false" | "no" | "off" | "0"))
            });
            let out = net(interactive, "pull", |r| {
                backend.pull(repository.as_deref(), branch.as_deref(), &args, r)
            })?;
            if !rebasing && !no_commit && (squash || head_or_zero(backend) != old) {
                post_hook(
                    backend,
                    backend.workdir(),
                    "post-merge",
                    &[if squash { "1" } else { "0" }],
                )?;
            }
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
            recurse_submodules,
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
                recurse_submodules,
            };
            let out = net(interactive, "push", |r| {
                backend.push_to(remote.as_deref(), &refspecs, &args, r)
            })?;
            if quiet { String::new() } else { out }
        }
        Command::Checkout {
            pathspec_file: _,
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
            quiet,
            paths,
        } => {
            let old = head_or_zero(backend);
            let (rev, paths) = rev_and_paths(rev, pathspec, paths, |r| {
                r == "-" || backend.rev_parse(r).is_ok() || guess_remote(backend, r).is_some()
            });
            if patch {
                return crate::interactive::patch(
                    backend,
                    interactive,
                    crate::interactive::PatchMode::Checkout,
                    rev.as_deref(),
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
            if !paths.is_empty() && rev.is_none() && (merge || conflict.is_some()) {
                return merge_paths(backend, &paths, conflict.as_deref());
            }
            if !paths.is_empty() {
                // `checkout [<rev>] -- <paths>`: take the paths from <rev> into
                // the index and working tree, or from the index.
                backend.restore(&paths, rev.as_deref(), rev.is_some(), true, true)?;
                post_checkout(backend, &old, false)?;
                if render::text_mode() {
                    return Ok(String::new());
                }
                let from = rev.as_deref().unwrap_or("the index");
                return Ok(format!("restored {} from {from}", paths.join(" ")));
            }
            if let Some(name) = orphan {
                backend.checkout_orphan(&name, Some(rev.as_deref().unwrap_or("HEAD")))?;
                post_checkout(backend, &old, true)?;
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
                quiet,
            };
            let out = switch(backend, rev, new, &opts)?;
            post_checkout(backend, &old, true)?;
            out
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
            quiet,
        } => {
            let old = head_or_zero(backend);
            if let Some(name) = orphan {
                backend.checkout_orphan(&name, None)?;
                post_checkout(backend, &old, true)?;
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
                quiet,
            };
            let out = switch(backend, rev, new, &opts)?;
            post_checkout(backend, &old, true)?;
            out
        }
        Command::Merge {
            mut revs,
            sign,
            rerere,
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
            autostash,
            no_autostash,
            into_name,
            cleanup,
            quiet,
            cont,
            abort,
            quit,
        } => {
            if abort {
                ok(backend.merge_abort())?
            } else if cont {
                backend.merge_continue()?;
                "ok".to_owned()
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
                    sign: sign.gpg_sign,
                    no_sign: sign.no_gpg_sign,
                    autostash: (autostash || no_autostash).then_some(autostash),
                    into_name,
                    cleanup,
                    rerere_autoupdate: rerere.value(),
                };
                let old = head_or_zero(backend);
                let out = net(interactive, "merge", |r| {
                    backend.merge_with(&revs, &opts, r)
                })?;
                if !no_commit && (squash || head_or_zero(backend) != old) {
                    post_hook(
                        backend,
                        backend.workdir(),
                        "post-merge",
                        &[if squash { "1" } else { "0" }],
                    )?;
                }
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
            let done = |out: String| {
                if render::text_mode() {
                    // git reports the finished rebase on stderr.
                    let (done, rest): (Vec<&str>, Vec<&str>) = out
                        .lines()
                        .partition(|l| l.starts_with("Successfully rebased"));
                    for l in done {
                        eprintln!("{l}");
                    }
                    return rest.join("\n");
                }
                if out.is_empty() { "ok".to_owned() } else { out }
            };
            // A scripted sequence editor stands in for the terminal.
            let scripted = std::env::var("GIT_SEQUENCE_EDITOR").is_ok_and(|e| !e.is_empty());
            if abort {
                done(backend.rebase_abort()?)
            } else if cont {
                done(backend.rebase_continue()?)
            } else if skip {
                done(backend.rebase_skip()?)
            } else if quit {
                ok(backend.rebase_quit())?
            } else if edit_todo {
                if !interactive && !scripted {
                    anyhow::bail!("rebase --edit-todo needs a terminal");
                }
                ok(backend.rebase_edit_todo())?
            } else if show_current_patch {
                if backend.rev_parse("REBASE_HEAD").is_err() {
                    anyhow::bail!("no rebase in progress");
                }
                show_one(backend, "REBASE_HEAD", &[], DiffFormat::default(), false)?
            } else {
                if edit && !interactive && !scripted {
                    anyhow::bail!("interactive rebase needs a terminal");
                }
                // Pick the base (how far back to edit) when it is not given.
                let onto = match onto {
                    None if edit && !root && interactive => Some(crate::interactive::pick_commit(
                        backend,
                        "Rebase onto which commit? (edits the commits after it)",
                    )?),
                    onto => onto,
                };
                // No upstream argument means the branch's upstream.
                let opts = rgit_git::RebaseOptions {
                    onto: onto_new,
                    interactive: edit,
                    root,
                    autosquash,
                    exec,
                    update_refs,
                    strategy_option,
                    branch,
                    ..more.options()
                };
                let out = backend.rebase_with(onto.as_deref(), &opts)?;
                if more.quiet {
                    "ok".to_owned()
                } else {
                    done(out)
                }
            }
        }
        Command::Undo => format!("undid {}", backend.undo()?),
        Command::Redo => format!("redid {}", backend.redo()?),
        Command::Oplog => render::oplog(&backend.oplog()?),
        Command::Smartlog => render::smartlog(&backend.smartlog()?),
        Command::Bisect { cmd } => bisect(backend, cmd)?.0,
        Command::Rerere { args, rerere } => backend.rerere(&args, rerere.value())?,
        Command::Reset {
            pathspec_file: _,
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
            let dashes = !paths.is_empty();
            let (rev, paths) =
                rev_and_paths(rev, pathspec, paths, |r| backend.rev_parse(r).is_ok());
            if rev.is_none()
                && !dashes
                && let Some(p) = paths.first()
                && !backend.workdir().join(p).exists()
            {
                if let Err(e) = backend.rev_parse(p)
                    && crate::plumbing::rev_dies(&e.to_string())
                {
                    return Err(e.into());
                }
                return Err(CliError {
                    message: format!(
                        "ambiguous argument '{p}': unknown revision or path not in the working tree"
                    ),
                    help: Some("Put paths after `--`, e.g. `rgit reset -- <path>`".to_owned()),
                    code: 128,
                }
                .into());
            }
            if patch {
                return crate::interactive::patch(
                    backend,
                    interactive,
                    crate::interactive::PatchMode::Reset,
                    rev.as_deref(),
                    &paths,
                );
            }
            let done = |text: String| if quiet { String::new() } else { text };
            // `reset [<rev>] [--] <paths>` resets those index entries to <rev>
            // (default HEAD), leaving HEAD and the working tree alone.
            if !paths.is_empty() {
                let msg = match rev {
                    None => {
                        for p in &paths {
                            backend.unstage_file(p)?;
                        }
                        format!("unstaged {}", paths.join(", "))
                    }
                    Some(rev) => {
                        backend.reset_paths(&rev, &paths)?;
                        format!("reset {} to {rev}", paths.join(", "))
                    }
                };
                if render::text_mode() {
                    return Ok(done(unstaged_after_reset(backend)?));
                }
                return Ok(done(msg));
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
            } else if render::text_mode() {
                backend.reset(&rev, mode)?;
                done(match mode {
                    ResetMode::Soft => String::new(),
                    ResetMode::Mixed => unstaged_after_reset(backend)?,
                    _ => format!("HEAD is now at {}", short_subject(backend, "HEAD")?),
                })
            } else {
                done(ok(backend.reset(&rev, mode))?)
            }
        }
        Command::CherryPick {
            revs,
            sign,
            no_commit,
            record_origin,
            mainline,
            strategy_option,
            more,
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
                sign: sign.gpg_sign,
                no_sign: sign.no_gpg_sign,
                rerere_autoupdate: more.rerere.value(),
                strategy: more.strategy,
                cleanup: more.cleanup,
            };
            pick(backend, revs, &opts, (cont, skip, abort, quit), interactive)?
        }
        Command::Revert {
            revs,
            sign,
            no_commit,
            mainline,
            strategy_option,
            more,
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
                sign: sign.gpg_sign,
                no_sign: sign.no_gpg_sign,
                rerere_autoupdate: more.rerere.value(),
                strategy: more.strategy,
                cleanup: more.cleanup,
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
                let (p, paths) = match cmd {
                    Some(StashCmd::Push { push, paths }) => (push, paths),
                    _ => (push, paths),
                };
                if p.staged {
                    ok_msg(backend.stash_push_part(p.message.as_deref(), None, true, &paths))?
                } else if p.patch {
                    if !interactive {
                        return Err(CliError::usage(
                            "stash -p picks hunks on a terminal; stash whole files with `rgit stash push -- <path>`",
                        ));
                    }
                    let picked = crate::add_patch::run(
                        backend,
                        crate::add_patch::Kind::Stash,
                        None,
                        &paths,
                    )?;
                    if picked.is_empty() {
                        return Ok("No changes selected".to_owned());
                    }
                    ok_msg(backend.stash_push_part(
                        p.message.as_deref(),
                        Some(&picked),
                        !p.no_keep_index,
                        &paths,
                    ))?
                } else {
                    match backend.stash_push_opts(
                        p.message.as_deref(),
                        p.include_untracked,
                        p.all,
                        p.keep_index,
                        &paths,
                    ) {
                        Err(e)
                            if render::text_mode()
                                && e.to_string().contains("there is nothing to stash") =>
                        {
                            "No local changes to save".to_owned()
                        }
                        r => ok_msg(r)?,
                    }
                }
            }
            Some(StashCmd::Pop {
                index,
                restore_index,
                quiet,
            }) => {
                let i = stash_index(backend, index, interactive, "Pop which stash?")?;
                let dropped = stash_dropped(backend, i);
                let out = ok(backend.stash_apply_opts(i, restore_index, true))?;
                quietly(quiet, stash_applied(backend, out, dropped)?)
            }
            Some(StashCmd::Apply {
                index,
                restore_index,
                quiet,
            }) => {
                let i = stash_index(backend, index, interactive, "Apply which stash?")?;
                let out = ok(backend.stash_apply_opts(i, restore_index, false))?;
                quietly(quiet, stash_applied(backend, out, None)?)
            }
            Some(StashCmd::Drop { index, quiet }) => {
                let i = stash_index(backend, index, interactive, "Drop which stash?")?;
                let dropped = stash_dropped(backend, i);
                let out = ok(backend.stash_drop(i))?;
                quietly(quiet, dropped.unwrap_or(out))
            }
            Some(StashCmd::List {
                pretty,
                diff,
                max_count,
            }) => stash_log(backend, pretty, diff, max_count)?,
            Some(StashCmd::Show {
                index,
                patch,
                name_only,
                name_status,
                numstat,
                stat,
                include_untracked,
                only_untracked,
                patch_with_stat,
            }) => {
                let bool_config = |k: &str, default: bool| {
                    backend.config_get(k).ok().flatten().map_or(default, |v| {
                        matches!(v.to_lowercase().as_str(), "true" | "yes" | "on" | "1")
                    })
                };
                let mut format = DiffFormat {
                    patch: patch || patch_with_stat,
                    name_only,
                    name_status,
                    numstat,
                    stat: stat || patch_with_stat,
                    shortstat: false,
                    raw: false,
                };
                // git's defaults: stash.showStat and stash.showPatch.
                if !format.any() {
                    format.stat = bool_config("stash.showStat", true);
                    format.patch = bool_config("stash.showPatch", false);
                }
                let untracked = include_untracked
                    || !only_untracked && bool_config("stash.showIncludeUntracked", false);
                let files = stash_diff(
                    backend,
                    index.unwrap_or(0),
                    (!only_untracked, untracked || only_untracked),
                )?;
                diff_out(&files, format)
            }
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
                let lines = match &opts.format {
                    Some(fmt) => crate::plumbing::format_refs(backend, &tags, fmt)?,
                    None => tags
                        .iter()
                        .map(|t| tag_with_message(t, opts.lines))
                        .collect(),
                };
                let mut cols = colopts(backend, "tag", opts.column.as_deref(), opts.no_column)?;
                if opts.lines.is_some() {
                    if opts.column.is_some() && cols.active() {
                        return Err(CliError::usage(
                            "options '--column' and '-n' cannot be used together",
                        ));
                    }
                    cols.enable = render::ColEnable::Never;
                }
                render::columns(&lines, cols, "", 2)
            } else if opts.delete {
                if names.is_empty() {
                    return Err(CliError::usage("tag -d needs a tag name"));
                }
                let mut deleted = String::new();
                let failed: Vec<String> = names
                    .iter()
                    .filter_map(|n| {
                        let was = backend
                            .read_object(&format!("refs/tags/{n}"))
                            .and_then(|o| backend.abbrev_id(&o.id, 7))
                            .unwrap_or_default();
                        match backend.delete_tag(n) {
                            Ok(()) => {
                                deleted.push_str(&format!("Deleted tag '{n}' (was {was})\n"));
                                None
                            }
                            Err(e) => Some(format!("{n}: {e}")),
                        }
                    })
                    .collect();
                if !failed.is_empty() {
                    anyhow::bail!("could not delete {}", failed.join("; "));
                }
                if render::text_mode() {
                    deleted
                } else {
                    "ok".to_owned()
                }
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
                if lines.is_empty() && !render::text_mode() {
                    "no remotes".to_owned()
                } else {
                    lines.join("\n")
                }
            }
            None if render::text_mode() => backend
                .remotes()?
                .iter()
                .map(|r| format!("{}\n", r.name))
                .collect(),
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
                    remotes: targets,
                    ..rgit_git::FetchArgs::default()
                };
                net(interactive, "fetch", |r| backend.fetch(None, &[], &args, r))?
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
                    None => {
                        let (heads, head) = backend.remote_heads(&name)?;
                        match remote_head_guess(&heads, head).as_slice() {
                            [one] => one.clone(),
                            [] => anyhow::bail!("Cannot determine remote HEAD"),
                            many => anyhow::bail!(
                                "Multiple remote HEAD branches. Please choose one explicitly with:\n{}",
                                many.iter()
                                    .map(|b| format!("  git remote set-head {name} {b}"))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            ),
                        }
                    }
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
                guess_remote,
                no_guess_remote,
                quiet: _,
            }) => {
                let mut args = rgit_git::WorktreeAddArgs {
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
                let guess = !no_guess_remote
                    && (guess_remote
                        || backend
                            .config_get("worktree.guessRemote")?
                            .is_some_and(|v| matches!(v.as_str(), "true" | "yes" | "on" | "1")));
                let mut commitish = commitish;
                let base = Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if guess
                    && commitish.is_none()
                    && args.new_branch.is_none()
                    && !detach
                    && !orphan
                    && !backend.local_branches()?.contains(&base)
                    && let Some(remote) = self::guess_remote(backend, &base)
                {
                    args.new_branch = Some(base);
                    args.track = args.track.or(Some(true));
                    commitish = Some(remote);
                }
                backend.worktree_add(&path, commitish.as_deref(), &args)?;
                if !no_checkout {
                    let wt = rgit_git::Git2Backend::discover(&path)?;
                    let new = wt.rev_parse("HEAD").unwrap_or_else(|_| "0".repeat(40));
                    post_hook(
                        backend,
                        wt.workdir(),
                        "post-checkout",
                        &["0".repeat(40).as_str(), &new, "1"],
                    )?;
                }
                "ok".to_owned()
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
        Command::Config(args) => config(Some(&backend.git_dir()), args)?,
        Command::Apply(a) => apply(Some(backend.as_ref()), a)?,
        Command::Notes { notes_ref, cmd } => {
            let notes_ref = notes_ref.map_or_else(
                || crate::pretty::default_notes_ref(backend),
                |r| notes_ref_name(&r),
            );
            let r = Some(notes_ref.as_str());
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
                        None if notes.is_empty() && !render::text_mode() => "no notes".to_owned(),
                        None => notes
                            .into_iter()
                            .map(|(note, obj)| format!("{note} {obj}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    }
                }
                NotesCmd::Show { rev } => backend.note_show(r, &head(rev))?,
                NotesCmd::Add { rev, text, force } => {
                    let rev = head(rev);
                    let existing = backend.note_show(r, &rev).ok();
                    if existing.is_some() && !force {
                        return Err(GitError::Other(format!(
                            "Cannot add notes. Found existing notes for object {}. Use '-f' to overwrite existing notes",
                            backend.rev_parse(&rev)?
                        ))
                        .into());
                    }
                    let note = note_text(backend, &text, None, interactive)?;
                    write_note(backend, r, &rev, &note, text.allow_empty, "add")?
                }
                NotesCmd::Append { rev, text } => {
                    let rev = head(rev);
                    let note = note_text(backend, &text, None, interactive)?;
                    let oid = backend.rev_parse(&rev)?;
                    let old = match backend.notes(r)?.into_iter().find(|(_, o)| *o == oid) {
                        Some((id, _)) => {
                            String::from_utf8_lossy(&backend.read_object(&id)?.data).into_owned()
                        }
                        None => String::new(),
                    };
                    let joined = match (old.is_empty(), note.is_empty()) {
                        (false, false) => format!("{old}{}{note}", text.separator()),
                        _ => old + &note,
                    };
                    write_note(backend, r, &rev, &joined, text.allow_empty, "append")?
                }
                NotesCmd::Edit { rev, allow_empty } => {
                    let rev = head(rev);
                    let text = NoteMessage {
                        reedit: Some(String::new()),
                        ..Default::default()
                    };
                    let current = backend.note_show(r, &rev).unwrap_or_default();
                    let note = note_text(backend, &text, Some(current), interactive)?;
                    write_note(backend, r, &rev, &note, allow_empty, "edit")?
                }
                NotesCmd::Copy { from, to, force } => {
                    ok(backend.note_copy(r, &from, &head(to), force))?
                }
                NotesCmd::Remove {
                    revs,
                    ignore_missing,
                } => {
                    let revs = if revs.is_empty() {
                        vec!["HEAD".to_owned()]
                    } else {
                        revs
                    };
                    let removed = backend.note_remove(r, &revs, !ignore_missing, "remove")?;
                    for (rev, had) in revs.iter().zip(&removed) {
                        if *had {
                            eprintln!("Removing note for object {rev}");
                        } else {
                            eprintln!("Object {rev} has no note");
                        }
                    }
                    if !ignore_missing && removed.contains(&false) {
                        return Err(anyhow::Error::new(CliError {
                            message: String::new(),
                            help: None,
                            code: 1,
                        }));
                    }
                    "ok".to_owned()
                }
                NotesCmd::Prune { dry_run, verbose } => {
                    let gone = backend.notes_prune(r, dry_run)?;
                    if dry_run || verbose {
                        gone.iter().map(|g| format!("{g}\n")).collect()
                    } else {
                        "ok".to_owned()
                    }
                }
                NotesCmd::Merge {
                    notes_ref: other,
                    strategy,
                    commit,
                    abort: _,
                    verbose,
                    quiet,
                } => {
                    let verbosity = (2 + verbose).saturating_sub(u8::from(quiet));
                    // git dies (128) on these, but a failed --abort is an error (1).
                    let fatal = |e: GitError| {
                        anyhow::Error::new(CliError {
                            message: e.to_string(),
                            help: None,
                            code: if commit || other.is_some() { 128 } else { 1 },
                        })
                    };
                    let Some(other) = other.clone() else {
                        return backend.notes_merge_finish(commit, verbosity).map_err(fatal);
                    };
                    let local = notes_ref.as_str();
                    let short = local.strip_prefix("refs/notes/").unwrap_or(local);
                    let strategy = match strategy {
                        Some(s) => s,
                        None => backend
                            .config_get(&format!("notes.{short}.mergeStrategy"))?
                            .or(backend.config_get("notes.mergeStrategy")?)
                            .unwrap_or_else(|| "manual".to_owned()),
                    };
                    let (out, stop) = backend
                        .notes_merge(local, &notes_ref_name(&other), &strategy, verbosity)
                        .map_err(fatal)?;
                    if let Some((message, code)) = stop {
                        eprintln!("{message}");
                        EXIT_CODE.store(code, std::sync::atomic::Ordering::Relaxed);
                    }
                    out
                }
                NotesCmd::GetRef => notes_ref.clone(),
            }
        }
        Command::UpdateRef {
            name,
            new,
            old,
            delete,
            no_deref,
            message,
            create_reflog,
            stdin,
            z,
            batch_updates,
        } => {
            if stdin {
                return update_ref_stdin(
                    backend,
                    z,
                    message.as_deref(),
                    no_deref,
                    create_reflog,
                    batch_updates,
                );
            }
            let name = name.ok_or_else(|| CliError::usage("a ref name required"))?;
            let update = if delete {
                if old.is_some() {
                    return Err(CliError::usage("-d takes a ref and an optional old value"));
                }
                rgit_git::RefUpdate {
                    name,
                    old: new,
                    ..Default::default()
                }
            } else {
                let new = new.ok_or_else(|| CliError::usage("a new value required"))?;
                rgit_git::RefUpdate {
                    name,
                    new: Some(new),
                    old,
                    ..Default::default()
                }
            };
            backend.update_refs(
                &[update],
                message.as_deref(),
                no_deref,
                create_reflog,
                false,
                false,
            )?;
            "ok".to_owned()
        }
        Command::HashObject(a) => hash_object(Some(backend.as_ref()), a)?,
        Command::FormatPatch(a) => {
            let a = *a;
            let (counts, ranges): (Vec<&String>, Vec<&String>) = a.revs.iter().partition(|r| {
                r.strip_prefix('-')
                    .is_some_and(|n| n.parse::<usize>().is_ok())
            });
            let count = counts.last().and_then(|n| n[1..].parse().ok());
            if ranges.len() > 1 || (ranges.is_empty() && count.is_none()) {
                return Err(CliError::usage(
                    "give `-<n>`, a `<since>` revision, or one `<a>..<b>` range",
                ));
            }
            let cfg = |k: &str| backend.config_get(k).ok().flatten();
            let cfg_bool = |k: &str| cfg(k).and_then(|v| maybe_bool(&v));
            let signature_file = a
                .signature_file
                .clone()
                .or_else(|| cfg("format.signatureFile"));
            let signature = match (&signature_file, a.no_signature) {
                (_, true) => Some(String::new()),
                _ if a.signature.is_some() => a.signature.clone(),
                // format.signature beats a signature file, as in git.
                (Some(f), _) if cfg("format.signature").is_none() => {
                    Some(String::from_utf8(read_input(f)?)?)
                }
                _ => None,
            };
            let numbered = match (a.numbered || a.no_numbered, cfg("format.numbered")) {
                (true, _) => Some(a.numbered),
                (false, Some(v)) if v != "auto" => maybe_bool(&v),
                _ => None,
            };
            let thread = match (&a.thread, a.no_thread, cfg("format.thread")) {
                (Some(t), ..) => Some(t.clone()),
                (None, false, Some(v)) => match maybe_bool(&v) {
                    Some(false) => None,
                    Some(true) => Some("shallow".to_owned()),
                    None => Some(v),
                },
                _ => None,
            };
            let cover = cfg("format.coverLetter");
            let from = a.from.clone().or_else(|| {
                cfg("format.from").and_then(|v| match maybe_bool(&v) {
                    Some(false) => None,
                    Some(true) => Some(String::new()),
                    None => Some(v),
                })
            });
            // --attach, --inline and --no-attach: the last one given decides.
            let last = last_flag(&["--attach", "--inline", "--no-attach"]);
            let inline = a.inline.is_some() && last.as_deref() != Some("--attach");
            let attach = match (&a.attach, &a.inline, a.no_attach) {
                (_, _, true) if last.as_deref() == Some("--no-attach") => None,
                (Some(b), Some(i), _) => Some(if inline { i.clone() } else { b.clone() }),
                (Some(b), None, _) | (None, Some(b), _) => Some(b.clone()),
                (None, None, _) => cfg("format.attach").filter(|b| !b.is_empty()),
            };
            // format.notes, then --no-notes and --notes, as git's display
            // notes options.
            let (mut show_notes, mut use_default, mut extra) = (false, None, Vec::new());
            for v in backend
                .config_entries(rgit_git::ConfigScope::Any, Some("format.notes"))
                .unwrap_or_default()
                .into_iter()
                .map(|(_, v)| v)
                .chain(a.no_notes.then(|| "false".to_owned()))
                .chain(a.notes.iter().map(|n| {
                    if n.is_empty() {
                        "true".to_owned()
                    } else {
                        n.clone()
                    }
                }))
            {
                match maybe_bool(&v) {
                    Some(true) => (show_notes, use_default) = (true, Some(true)),
                    Some(false) => {
                        (show_notes, use_default, extra) = (false, Some(false), Vec::new())
                    }
                    None => {
                        show_notes = true;
                        extra.push(notes_ref_name(&v));
                    }
                }
            }
            let notes = if show_notes {
                crate::pretty::display_notes_refs(backend, use_default, &extra)
            } else {
                Vec::new()
            };
            let opts = rgit_git::FormatPatchOpts {
                range: ranges.first().map(|r| r.to_string()),
                count,
                numbered,
                subject_prefix: a.subject_prefix,
                reroll: a.reroll,
                rfc: a.rfc,
                keep_subject: a.keep_subject,
                cover_letter: a.cover_letter
                    || (!a.no_cover_letter && cover.as_deref().and_then(maybe_bool) == Some(true)),
                cover_letter_auto: !a.cover_letter
                    && !a.no_cover_letter
                    && cover.as_deref() == Some("auto"),
                thread: thread.as_deref().map(|t| match t {
                    "deep" => rgit_git::Thread::Deep,
                    _ => rgit_git::Thread::Shallow,
                }),
                in_reply_to: a.in_reply_to,
                to: a.to,
                cc: a.cc,
                headers: a.add_header,
                base: a.base,
                zero_commit: a.zero_commit,
                start_number: a.start_number,
                signature,
                no_stat: a.no_stat,
                numbered_files: a.numbered_files,
                suffix: a.suffix,
                root: a.root,
                signoff: a.signoff || cfg_bool("format.signOff") == Some(true),
                from,
                filename_max_length: a
                    .filename_max_length
                    .or_else(|| cfg("format.filenameMaxLength").and_then(|v| v.parse().ok())),
                no_binary: a.no_binary,
                ignore_if_in_upstream: a.ignore_if_in_upstream,
                cover_from_description: a.cover_from_description,
                description: match &a.description_file {
                    Some(f) => Some(String::from_utf8(read_input(f)?)?),
                    None => None,
                },
                interdiff: a.interdiff,
                range_diff: a.range_diff,
                creation_factor: a.creation_factor,
                attach,
                inline,
                notes,
                no_encode_headers: a.no_encode_email_headers
                    || (!a.encode_email_headers
                        && cfg_bool("format.encodeEmailHeaders") == Some(false)),
                force_in_body_from: a.force_in_body_from
                    || (!a.no_force_in_body_from
                        && cfg_bool("format.forceInBodyFrom") == Some(true)),
                no_to: a.no_to,
                no_cc: a.no_cc,
            };
            let mails = backend.format_patch(&opts)?;
            if mails.is_empty() && a.stdout {
                return Ok(String::new());
            }
            if mails.is_empty() {
                return Ok("no commits to format".to_owned());
            }
            if a.stdout {
                return Ok(rgit_git::mbox(&mails));
            }
            if let Some(file) = &a.output {
                std::fs::write(file, rgit_git::mbox(&mails))?;
                return Ok(String::new());
            }
            let dir = PathBuf::from(
                a.output_dir
                    .or_else(|| cfg("format.outputDirectory"))
                    .unwrap_or_default(),
            );
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(&dir)?;
            }
            let mut written = String::new();
            for m in mails {
                let path = dir.join(&m.name);
                std::fs::write(&path, m.text)?;
                written.push_str(&format!("{}\n", path.display()));
            }
            if a.quiet { String::new() } else { written }
        }
        Command::Am(a) => {
            let resume = a.abort
                || a.cont
                || a.skip
                || a.quit
                || a.allow_empty
                || a.show_current_patch.is_some()
                || backend.git_dir().join("rebase-apply").is_dir();
            let mut args: Vec<String> = a.strip.map(|n| format!("-p{n}")).into_iter().collect();
            for (on, flag) in [
                (a.abort, "--abort"),
                (a.cont, "--continue"),
                (a.skip, "--skip"),
                (a.quit, "--quit"),
                (a.allow_empty, "--allow-empty"),
                (a.keep_cr, "--keep-cr"),
                (a.no_keep_cr, "--no-keep-cr"),
                (a.three_way, "--3way"),
                (a.signoff, "--signoff"),
                (a.keep, "--keep"),
                (a.keep_non_patch, "--keep-non-patch"),
                (a.message_id, "--message-id"),
                (a.scissors, "--scissors"),
                (a.no_scissors, "--no-scissors"),
                (
                    a.committer_date_is_author_date,
                    "--committer-date-is-author-date",
                ),
                (a.ignore_date, "--ignore-date"),
                (a.interactive, "--interactive"),
                (a.no_verify, "--no-verify"),
                (a.reject, "--reject"),
                (a.quiet, "--quiet"),
            ] {
                args.extend(on.then(|| flag.to_owned()));
            }
            for (flag, value) in [
                ("--show-current-patch", a.show_current_patch),
                ("--empty", a.empty),
                ("--directory", a.directory),
                ("--whitespace", a.whitespace),
                ("--patch-format", a.patch_format),
            ] {
                args.extend(value.map(|v| format!("{flag}={v}")));
            }
            args.extend(a.rerere.value().map(|on| {
                let no = if on { "" } else { "no-" };
                format!("--{no}rerere-autoupdate")
            }));
            args.extend(a.sign.gpg_sign.map(|k| format!("--gpg-sign={k}")));
            args.extend(a.sign.no_gpg_sign.then(|| "--no-gpg-sign".to_owned()));
            for g in a.exclude {
                args.push(format!("--exclude={g}"));
            }
            for g in a.include {
                args.push(format!("--include={g}"));
            }
            let input = if !resume && a.mbox.is_empty() {
                Some(read_input("-")?)
            } else {
                None
            };
            for m in &a.mbox {
                args.push(std::path::absolute(m)?.display().to_string());
            }
            let out = backend.am(&args, input.as_deref())?;
            if out.is_empty() { "ok".to_owned() } else { out }
        }
        Command::Archive(a) => archive_cmd(Some(backend), a)?,
        Command::Gc {
            prune,
            no_prune,
            aggressive,
            auto,
            force,
            keep_largest_pack,
            cruft,
            no_cruft,
            quiet,
        } => backend.gc(&rgit_git::GcOptions {
            prune,
            no_prune,
            aggressive,
            auto,
            force,
            keep_largest_pack,
            cruft: (cruft || no_cruft).then_some(cruft),
            quiet,
        })?,
        Command::VerifyCommit {
            verbose,
            raw,
            commits,
        } => verify_signatures(backend, &commits, false, verbose, raw),
        Command::VerifyTag { verbose, raw, tags } => {
            verify_signatures(backend, &tags, true, verbose, raw)
        }
        c @ Command::Fsck { .. } => {
            let report = fsck(backend, c)?;
            eprint!("{}", report.stderr);
            report.stdout
        }
        Command::Repack {
            all,
            all_loosen,
            delete,
            keep_unreachable,
            cruft,
            keep_pack,
            cruft_expiration,
            unpack_unreachable,
            no_update_server_info,
            quiet,
            no_reuse_delta,
            no_reuse_object,
            write_bitmap_index,
            no_write_bitmap_index,
            window,
            depth,
            window_memory,
            threads,
            ..
        } => {
            let date = |d: Option<String>| -> anyhow::Result<Option<i64>> {
                d.map(|d| {
                    rgit_git::expiry_date(&d)
                        .ok_or_else(|| anyhow::anyhow!("malformed expiration date '{d}'"))
                })
                .transpose()
            };
            let out = backend.repack(&rgit_git::RepackOptions {
                all,
                all_loosen,
                delete,
                keep_unreachable,
                cruft,
                cruft_expiration: date(cruft_expiration)?,
                unpack_unreachable: date(unpack_unreachable)?,
                keep_pack,
                no_update_server_info,
                no_reuse_delta,
                no_reuse_object,
                window,
                depth,
                window_memory,
                threads,
                write_bitmap: (write_bitmap_index || no_write_bitmap_index)
                    .then_some(write_bitmap_index),
            })?;
            if quiet { String::new() } else { out }
        }
        Command::Hook {
            cmd:
                HookCmd::Run {
                    ignore_missing,
                    to_stdin,
                    name,
                    args,
                },
        } => {
            set_exit_code(hook_run(
                backend,
                ignore_missing,
                to_stdin.as_deref(),
                &name,
                &args,
            ));
            String::new()
        }
        Command::PackRefs {
            all,
            no_prune,
            auto,
        } => {
            backend.pack_refs(all, no_prune, auto)?;
            String::new()
        }
        Command::CommitGraph { cmd } => {
            let op = match cmd {
                CommitGraphCmd::Write {
                    reachable,
                    stdin_packs,
                    stdin_commits,
                    append,
                    split,
                    size_multiple,
                    max_commits,
                    expire_time,
                    changed_paths,
                    no_changed_paths,
                    ..
                } => {
                    let words = || -> anyhow::Result<Vec<String>> {
                        let input = read_input("-")?;
                        Ok(String::from_utf8_lossy(&input)
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect())
                    };
                    rgit_git::CommitGraphOp::Write(rgit_git::CommitGraphWrite {
                        reachable,
                        commits: if stdin_commits { Some(words()?) } else { None },
                        packs: if stdin_packs { Some(words()?) } else { None },
                        append,
                        split: split.map(|s| match s.as_str() {
                            "no-merge" => rgit_git::CommitGraphSplit::NoMerge,
                            "replace" => rgit_git::CommitGraphSplit::Replace,
                            _ => rgit_git::CommitGraphSplit::Merge,
                        }),
                        size_multiple,
                        max_commits,
                        expire_time: expire_time
                            .map(|d| {
                                rgit_git::expiry_date(&d)
                                    .ok_or_else(|| anyhow::anyhow!("malformed date '{d}'"))
                            })
                            .transpose()?,
                        changed_paths: (changed_paths || no_changed_paths).then_some(changed_paths),
                    })
                }
                CommitGraphCmd::Verify { shallow, .. } => {
                    rgit_git::CommitGraphOp::Verify { shallow }
                }
            };
            complaints(backend.commit_graph(&op)?)
        }
        Command::MultiPackIndex { cmd } => {
            let errors = backend.multi_pack_index(&match cmd {
                MidxCmd::Write { preferred_pack, .. } => rgit_git::MidxOp::Write { preferred_pack },
                MidxCmd::Verify { .. } => rgit_git::MidxOp::Verify,
                MidxCmd::Expire { .. } => rgit_git::MidxOp::Expire,
                MidxCmd::Repack { batch_size, .. } => rgit_git::MidxOp::Repack { batch_size },
            })?;
            complaints(errors)
        }
        Command::Maintenance { cmd } => crate::maintenance::run(backend, cmd)?,
        Command::Cherry {
            upstream,
            head,
            limit,
            verbose,
        } => {
            let upstream = upstream.unwrap_or_else(|| "@{upstream}".to_owned());
            let head = head.unwrap_or_else(|| "HEAD".to_owned());
            backend
                .cherry(&upstream, &head, limit.as_deref())?
                .iter()
                .map(|c| {
                    let sign = if c.upstream_has_it { '-' } else { '+' };
                    if verbose {
                        format!("{sign} {} {}\n", c.id, c.subject)
                    } else {
                        format!("{sign} {}\n", c.id)
                    }
                })
                .collect()
        }
        Command::Bundle { cmd } => bundle(Some(backend), cmd)?,
        Command::RequestPull {
            start,
            url,
            end,
            patch,
        } => {
            let (text, warnings) = backend.request_pull(&start, &url, end.as_deref(), patch)?;
            if warnings.is_empty() {
                text
            } else {
                // As git: the summary still prints, the warnings go to stderr
                // and the status is 1.
                for w in &warnings {
                    eprintln!("{w}");
                }
                print!("{text}");
                return Err(anyhow::Error::new(CliError {
                    message: String::new(),
                    help: None,
                    code: 1,
                }));
            }
        }
        Command::RangeDiff {
            revs,
            creation_factor,
            no_patch,
            left_only,
            right_only,
            unified,
            notes,
            no_notes,
            paths,
            ..
        } => {
            // `--notes=<ref>` shows only the refs named; a bare --notes (an
            // empty entry) adds the default ref back.
            let notes = if notes.iter().any(|n| !n.is_empty()) {
                Some(notes)
            } else if no_notes {
                Some(Vec::new())
            } else {
                None
            };
            let (range1, range2) = match revs.as_slice() {
                [base, old, new] => (format!("{base}..{old}"), format!("{base}..{new}")),
                [r1, r2] if r1.contains("..") && r2.contains("..") => (r1.clone(), r2.clone()),
                [sym] if sym.contains("...") => {
                    let (l, r) = sym.split_once("...").unwrap_or_default();
                    let (l, r) = (
                        if l.is_empty() { "HEAD" } else { l },
                        if r.is_empty() { "HEAD" } else { r },
                    );
                    (format!("{r}..{l}"), format!("{l}..{r}"))
                }
                _ => {
                    return Err(CliError::usage(
                        "give <base> <old> <new>, two ranges, or <old>...<new>",
                    ));
                }
            };
            backend.range_diff(&rgit_git::RangeDiffOpts {
                range1,
                range2,
                creation_factor,
                patches: !no_patch,
                left_only,
                right_only,
                context: unified,
                paths,
                notes,
            })?
        }
        Command::Difftool(a) => difftool(backend, a, interactive)?,
        Command::Mergetool(a) => mergetool(backend, a, interactive)?,
        Command::Clean {
            dry_run,
            ignored_too,
            only_ignored,
            exclude,
            dirs,
            force,
            interactive: menu,
            quiet,
            paths,
        } => crate::clean::run(
            backend,
            rgit_git::CleanOptions {
                dirs,
                ignored: ignored_too,
                only_ignored,
                exclude,
                dry_run,
                paths,
                ..Default::default()
            },
            force,
            menu,
            quiet,
        )?,
        Command::Rm {
            pathspec_file: _,
            paths,
            cached,
            recursive,
            force,
            dry_run,
            quiet,
            ignore_unmatch,
            sparse,
        } => {
            let mut paths = paths;
            let refused = if sparse {
                Vec::new()
            } else {
                sparse_refused(backend, &mut paths, false)?
            };
            if !refused.is_empty() && paths.is_empty() {
                return Err(sparse_advice(&refused));
            }
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
            if !refused.is_empty() {
                for p in removed.iter().filter(|_| !quiet) {
                    println!("rm '{p}'");
                }
                return Err(sparse_advice(&refused));
            }
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
            sparse,
        } => {
            let (to, from) = paths.split_last().expect("clap requires two paths");
            let git_dir = backend.git_dir();
            let into = |f: &str| {
                if backend.workdir().join(to).is_dir() {
                    let name = f.rsplit('/').next().unwrap_or(f);
                    format!("{}/{name}", to.trim_end_matches('/'))
                } else {
                    to.clone()
                }
            };
            let mut refused = Vec::new();
            for f in from {
                if rgit_git::outside_sparse(&git_dir, f)? {
                    refused.push(f.clone());
                } else if rgit_git::outside_sparse(&git_dir, &into(f))? {
                    refused.push(into(f));
                }
            }
            if !refused.is_empty() && !sparse {
                return Err(sparse_advice(&refused));
            }
            if from.len() > 1 && !backend.workdir().join(to).is_dir() {
                anyhow::bail!("destination '{to}' is not a directory");
            }
            let mut out = Vec::new();
            for f in from {
                let file =
                    backend.workdir().join(f).is_file() || !backend.workdir().join(f).exists();
                if !refused.is_empty() && file && !dry_run {
                    rgit_git::sparse_mv(&git_dir, f, &into(f))?;
                    if verbose {
                        out.push(format!("Renaming {f} to {}", into(f)));
                    }
                    continue;
                }
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
            all,
            dirty,
            broken,
            long,
            abbrev,
            always,
            first_parent,
            candidates,
            pattern,
            exclude,
            exact_match,
            contains,
        } => {
            let fatal = |message: String| CliError {
                message,
                help: None,
                code: 128,
            };
            if rev.is_some() && (dirty.is_some() || broken.is_some()) {
                let flag = if broken.is_some() {
                    "--broken"
                } else {
                    "--dirty"
                };
                return Err(fatal(format!(
                    "option '{flag}' and commit-ishes cannot be used together"
                ))
                .into());
            }
            if long && abbrev == Some(0) {
                return Err(fatal(
                    "options '--long' and '--abbrev=0' cannot be used together".to_owned(),
                )
                .into());
            }
            let rev = rev.as_deref().unwrap_or("HEAD");
            if contains {
                let id = backend.rev_parse(rev)?;
                match describe_contains(backend, &id, &pattern, &exclude)? {
                    Some(name) => name,
                    None if always => backend.abbrev_id(&id, 0)?,
                    None => {
                        return Err(CliError {
                            message: format!("cannot describe '{id}'"),
                            help: Some(
                                "Run `rgit describe --contains --always` for its id".to_owned(),
                            ),
                            code: 128,
                        }
                        .into());
                    }
                }
            } else {
                let opts = rgit_git::DescribeOptions {
                    all,
                    tags,
                    long,
                    always,
                    abbrev,
                    first_parent,
                    candidates: if exact_match {
                        0
                    } else {
                        candidates.unwrap_or(10)
                    },
                    matches: pattern,
                    excludes: exclude,
                    dirty,
                    broken,
                };
                backend
                    .describe(rev, &opts)
                    .map_err(|e| fatal(e.to_string()))?
            }
        }
        Command::Submodule { cmd } => submodule(backend, cmd, interactive)?,
        command @ Command::LsRemote { .. } => {
            crate::plumbing::ls_remote(Some(backend.workdir()), command, true)?.text
        }
        Command::Git { args } => backend.git(&args)?,
        Command::Init { .. }
        | Command::Clone { .. }
        | Command::ForEachRepo { .. }
        | Command::Credential { .. }
        | Command::CredentialStore { .. }
        | Command::CredentialCache { .. }
        | Command::CredentialCacheDaemon { .. }
        | Command::Scalar { .. }
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
            jobs,
            ..
        }) => (
            Op::Update {
                paths,
                init,
                recursive,
                remote,
                jobs,
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
    // An empty summary prints nothing, as in git.
    let empty = matches!(op, Op::Summary { .. }) && out == "ok";
    Ok(if quiet || empty { String::new() } else { out })
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
    let top = std::fs::canonicalize(&top).unwrap_or(top);
    let mut out = String::new();
    let all = backend.submodules(recursive)?;
    for s in &all {
        if s.state == '-' {
            continue;
        }
        if !quiet {
            out.push_str(&format!("Entering '{}'\n", s.path));
        }
        // A nested submodule's $toplevel and $sm_path are its immediate
        // superproject's, as in git.
        let parent = all
            .iter()
            .filter(|p| s.path.starts_with(&format!("{}/", p.path)))
            .max_by_key(|p| p.path.len());
        let (toplevel, sm_path) = match parent {
            Some(p) => (top.join(&p.path), &s.path[p.path.len() + 1..]),
            None => (top.clone(), s.path.as_str()),
        };
        // As in git, only a single-argument command sees the variables.
        let mut cmd = match command {
            [one] => {
                let quoted = format!("'{}'", sm_path.replace('\'', r"'\''"));
                let mut sh = std::process::Command::new("sh");
                sh.args(["-c", &format!("path={quoted}; {one}")])
                    .env("name", &s.name)
                    .env("sm_path", sm_path)
                    .env("displaypath", &s.path)
                    .env("sha1", s.recorded.as_deref().unwrap_or_default())
                    .env("toplevel", &toplevel);
                sh
            }
            [program, args @ ..] => {
                let mut c = std::process::Command::new(program);
                c.args(args);
                c
            }
            [] => anyhow::bail!("foreach needs a command"),
        };
        let result = cmd.current_dir(top.join(&s.path)).output()?;
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
    let old = backend.rev_parse("HEAD").unwrap_or_default();
    let out = ok(backend.pick(&revs, opts))?;
    if render::text_mode() && !opts.no_commit {
        return pick_summaries(backend, &old);
    }
    Ok(out)
}

/// The bytes of a file, or of stdin for `-`.
/// `rgit config`, in a repository (`git_dir`) or outside one.
pub fn config(git_dir: Option<&Path>, mut a: ConfigArgs) -> anyhow::Result<String> {
    use rgit_git::{ConfigScope, SetMode};
    // git 2.46's verb forms: `config get|set|unset|list|edit|rename-section|
    // remove-section ...` map onto the flag forms (keys always have a dot).
    let mut verb_get = false;
    if let Some(verb) = a.key.clone().filter(|k| !k.contains('.')) {
        let args = [a.value.take(), a.value_pattern.take()];
        let [first, second] = args;
        let known = matches!(
            verb.as_str(),
            "get" | "set" | "unset" | "list" | "edit" | "rename-section" | "remove-section"
        );
        if known {
            a.key = first;
            a.value = second;
            match verb.as_str() {
                "list" => a.list = true,
                "edit" => a.edit = true,
                "rename-section" => a.rename_section = true,
                "remove-section" => a.remove_section = true,
                "get" => {
                    a.get = true;
                    verb_get = true;
                    a.value = a.value_filter.take();
                }
                "set" => {
                    a.replace_all = a.all;
                    a.value_pattern = a.value_filter.take();
                }
                _ => {
                    a.unset_all = a.all;
                    a.unset = !a.all;
                    a.value = a.value_filter.take();
                }
            }
        } else {
            a.value = first;
            a.value_pattern = second;
        }
    }
    let mut scopes: Vec<ConfigScope> = [
        (a.system, ConfigScope::System),
        (a.global, ConfigScope::Global),
        (a.local, ConfigScope::Local),
        (a.worktree, ConfigScope::Worktree),
    ]
    .into_iter()
    .filter_map(|(on, s)| on.then_some(s))
    .chain(a.file.map(|f| ConfigScope::File(f.into())))
    .collect();
    if scopes.len() > 1 {
        return Err(CliError::usage("only one config file at a time"));
    }
    let scope = scopes.pop().unwrap_or_default();
    let includes = if a.no_includes {
        false
    } else {
        a.includes || scope == ConfigScope::Any
    };
    let kind = a.kind.as_deref().or_else(|| {
        [
            (a.as_bool, "bool"),
            (a.as_int, "int"),
            (a.as_bool_or_int, "bool-or-int"),
            (a.as_path, "path"),
            (a.as_expiry_date, "expiry-date"),
        ]
        .into_iter()
        .find_map(|(on, k)| on.then_some(k))
    });
    let typed = |v: Option<&str>| -> anyhow::Result<String> {
        Ok(match kind {
            Some(k) => rgit_git::config_typed(v, k)?,
            None => v.unwrap_or_default().to_owned(),
        })
    };
    let fixed = |p: String| {
        if a.fixed_value {
            rgit_git::config_fixed_value(&p)
        } else {
            p
        }
    };
    let (end, sep) = if a.null { ('\0', '\0') } else { ('\n', '\t') };
    let prefix = |e: &rgit_git::ConfigEntry| {
        let mut s = String::new();
        if a.show_scope {
            s.push_str(e.scope);
            s.push(sep);
        }
        if a.show_origin && e.scope == "command" && e.origin.is_empty() {
            s.push_str("command line:");
            s.push(sep);
        } else if a.show_origin {
            s.push_str(&format!("file:{}", e.origin));
            s.push(sep);
        }
        s
    };
    let key_required = || {
        a.key
            .clone()
            .ok_or_else(|| CliError::usage("a config key required"))
    };
    let write_scope = || match &scope {
        ConfigScope::Any => ConfigScope::Local,
        s => s.clone(),
    };
    if a.edit {
        run_editor(git_dir, &rgit_git::config_file(git_dir, &write_scope())?)?;
        return Ok(String::new());
    }
    if a.rename_section || a.remove_section {
        let section = key_required()?;
        let new = if a.rename_section {
            Some(
                a.value
                    .ok_or_else(|| CliError::usage("--rename-section <old> <new>"))?,
            )
        } else {
            None
        };
        rgit_git::config_section(git_dir, &write_scope(), &section, new.as_deref())?;
        return Ok("ok".to_owned());
    }
    if a.list || (a.key.is_none() && !a.get && !a.get_all && !a.get_regexp) {
        if !a.list {
            return Err(CliError::usage("a config key required (or --list)"));
        }
        let mut out = String::new();
        for e in rgit_git::config_list(git_dir, &scope, includes)? {
            out.push_str(&prefix(&e));
            out.push_str(&e.name);
            match &e.value {
                Some(v) if !a.name_only => {
                    out.push(if a.null { '\n' } else { '=' });
                    out.push_str(v);
                }
                _ => {}
            }
            out.push(end);
        }
        return Ok(out);
    }
    let key = key_required()?;
    if a.unset || a.unset_all {
        rgit_git::config_unset(
            git_dir,
            &write_scope(),
            &key,
            a.value.map(fixed).as_deref(),
            a.unset_all,
        )?;
        return Ok("ok".to_owned());
    }
    let reading = a.get || a.get_all || a.get_regexp || a.value.is_none();
    if !reading {
        let value = a.value.clone().unwrap_or_default();
        let value = match kind {
            // git stores canonical booleans and numbers.
            Some(k @ ("bool" | "int" | "bool-or-int")) => rgit_git::config_typed(Some(&value), k)?,
            Some(k) => {
                rgit_git::config_typed(Some(&value), k)?;
                value
            }
            None => value,
        };
        let mode = if a.add {
            SetMode::Add
        } else if a.replace_all {
            SetMode::ReplaceAll
        } else {
            SetMode::Replace
        };
        rgit_git::config_set(
            git_dir,
            &write_scope(),
            &key,
            &value,
            a.value_pattern.map(fixed).as_deref(),
            mode,
        )?;
        return Ok("ok".to_owned());
    }
    // `get` prints values only unless --show-names; --get-regexp prints both.
    let (by_regex, many, names) = if verb_get {
        (a.regexp, a.all, a.show_names)
    } else {
        (a.get_regexp, a.get_all || a.get_regexp, a.get_regexp)
    };
    let matches_value = rgit_git::value_matcher(a.value.map(fixed).as_deref())?;
    let entries = rgit_git::config_list(git_dir, &scope, includes)?;
    let mut hits: Vec<&rgit_git::ConfigEntry> = if by_regex {
        let matches = rgit_git::config_name_matcher(&key)?;
        entries.iter().filter(|e| matches(&e.name)).collect()
    } else {
        let name = rgit_git::config_key(&key)?;
        entries.iter().filter(|e| e.name == name).collect()
    };
    hits.retain(|e| matches_value(e.value.as_deref().unwrap_or_default()));
    if hits.is_empty() {
        return match a.default {
            Some(d) => Ok(format!("{}{end}", typed(Some(&d))?)),
            None => Err(GitError::Other(format!("{key} is not set")).into()),
        };
    }
    if !many {
        hits.drain(..hits.len() - 1);
    }
    let mut out = String::new();
    for e in hits {
        out.push_str(&prefix(e));
        if names {
            out.push_str(&e.name);
            if !a.name_only && e.value.is_some() {
                out.push(if a.null { '\n' } else { ' ' });
                out.push_str(&typed(e.value.as_deref())?);
            }
        } else {
            out.push_str(&typed(e.value.as_deref())?);
        }
        out.push(end);
    }
    Ok(out)
}

/// Open `path` in git's editor: $GIT_EDITOR, core.editor, $VISUAL, $EDITOR, vi.
fn run_editor(git_dir: Option<&Path>, path: &Path) -> anyhow::Result<()> {
    let editor = std::env::var("GIT_EDITOR")
        .ok()
        .or_else(|| {
            rgit_git::config_list(git_dir, &rgit_git::ConfigScope::Any, true)
                .ok()?
                .into_iter()
                .rfind(|e| e.name == "core.editor")?
                .value
        })
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "vi".to_owned());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor)
        .arg(path)
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("the editor exited with {status}"));
    }
    Ok(())
}

/// git's notes ref for `--ref`: `refs/notes/<name>` unless already a ref.
pub(crate) fn notes_ref_name(name: &str) -> String {
    if name.starts_with("refs/notes/") {
        name.to_owned()
    } else if name.starts_with("notes/") {
        format!("refs/{name}")
    } else {
        format!("refs/notes/{name}")
    }
}

/// A note's text from -m, -F and -C/-c, or from the editor (starting from
/// `current`) when none is given or with -c.
fn note_text(
    backend: &Arc<dyn GitBackend>,
    t: &NoteMessage,
    current: Option<String>,
    interactive: bool,
) -> anyhow::Result<String> {
    // (text, cleaned up unless --no-stripspace): -C/-c blobs are kept as is.
    let mut blobs = Vec::new();
    for reuse in t
        .reuse
        .iter()
        .chain(t.reedit.iter().filter(|r| !r.is_empty()))
    {
        let obj = backend.read_object(reuse)?;
        if obj.kind != "blob" {
            return Err(fatal_128(format!(
                "cannot read note data from non-blob object '{reuse}'."
            )));
        }
        blobs.push((String::from_utf8_lossy(&obj.data).into_owned(), false));
    }
    let messages = t.message.iter().map(|m| (m.clone(), true));
    let mut files = Vec::new();
    for f in &t.file {
        files.push((String::from_utf8(read_input(f)?)?, true));
    }
    // git takes the paragraphs in command-line order.
    let order = message_order();
    let mut sources = [
        blobs.into_iter(),
        messages.collect::<Vec<_>>().into_iter(),
        files.into_iter(),
    ];
    let mut parts: Vec<(String, bool)> = Vec::new();
    if order.iter().filter(|&&k| k == 0).count() == sources[0].len()
        && order.iter().filter(|&&k| k == 1).count() == sources[1].len()
        && order.iter().filter(|&&k| k == 2).count() == sources[2].len()
    {
        parts.extend(order.iter().filter_map(|&k| sources[k].next()));
    } else {
        parts.extend(sources.into_iter().flatten());
    }
    // git's concat_messages: each cleanup covers everything so far.
    let mut text = String::new();
    for (part, strip) in &parts {
        if !text.is_empty() {
            text.push_str(&t.separator());
        }
        text.push_str(part);
        if t.strip().unwrap_or(*strip) {
            text = stripspace(&text);
        }
    }
    if !(parts.is_empty() || t.reedit.is_some() || t.edit) {
        return Ok(text);
    }
    // Like git, a set $GIT_EDITOR works without a terminal.
    if !interactive && std::env::var_os("GIT_EDITOR").is_none() {
        return Err(CliError::usage("a note message required (-m, -F or -C)"));
    }
    let seed = current.unwrap_or(text);
    let edited = edit_file(
        backend,
        "NOTES_EDITMSG",
        &format!(
            "{seed}\n\n# Write/edit the notes for the object. Lines starting with '#' are ignored.\n"
        ),
    )?;
    let _ = std::fs::remove_file(backend.git_dir().join("NOTES_EDITMSG"));
    Ok(match t.strip() {
        Some(false) => edited,
        _ => stripspace(&strip_comments(&edited)),
    })
}

/// git's maybe-bool config values.
fn maybe_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" | "" => Some(false),
        _ => None,
    }
}

/// Which of `flags` (bare or `--flag=value`) comes last on the command line.
fn last_flag(flags: &[&str]) -> Option<String> {
    std::env::args().rev().find_map(|a| {
        let name = a.split('=').next().unwrap_or("").to_owned();
        flags.contains(&name.as_str()).then_some(name)
    })
}

/// The kinds of note paragraphs on the command line, in order: 0 for -C/-c,
/// 1 for -m, 2 for -F.
fn message_order() -> Vec<usize> {
    let mut order = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--" {
            break;
        }
        let kind = |a: &str| match a {
            "-C" | "-c" | "--reuse-message" | "--reedit-message" => Some(0),
            "-m" | "--message" => Some(1),
            "-F" | "--file" => Some(2),
            _ => None,
        };
        let (flag, joined) = match a.split_once('=') {
            Some((f, _)) if f.starts_with("--") => (f.to_owned(), true),
            _ if a.len() > 2 && !a.starts_with("--") => (a[..2].to_owned(), true),
            _ => (a.clone(), false),
        };
        if let Some(k) = kind(&flag) {
            order.push(k);
            if !joined {
                args.next();
            }
        }
    }
    order
}

fn fatal_128(message: String) -> anyhow::Error {
    anyhow::Error::new(CliError {
        message,
        help: None,
        code: 128,
    })
}

/// Write a note as `git notes <cmd>` does; an empty one removes the note
/// unless allowed.
fn write_note(
    backend: &Arc<dyn GitBackend>,
    notes_ref: Option<&str>,
    rev: &str,
    text: &str,
    allow_empty: bool,
    cmd: &str,
) -> anyhow::Result<String> {
    if text.is_empty() && !allow_empty {
        eprintln!("Removing note for object {}", backend.rev_parse(rev)?);
        backend.note_remove(notes_ref, &[rev.to_owned()], false, cmd)?;
        return Ok("ok".to_owned());
    }
    backend.note_add(notes_ref, rev, text, cmd)?;
    Ok("ok".to_owned())
}

/// `update-ref --stdin`: git's transaction protocol, answering `start`,
/// `prepare`, `commit` and `abort` on stdout as each arrives.
fn update_ref_stdin(
    backend: &Arc<dyn GitBackend>,
    z: bool,
    message: Option<&str>,
    no_deref: bool,
    create_reflog: bool,
    batch_updates: bool,
) -> anyhow::Result<String> {
    use std::io::{BufRead, Write};
    let mut input = std::io::stdin().lock();
    let mut out = std::io::stdout();
    let mut batch: Vec<rgit_git::RefUpdate> = Vec::new();
    let sep = if z { 0 } else { b'\n' };
    let read = |input: &mut std::io::StdinLock| -> anyhow::Result<Option<String>> {
        let mut buf = Vec::new();
        if input.read_until(sep, &mut buf)? == 0 {
            return Ok(None);
        }
        if buf.last() == Some(&sep) {
            buf.pop();
        }
        Ok(Some(String::from_utf8(buf)?))
    };
    let mut reply = |s: &str| -> anyhow::Result<()> {
        writeln!(out, "{s}")?;
        out.flush()?;
        Ok(())
    };
    let run = |batch: &[rgit_git::RefUpdate], check: bool| -> anyhow::Result<()> {
        let rejected = backend.update_refs(
            batch,
            message,
            no_deref,
            create_reflog,
            check,
            batch_updates,
        )?;
        let mut out = std::io::stdout();
        for r in rejected {
            writeln!(out, "{r}")?;
        }
        Ok(())
    };
    let zero = || "0".repeat(40);
    let mut open = false;
    // `option no-deref` holds for the next command only.
    let mut next_no_deref = false;
    while let Some(line) = read(&mut input)? {
        let (cmd, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        // With -z the values follow as their own NUL-terminated fields.
        let mut fields: Vec<String> = if z {
            vec![rest.to_owned()]
        } else {
            rest.split(' ')
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let wanted = match cmd {
            "symref-update" => 4,
            "update" => 3,
            "create" | "delete" | "verify" => 2,
            "symref-create" | "symref-delete" | "symref-verify" => 2,
            _ => 1,
        };
        while z && fields.len() < wanted && !rest.is_empty() {
            fields.push(read(&mut input)?.unwrap_or_default());
        }
        let field = |i: usize| fields.get(i).filter(|f| !f.is_empty()).cloned();
        let name = || field(0).ok_or_else(|| CliError::usage(format!("{cmd}: missing <ref>")));
        let missing = |what: &str| CliError::usage(format!("{cmd}: missing <{what}>"));
        let deref_only = || -> anyhow::Result<()> {
            if !(no_deref || next_no_deref) {
                return Err(CliError::usage(format!(
                    "{cmd}: cannot operate with deref mode"
                )));
            }
            Ok(())
        };
        let update = rgit_git::RefUpdate {
            no_deref: next_no_deref,
            ..Default::default()
        };
        let update = match cmd {
            "update" => Some(rgit_git::RefUpdate {
                name: name()?,
                new: Some(field(1).ok_or_else(|| missing("new-oid"))?),
                old: field(2),
                ..update
            }),
            "create" => Some(rgit_git::RefUpdate {
                name: name()?,
                new: Some(field(1).ok_or_else(|| missing("new-oid"))?),
                old: Some(zero()),
                ..update
            }),
            "delete" => Some(rgit_git::RefUpdate {
                name: name()?,
                old: field(1),
                ..update
            }),
            "verify" => Some(rgit_git::RefUpdate {
                name: name()?,
                old: Some(field(1).unwrap_or_else(zero)),
                verify: true,
                ..update
            }),
            "symref-update" => {
                let (old, old_target) = match (field(2).as_deref(), field(3)) {
                    (None, _) => (None, None),
                    (Some(_), None) => {
                        return Err(CliError::usage("symref-update: expected old value"));
                    }
                    (Some("oid"), v) => (v, None),
                    (Some("ref"), v) => (None, v),
                    (Some(arg), _) => {
                        return Err(CliError::usage(format!(
                            "symref-update {}: invalid arg '{arg}' for old value",
                            name()?
                        )));
                    }
                };
                Some(rgit_git::RefUpdate {
                    name: name()?,
                    new_target: Some(field(1).ok_or_else(|| missing("new-target"))?),
                    old,
                    old_target,
                    symref: true,
                    ..update
                })
            }
            "symref-create" => Some(rgit_git::RefUpdate {
                name: name()?,
                new_target: Some(field(1).ok_or_else(|| missing("new-target"))?),
                old: Some(zero()),
                symref: true,
                ..update
            }),
            "symref-delete" => {
                deref_only()?;
                Some(rgit_git::RefUpdate {
                    name: name()?,
                    old_target: field(1),
                    symref: true,
                    ..update
                })
            }
            "symref-verify" => {
                deref_only()?;
                Some(rgit_git::RefUpdate {
                    name: name()?,
                    old_target: field(1),
                    verify: true,
                    symref: true,
                    ..update
                })
            }
            _ => None,
        };
        if let Some(u) = update {
            batch.push(u);
            next_no_deref = false;
            continue;
        }
        match cmd {
            "option" => {
                if rest.trim() != "no-deref" {
                    return Err(CliError::usage(format!("option unknown: {rest}")));
                }
                next_no_deref = true;
            }
            "start" => {
                open = true;
                reply("start: ok")?;
            }
            "prepare" => {
                if !batch_updates {
                    run(&batch, true)?;
                }
                reply("prepare: ok")?;
            }
            "commit" => {
                run(&batch, false)?;
                batch.clear();
                open = false;
                reply("commit: ok")?;
            }
            "abort" => {
                batch.clear();
                open = false;
                reply("abort: ok")?;
            }
            "" => {}
            other => return Err(CliError::usage(format!("unknown command: {other}"))),
        }
    }
    // Without an explicit transaction, everything read is one; an open
    // transaction left at EOF is aborted, as in git.
    if !open && !batch.is_empty() {
        run(&batch, false)?;
    }
    Ok(String::new())
}

/// `rgit bundle`; list-heads needs no repository.
fn bundle(backend: Option<&Arc<dyn GitBackend>>, cmd: BundleCmd) -> anyhow::Result<String> {
    let refs = |list: &[(String, String)]| -> String {
        list.iter()
            .map(|(id, name)| format!("{id} {name}\n"))
            .collect()
    };
    if let BundleCmd::ListHeads { file, refnames } = &cmd {
        let header = rgit_git::bundle_header(Path::new(file))?;
        let kept: Vec<(String, String)> = header
            .refs
            .into_iter()
            .filter(|(_, name)| refnames.is_empty() || refnames.contains(name))
            .collect();
        return Ok(refs(&kept));
    }
    let backend = backend.ok_or_else(|| {
        anyhow::anyhow!("need a repository to create, verify or unbundle a bundle")
    })?;
    Ok(match cmd {
        BundleCmd::Create { file, revs, .. } => {
            backend.bundle_create(Path::new(&file), &revs)?;
            "ok".to_owned()
        }
        BundleCmd::Verify { file, quiet } => {
            let (header, missing) = backend.bundle_verify(Path::new(&file))?;
            if !missing.is_empty() {
                let mut msg = String::from("Repository lacks these prerequisite commits:");
                for (id, comment) in &missing {
                    msg.push_str(&format!("\n{id} {comment}"));
                }
                return Err(GitError::Other(msg).into());
            }
            eprintln!("{file} is okay");
            if quiet {
                return Ok(String::new());
            }
            let count = |n: usize, one: &str, many: &str| {
                if n == 1 {
                    one.to_owned()
                } else {
                    many.replace("%d", &n.to_string())
                }
            };
            let mut out = count(
                header.refs.len(),
                "The bundle contains this ref:\n",
                "The bundle contains these %d refs:\n",
            );
            out.push_str(&refs(&header.refs));
            if header.prerequisites.is_empty() {
                out.push_str("The bundle records a complete history.\n");
            } else {
                out.push_str(&count(
                    header.prerequisites.len(),
                    "The bundle requires this ref:\n",
                    "The bundle requires these %d refs:\n",
                ));
                for (id, _) in &header.prerequisites {
                    out.push_str(&format!("{id} \n"));
                }
            }
            out.push_str("The bundle uses this hash algorithm: sha1\n");
            out
        }
        BundleCmd::Unbundle { file } => refs(&backend.bundle_unbundle(Path::new(&file))?),
        BundleCmd::ListHeads { .. } => unreachable!(),
    })
}

/// The tools git knows: name, the diff command and the merge command, in
/// terms of $LOCAL, $REMOTE, $BASE and $MERGED.
const KNOWN_TOOLS: &[(&str, &str, &str)] = &[
    (
        "vimdiff",
        r#"vim -R -f -d "$LOCAL" "$REMOTE""#,
        r#"vim -f -d -c '4wincmd w | wincmd J' "$LOCAL" "$BASE" "$REMOTE" "$MERGED""#,
    ),
    (
        "nvimdiff",
        r#"nvim -R -f -d "$LOCAL" "$REMOTE""#,
        r#"nvim -f -d -c '4wincmd w | wincmd J' "$LOCAL" "$BASE" "$REMOTE" "$MERGED""#,
    ),
    (
        "meld",
        r#"meld "$LOCAL" "$REMOTE""#,
        r#"meld "$LOCAL" "$MERGED" "$REMOTE" --output "$MERGED""#,
    ),
    (
        "vscode",
        r#"code --wait --diff "$LOCAL" "$REMOTE""#,
        r#"code --wait --merge "$REMOTE" "$LOCAL" "$BASE" "$MERGED""#,
    ),
    (
        "opendiff",
        r#"opendiff "$LOCAL" "$REMOTE""#,
        r#"opendiff "$LOCAL" "$REMOTE" -ancestor "$BASE" -merge "$MERGED""#,
    ),
    (
        "kdiff3",
        r#"kdiff3 --L1 "$MERGED (A)" --L2 "$MERGED (B)" "$LOCAL" "$REMOTE""#,
        r#"kdiff3 --auto --L1 "$MERGED (Base)" --L2 "$MERGED (Local)" --L3 "$MERGED (Remote)" -o "$MERGED" "$BASE" "$LOCAL" "$REMOTE""#,
    ),
    (
        "bc",
        r#"bcompare "$LOCAL" "$REMOTE""#,
        r#"bcompare "$LOCAL" "$REMOTE" "$BASE" -mergeoutput="$MERGED""#,
    ),
    (
        "tkdiff",
        r#"tkdiff "$LOCAL" "$REMOTE""#,
        r#"tkdiff -a "$BASE" -o "$MERGED" "$LOCAL" "$REMOTE""#,
    ),
];

/// The shell command for `tool`: its configured `<kind>tool.<tool>.cmd`, else a
/// known tool's (with `<kind>tool.<tool>.path` as the program, if set).
fn tool_command(backend: &Arc<dyn GitBackend>, kind: &str, tool: &str) -> anyhow::Result<String> {
    let config = |k: String| backend.config_get(&k).ok().flatten();
    if let Some(cmd) = config(format!("{kind}tool.{tool}.cmd")) {
        return Ok(cmd);
    }
    let known = KNOWN_TOOLS.iter().find(|(name, ..)| {
        *name == tool
            || (tool == "code" && *name == "vscode")
            || tool.starts_with("bc") && *name == "bc"
    });
    let Some((_, diff, merge)) = known else {
        return Err(CliError::usage(format!(
            "unknown tool {tool}; set {kind}tool.{tool}.cmd or run `rgit {kind}tool --tool-help`"
        )));
    };
    let cmd = if kind == "diff" { *diff } else { *merge };
    Ok(match config(format!("{kind}tool.{tool}.path")) {
        Some(path) => match cmd.split_once(' ') {
            Some((_, rest)) => format!("'{path}' {rest}"),
            None => path,
        },
        None => cmd.to_owned(),
    })
}

/// The tool to use: --tool, else the configured one (difftool falls back to
/// merge.tool, as git does).
fn pick_tool(
    backend: &Arc<dyn GitBackend>,
    kind: &str,
    tool: Option<String>,
) -> anyhow::Result<String> {
    let config = |k: &str| backend.config_get(k).ok().flatten();
    tool.or_else(|| config(&format!("{kind}.tool")))
        .or_else(|| (kind == "diff").then(|| config("merge.tool")).flatten())
        .ok_or_else(|| {
            CliError::usage(format!(
                "no {kind} tool configured; pass --tool or set {kind}.tool (`rgit {kind}tool --tool-help` lists them)"
            ))
        })
}

/// Run a tool command through the shell with git's variables set.
fn run_tool(cmd: &str, vars: &[(&str, &Path)]) -> anyhow::Result<std::process::ExitStatus> {
    let mut sh = std::process::Command::new("sh");
    sh.arg("-c").arg(cmd);
    for (k, v) in vars {
        sh.env(k, v);
    }
    Ok(sh.status()?)
}

/// A temporary copy of a file's content, named after it as git names them.
fn temp_file(dir: &Path, path: &str, tag: &str, data: &[u8]) -> anyhow::Result<PathBuf> {
    let p = Path::new(path);
    let stem = p
        .file_stem()
        .map_or(String::new(), |s| s.to_string_lossy().into_owned());
    let ext = p
        .extension()
        .map_or(String::new(), |e| format!(".{}", e.to_string_lossy()));
    let file = dir.join(format!("{stem}_{tag}_{}{ext}", std::process::id()));
    std::fs::write(&file, data)?;
    Ok(file)
}

fn tool_help(kind: &str) -> String {
    let mut out = format!("'rgit {kind}tool --tool=<tool>' may be set to one of the following:\n");
    for (name, ..) in KNOWN_TOOLS {
        out.push_str(&format!("\t\t{name}\n"));
    }
    out.push_str(&format!("\nOr any command set as {kind}tool.<tool>.cmd.\n"));
    out
}

/// `rgit difftool`: each changed file (or both trees with --dir-diff) in the
/// diff tool.
fn difftool(
    backend: &Arc<dyn GitBackend>,
    a: ToolArgs,
    interactive: bool,
) -> anyhow::Result<String> {
    if a.tool_help {
        return Ok(tool_help("diff"));
    }
    let cmd = match &a.extcmd {
        Some(x) => format!(r#"{x} "$LOCAL" "$REMOTE""#),
        None => tool_command(
            backend,
            "diff",
            &pick_tool(backend, "diff", a.tool.clone())?,
        )?,
    };
    let (from, to) = match a.revs.as_slice() {
        [] => (None, None),
        [r] if r.contains("..") => {
            let (x, y) = r.split_once("..").unwrap_or_default();
            (Some(x.trim_end_matches('.').to_owned()), Some(y.to_owned()))
        }
        [r] => (Some(r.clone()), None),
        [x, y] => (Some(x.clone()), Some(y.clone())),
        _ => return Err(CliError::usage("give at most two revisions")),
    };
    let files = backend.diff(&rgit_git::DiffSpec {
        from: from.clone(),
        to: to.clone(),
        cached: a.cached,
        paths: a.paths.clone(),
        ..Default::default()
    })?;
    let index = backend.index_entries()?;
    let blob = |rev: Option<&str>, path: &str| -> Vec<u8> {
        let id = match rev {
            Some(r) => backend
                .read_object(&format!("{r}:{path}"))
                .ok()
                .map(|o| o.data),
            None => index
                .iter()
                .find(|e| e.path == path && e.stage == 0)
                .and_then(|e| backend.read_object(&e.id).ok())
                .map(|o| o.data),
        };
        id.unwrap_or_default()
    };
    let head = || Some("HEAD");
    // Old side: the first revision, HEAD for --cached, else the index.
    let old_rev = from.as_deref().or(if a.cached { head() } else { None });
    let root = backend.workdir().to_path_buf();
    let tmp = std::env::temp_dir().join(format!("rgit-difftool-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let result = (|| -> anyhow::Result<()> {
        let new_side = |path: &str, dir: &Path| -> anyhow::Result<PathBuf> {
            Ok(match (&to, a.cached) {
                (Some(r), _) => temp_file(dir, path, "REMOTE", &blob(Some(r), path))?,
                (None, true) => temp_file(dir, path, "REMOTE", &blob(None, path))?,
                (None, false) => root.join(path),
            })
        };
        if a.dir_diff {
            let (left, right) = (tmp.join("left"), tmp.join("right"));
            for f in &files {
                let old = f.old_path.as_deref().unwrap_or(&f.path);
                for (dir, path, data) in [
                    (&left, old, blob(old_rev, old)),
                    (
                        &right,
                        f.path.as_str(),
                        std::fs::read(new_side(&f.path, &tmp)?).unwrap_or_default(),
                    ),
                ] {
                    let dest = dir.join(path);
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(dest, data)?;
                }
            }
            run_tool(&cmd, &[("LOCAL", &left), ("REMOTE", &right)])?;
            return Ok(());
        }
        let prompt = !a.no_prompt
            && (a.prompt
                || backend
                    .config_get("difftool.prompt")?
                    .is_none_or(|v| v != "false"))
            && interactive;
        let trust = a.trust_exit_code
            || backend.config_get("difftool.trustExitCode")?.as_deref() == Some("true");
        for (i, f) in files.iter().enumerate() {
            let old = f.old_path.as_deref().unwrap_or(&f.path);
            if prompt {
                eprintln!("\nViewing ({}/{}): '{}'", i + 1, files.len(), f.path);
                let answer = crate::interactive::input("Launch the tool [Y/n]? ")?;
                if answer.trim().eq_ignore_ascii_case("n") {
                    continue;
                }
            }
            let local = temp_file(&tmp, old, "LOCAL", &blob(old_rev, old))?;
            let remote = new_side(&f.path, &tmp)?;
            let merged = root.join(&f.path);
            let status = run_tool(
                &cmd,
                &[
                    ("LOCAL", &local),
                    ("REMOTE", &remote),
                    ("MERGED", &merged),
                    ("BASE", &merged),
                ],
            )?;
            if trust && !status.success() {
                return Err(anyhow::anyhow!("the diff tool exited with {status}"));
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result?;
    Ok(String::new())
}

/// `rgit mergetool`: each conflicted file in the merge tool; a resolved file
/// is staged, as git does.
fn mergetool(
    backend: &Arc<dyn GitBackend>,
    a: ToolArgs,
    interactive: bool,
) -> anyhow::Result<String> {
    if a.tool_help {
        return Ok(tool_help("merge"));
    }
    let tool = pick_tool(backend, "merge", a.tool.clone())?;
    let cmd = tool_command(backend, "merge", &tool)?;
    let config = |k: String| backend.config_get(&k).ok().flatten();
    let index = backend.index_entries()?;
    let wanted: Vec<String> = a.revs.iter().chain(&a.paths).cloned().collect();
    let mut paths: Vec<String> = index
        .iter()
        .filter(|e| e.stage > 0)
        .map(|e| e.path.clone())
        .filter(|p| wanted.is_empty() || rgit_git::pathspec_matches(&wanted, p))
        .collect();
    paths.dedup();
    if paths.is_empty() {
        return Ok("No files need merging".to_owned());
    }
    let prompt = !a.no_prompt
        && (a.prompt || config("mergetool.prompt".into()).is_none_or(|v| v != "false"))
        && interactive;
    let trust = a.trust_exit_code
        || config(format!("mergetool.{tool}.trustExitCode")).as_deref() == Some("true");
    let keep_backup = config("mergetool.keepBackup".into()).is_none_or(|v| v != "false");
    let root = backend.workdir().to_path_buf();
    let mut resolved = Vec::new();
    for path in &paths {
        let stage = |n: u8| {
            index
                .iter()
                .find(|e| e.path == *path && e.stage == n)
                .and_then(|e| backend.read_object(&e.id).ok())
                .map(|o| o.data)
        };
        let (base, local, remote) = (stage(1), stage(2), stage(3));
        if local.is_none() || remote.is_none() {
            return Err(anyhow::anyhow!(
                "{path} was deleted on one side; resolve it with `rgit add` or `rgit rm`"
            ));
        }
        if prompt {
            eprintln!("\nNormal merge conflict for '{path}':");
            let _ = crate::interactive::input(&format!(
                "Hit return to start merge resolution tool ({tool}): "
            ))?;
        }
        let merged = root.join(path);
        let dir = merged.parent().unwrap_or(&root).to_path_buf();
        let before = std::fs::read(&merged).unwrap_or_default();
        if keep_backup {
            std::fs::write(
                merged.with_file_name(format!(
                    "{}.orig",
                    merged.file_name().unwrap_or_default().to_string_lossy()
                )),
                &before,
            )?;
        }
        let files = [
            (
                "BASE",
                temp_file(&dir, path, "BASE", &base.unwrap_or_default())?,
            ),
            (
                "LOCAL",
                temp_file(&dir, path, "LOCAL", &local.unwrap_or_default())?,
            ),
            (
                "REMOTE",
                temp_file(&dir, path, "REMOTE", &remote.unwrap_or_default())?,
            ),
        ];
        let mut vars: Vec<(&str, &Path)> = files.iter().map(|(k, p)| (*k, p.as_path())).collect();
        vars.push(("MERGED", &merged));
        let status = run_tool(&cmd, &vars);
        for (_, f) in &files {
            let _ = std::fs::remove_file(f);
        }
        let status = status?;
        let changed = std::fs::read(&merged).unwrap_or_default() != before;
        let ok = if trust {
            status.success()
        } else if changed {
            true
        } else if interactive {
            eprintln!("{path} seems unchanged.");
            crate::interactive::confirm("Was the merge successful?")?
        } else {
            false
        };
        if !ok {
            return Err(anyhow::anyhow!("merge of {path} failed"));
        }
        backend.add(std::slice::from_ref(path), false, false)?;
        resolved.push(path.clone());
    }
    Ok(format!("resolved {}", resolved.join(", ")))
}

/// `rgit hash-object`, in a repository or outside one.
pub fn hash_object(backend: Option<&dyn GitBackend>, a: HashObjectArgs) -> anyhow::Result<String> {
    let git_dir = backend.map(|b| b.git_dir());
    let root = backend.and_then(|b| b.workdir().canonicalize().ok());
    // Attributes are looked up by the path inside the working tree.
    let repo_path = |p: &str| -> Option<String> {
        let root = root.as_ref()?;
        let abs = std::path::absolute(p).ok()?;
        let dir = abs.parent()?.canonicalize().ok()?;
        let rel = dir.join(abs.file_name()?);
        Some(rel.strip_prefix(root).ok()?.to_string_lossy().into_owned())
    };
    let kind = a.kind.as_deref().unwrap_or("blob");
    let hash = |data: &[u8], file: Option<&str>| -> anyhow::Result<String> {
        let path = if a.no_filters {
            None
        } else {
            a.path.as_deref().or(file).and_then(repo_path)
        };
        Ok(rgit_git::hash_object(
            git_dir.as_deref(),
            kind,
            data,
            path.as_deref(),
            a.write,
            a.literally,
        )?)
    };
    let mut out = String::new();
    if a.stdin {
        out.push_str(&hash(&read_input("-")?, None)?);
        out.push('\n');
    }
    let listed = if a.stdin_paths {
        String::from_utf8(read_input("-")?)?
            .lines()
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };
    for p in a.paths.iter().chain(&listed) {
        out.push_str(&hash(&read_input(p)?, Some(p))?);
        out.push('\n');
    }
    if out.is_empty() && !a.stdin_paths {
        return Err(CliError::usage("a file, --stdin or --stdin-paths required"));
    }
    Ok(out)
}

/// `rgit apply`, in a repository or outside one.
pub fn apply(backend: Option<&dyn GitBackend>, a: ApplyArgs) -> anyhow::Result<String> {
    let opts = rgit_git::ApplyOpts {
        cached: a.cached,
        index: a.index,
        check: a.check,
        reverse: a.reverse,
        three_way: a.three_way,
        reject: a.reject,
        verbose: a.verbose,
        strip: a.strip,
        directory: a.directory,
        include: a.include,
        exclude: a.exclude,
        whitespace: a.whitespace.clone(),
        allow_empty: a.allow_empty,
        favor: [(a.ours, "ours"), (a.theirs, "theirs"), (a.union, "union")]
            .into_iter()
            .find_map(|(on, f)| on.then(|| f.to_owned())),
        recount: a.recount,
        quiet: a.quiet,
        context: a.context,
        unidiff_zero: a.unidiff_zero,
        ignore_whitespace: a.ignore_whitespace
            || backend
                .and_then(|b| b.config_get("apply.ignoreWhitespace").ok().flatten())
                .is_some_and(|v| v == "change"),
        inaccurate_eof: a.inaccurate_eof,
        allow_overlap: a.allow_overlap,
        intent_to_add: a.intent_to_add && !a.cached && !a.index && !a.three_way,
        fake_ancestor: a.build_fake_ancestor.map(Into::into),
        // As in git, a subfolder only applies the paths under it.
        prefix: backend.and_then(|b| {
            let root = b.workdir().canonicalize().ok()?;
            let cwd = std::env::current_dir().ok()?.canonicalize().ok()?;
            Some(cwd.strip_prefix(root).ok()?.to_string_lossy().into_owned())
        }),
    };
    let patches = if a.patches.is_empty() {
        vec!["-".to_owned()]
    } else {
        a.patches
    };
    let mut files = Vec::new();
    for p in &patches {
        let mut parsed = rgit_git::parse_patch(&read_input(p)?, &opts)?;
        for f in &mut parsed {
            f.source = p.clone();
        }
        files.extend(parsed);
    }
    let applying = !a.check && (a.apply || !(a.stat || a.numstat || a.summary));
    // As git: warn when applying, and say nothing when only reporting.
    let ws_action = a
        .whitespace
        .clone()
        .or_else(|| backend.and_then(|b| b.config_get("apply.whitespace").ok().flatten()))
        .unwrap_or_else(|| if applying { "warn" } else { "nowarn" }.to_owned());
    let git_dir = backend.map(|b| b.git_dir());
    let ws = rgit_git::check_whitespace(
        &mut files,
        &ws_action,
        git_dir.as_deref(),
        a.reverse,
        applying,
    );
    eprint!("{}", ws.report);
    if ws.fatal {
        eprint!("{}", ws.summary);
        return Err(fatal_128(String::new()));
    }
    let mut out = String::new();
    if a.stat {
        out.push_str(&rgit_git::patch_stat(&files));
    }
    if a.numstat {
        let numstat = rgit_git::patch_numstat(&files);
        out.push_str(&if a.z {
            numstat.replace('\n', "\0")
        } else {
            numstat
        });
    }
    if a.summary {
        out.push_str(&rgit_git::patch_summary(&files));
    }
    if a.apply || !(a.stat || a.numstat || a.summary) {
        let log = match backend {
            Some(b) => b.apply_patch(&files, &opts)?,
            None => rgit_git::apply_outside(&files, &opts)?,
        };
        if !log.is_empty() {
            eprintln!("{log}");
        }
        if out.is_empty() {
            out = "ok".to_owned();
        }
    }
    eprint!("{}", ws.summary);
    Ok(out)
}

fn read_input(path: &str) -> anyhow::Result<Vec<u8>> {
    if path == "-" {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
        Ok(buf)
    } else {
        std::fs::read(path).map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))
    }
}

/// `--add-file`s and `--add-virtual-file`s in command-line order, as git adds
/// them, each add-file with the `--prefix` in force where it stood: `(true,
/// "")` is a virtual file, `(false, prefix)` an add-file. Without such a
/// command line (MCP), add-files take `fallback` and come first.
fn extra_order(files: usize, virtuals: usize, fallback: &str) -> Vec<(bool, String)> {
    let mut prefix = String::new();
    let mut found = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let (name, value) = arg.split_once('=').unwrap_or((&arg, ""));
        let value = match name {
            "--prefix" | "--add-file" | "--add-virtual-file" if !arg.contains('=') => {
                args.next().unwrap_or_default()
            }
            _ => value.to_owned(),
        };
        match name {
            "--prefix" => prefix = value,
            "--add-file" => found.push((false, prefix.clone())),
            "--add-virtual-file" => found.push((true, String::new())),
            _ => {}
        }
    }
    if found.iter().filter(|(v, _)| !v).count() == files && found.len() == files + virtuals {
        return found;
    }
    let mut order = vec![(false, fallback.to_owned()); files];
    order.extend(vec![(true, String::new()); virtuals]);
    order
}

/// `rgit archive` output; the format defaults from `output`'s extension.
pub fn archive(backend: Option<&Arc<dyn GitBackend>>, a: &ArchiveArgs) -> anyhow::Result<Vec<u8>> {
    // `-0` to `-9` arrive as positionals.
    let is_level = |s: &str| s.len() == 2 && s.starts_with('-') && s.as_bytes()[1].is_ascii_digit();
    let mut words: Vec<&String> = a.rev.iter().chain(&a.paths).collect();
    let level = words
        .iter()
        .rev()
        .find(|w| is_level(w))
        .map(|w| u32::from(w.as_bytes()[1] - b'0'));
    words.retain(|w| !is_level(w));
    let formats = rgit_git::archive_formats(backend.map(|b| b.workdir()));
    let inferred = a
        .output
        .as_deref()
        .and_then(|o| rgit_git::archive_format_from_filename(o, &formats));
    let mut cwd = a.cwd.clone();
    let remote: Arc<dyn GitBackend>;
    let backend = match (&a.remote, backend) {
        (Some(name), b) => {
            let url = b
                .and_then(|b| b.config_get(&format!("remote.{name}.url")).ok().flatten())
                .unwrap_or_else(|| name.clone());
            if !rgit_git::is_local_url(&url) {
                // git sends its own arguments, the format -o implies first.
                let mut args: Vec<String> = inferred
                    .map(|f| format!("--format={f}"))
                    .into_iter()
                    .collect();
                args.extend(a.list.then(|| "-l".to_owned()));
                args.extend(a.format.as_ref().map(|f| format!("--format={f}")));
                args.extend(a.prefix.as_ref().map(|p| format!("--prefix={p}")));
                args.extend(a.add_file.iter().map(|f| format!("--add-file={f}")));
                args.extend(
                    a.add_virtual_file
                        .iter()
                        .map(|f| format!("--add-virtual-file={f}")),
                );
                args.extend(
                    a.worktree_attributes
                        .then(|| "--worktree-attributes".to_owned()),
                );
                args.extend(a.verbose.then(|| "-v".to_owned()));
                args.extend(a.mtime.as_ref().map(|t| format!("--mtime={t}")));
                args.extend(level.map(|l| format!("-{l}")));
                args.extend(words.iter().map(|w| w.to_string()));
                let ssh = b.and_then(|b| b.config_get("core.sshCommand").ok().flatten());
                let exec = a.exec.as_deref().unwrap_or("git-upload-archive");
                // The remote's own error prints as git shows it, `remote: ...`.
                return rgit_git::remote_archive(&url, exec, &args, ssh.as_deref()).map_err(|e| {
                    let msg = e.to_string();
                    if !msg.starts_with("remote: ") {
                        return e.into();
                    }
                    eprintln!("{msg}");
                    anyhow::Error::new(CliError {
                        message: String::new(),
                        help: None,
                        code: 1,
                    })
                });
            }
            let path = url.strip_prefix("file://").unwrap_or(&url);
            remote = Arc::new(rgit_git::Git2Backend::discover(path)?);
            cwd.clear();
            &remote
        }
        (None, Some(b)) => b,
        (None, None) => return Err(CliError::not_a_repo()),
    };
    if a.list {
        return Ok(formats
            .iter()
            .map(|f| format!("{f}\n"))
            .collect::<String>()
            .into_bytes());
    }
    let rev = words.first().map_or("HEAD".to_owned(), |r| r.to_string());
    let paths: Vec<String> = words.iter().skip(1).map(|p| p.to_string()).collect();
    let format = a
        .format
        .clone()
        .or(inferred)
        .unwrap_or_else(|| "tar".to_owned());
    let prefix = a.prefix.clone().unwrap_or_default();
    let mut extra = Vec::new();
    let (mut files, mut virtuals) = (a.add_file.iter(), a.add_virtual_file.iter());
    for (virtual_file, base) in extra_order(a.add_file.len(), a.add_virtual_file.len(), &prefix) {
        if !virtual_file && let Some(f) = files.next() {
            let (name, mode, data) = rgit_git::archive_file(Path::new(f))?;
            extra.push((format!("{base}{name}"), mode, data));
        } else if let Some(v) = virtuals.next() {
            let (path, content) = v
                .split_once(':')
                .filter(|(p, _)| !p.is_empty())
                .ok_or_else(|| CliError::usage(format!("missing colon: '{v}'")))?;
            extra.push((
                format!("{cwd}{path}"),
                0o100644,
                content.as_bytes().to_vec(),
            ));
        }
    }
    Ok(backend.archive(&rgit_git::ArchiveOpts {
        rev,
        format,
        prefix,
        paths,
        level,
        extra,
        worktree_attributes: a.worktree_attributes,
        mtime: match &a.mtime {
            Some(t) => Some(
                rgit_git::expiry_date(t)
                    .ok_or_else(|| CliError::usage(format!("bad --mtime {t:?}")))?,
            ),
            None => None,
        },
        verbose: a.verbose,
        cwd,
    })?)
}

/// `rgit archive` with an output file, or `--list`.
pub fn archive_cmd(
    backend: Option<&Arc<dyn GitBackend>>,
    a: ArchiveArgs,
) -> anyhow::Result<String> {
    if a.list {
        return Ok(String::from_utf8_lossy(&archive(backend, &a)?).into_owned());
    }
    let Some(output) = a.output.clone().filter(|o| o != "-") else {
        return Err(CliError::usage("-o <file> required"));
    };
    let bytes = archive(backend, &a)?;
    std::fs::write(&output, bytes)?;
    Ok(format!("wrote {output}"))
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
    quiet: bool,
}

/// The checkout mode for -f/-m and `--conflict`, which implies -m.
/// `checkout -m`/`restore --merge` on paths: conflicts recreated, the rest
/// taken from the index, reported as git does.
fn merge_paths(
    backend: &Arc<dyn GitBackend>,
    paths: &[String],
    style: Option<&str>,
) -> anyhow::Result<String> {
    let (recreated, updated, errors) = backend.checkout_merge(paths, style)?;
    let s = |n: usize| if n == 1 { "" } else { "s" };
    let mut lines = Vec::new();
    if recreated > 0 {
        lines.push(format!(
            "Recreated {recreated} merge conflict{}",
            s(recreated)
        ));
    }
    if recreated == 0 || updated > 0 {
        lines.push(format!(
            "Updated {updated} path{} from the index",
            s(updated)
        ));
    }
    if !errors.is_empty() {
        anyhow::bail!("{}", errors.join("\n"));
    }
    Ok(lines.join("\n"))
}

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
    let text = render::text_mode();
    // git reports a switch on stderr, after the local changes on stdout.
    let switched = |line: String| -> anyhow::Result<String> {
        if o.quiet {
            return Ok(String::new());
        }
        let changes = local_changes(backend, true)?;
        eprintln!("{line}");
        Ok(changes)
    };
    if let Some((name, force)) = new {
        create(&name, rev.as_deref().unwrap_or("HEAD"), force, o.track)?;
        if text {
            // From HEAD the tree is left alone, so git lists no changes.
            if rev.is_none() {
                if !o.quiet {
                    eprintln!("Switched to a new branch '{name}'");
                }
                return Ok(String::new());
            }
            return switched(format!("Switched to a new branch '{name}'"));
        }
        return Ok("ok".to_owned());
    }
    let rev = rev.unwrap_or_else(|| "HEAD".to_owned());
    let old = backend.status()?.head;
    let tracked = |name: &str, start: &str| -> anyhow::Result<String> {
        create(name, start, false, !o.no_track)?;
        if text {
            let set_up = if o.no_track || o.quiet {
                String::new()
            } else {
                format!("branch '{name}' set up to track '{start}'.\n")
            };
            return Ok(set_up + &switched(format!("Switched to a new branch '{name}'"))?);
        }
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
    if text {
        let head = backend.status()?.head;
        return match &head.branch {
            Some(b) if old.branch.as_ref() == Some(b) => switched(format!("Already on '{b}'")),
            Some(b) => switched(format!("Switched to branch '{b}'")),
            None => {
                let now = short_subject(backend, "HEAD")?;
                if old.branch.is_some() && !o.detach && !o.quiet {
                    eprint!("{}", detach_advice(&rev));
                }
                switched(format!("HEAD is now at {now}"))
            }
        };
    }
    Ok(if prev {
        format!("checked out {rev}")
    } else {
        "ok".to_owned()
    })
}

/// `<abbrev> <subject>` of `rev`, as git's "HEAD is now at" names a commit.
fn short_subject(backend: &Arc<dyn GitBackend>, rev: &str) -> anyhow::Result<String> {
    let id = backend.rev_parse(rev)?;
    let c = crate::pretty::parse(&backend.read_object(&id)?);
    Ok(format!(
        "{} {}",
        backend.abbrev_id(&id, 7)?,
        crate::pretty::subject(&c.message, " ")
    ))
}

/// git's `<letter>\t<path>` lines for tracked changes: the working tree
/// against the index (`diff-files`), or against HEAD (`diff-index HEAD`).
fn local_changes(backend: &Arc<dyn GitBackend>, against_head: bool) -> anyhow::Result<String> {
    use rgit_git::StatusCode as S;
    let mut out = String::new();
    for e in backend.status()?.entries {
        if matches!(e.worktree, S::Untracked | S::Ignored) {
            continue;
        }
        let letter = match (e.index, e.worktree) {
            (S::Unmerged, _) | (_, S::Unmerged) => "U",
            (_, S::Deleted) => "D",
            (S::Added, _) if against_head => "A",
            (S::Deleted, _) if against_head => "D",
            (_, S::Unmodified) if !against_head => continue,
            (S::Unmodified, S::Unmodified) => continue,
            _ => "M",
        };
        out.push_str(&format!("{letter}\t{}\n", e.path));
    }
    Ok(out)
}

/// git's summaries of the commits the sequencer just made on top of `old`.
fn pick_summaries(backend: &Arc<dyn GitBackend>, old: &str) -> anyhow::Result<String> {
    let mut ids = Vec::new();
    let mut id = backend.rev_parse("HEAD")?;
    while id != old && ids.len() < 10_000 {
        let parent = crate::pretty::parse(&backend.read_object(&id)?)
            .parents
            .into_iter()
            .next();
        ids.push(id);
        match parent {
            Some(p) => id = p,
            None => break,
        }
    }
    let mut out = String::new();
    for id in ids.iter().rev() {
        out.push_str(&commit_summary(backend, id, true)?);
        out.push('\n');
    }
    Ok(out)
}

/// git's summary of a commit it just made: `[<branch> <abbrev>] <subject>`,
/// the author date when it is not the commit's own, the shortstat and the
/// created and deleted files.
fn commit_summary(
    backend: &Arc<dyn GitBackend>,
    id: &str,
    show_date: bool,
) -> anyhow::Result<String> {
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let id = backend.rev_parse(id)?;
    let c = crate::pretty::parse(&backend.read_object(&id)?);
    let branch = backend
        .status()?
        .head
        .branch
        .unwrap_or_else(|| "detached HEAD".to_owned());
    let root = if c.parents.is_empty() {
        " (root-commit)"
    } else {
        ""
    };
    let mut out = format!(
        "[{branch}{root} {}] {}",
        backend.abbrev_id(&id, 7)?,
        crate::pretty::subject(&c.message, " ")
    );
    if show_date {
        let a = &c.author;
        out.push_str(&format!(
            "\n Date: {}",
            crate::pretty::format_date(a.time, a.offset, "default")
        ));
    }
    let parent = c.parents.first().map_or(EMPTY_TREE, String::as_str);
    let files = backend.diff_refs(parent, &id)?;
    if !files.is_empty() {
        out.push('\n');
        out.push_str(&render::stat_summary(&files));
    }
    for f in &files {
        match f.status {
            rgit_git::StatusCode::Added => {
                out.push_str(&format!("\n create mode {:06o} {}", f.modes.1, f.path))
            }
            rgit_git::StatusCode::Deleted => {
                out.push_str(&format!("\n delete mode {:06o} {}", f.modes.0, f.path))
            }
            _ => {}
        }
    }
    Ok(out)
}

/// git's `Unstaged changes after reset:` list, or nothing.
fn unstaged_after_reset(backend: &Arc<dyn GitBackend>) -> anyhow::Result<String> {
    let changes = local_changes(backend, false)?;
    Ok(if changes.is_empty() {
        changes
    } else {
        format!("Unstaged changes after reset:\n{changes}")
    })
}

/// git's advice on detaching HEAD at `rev`.
fn detach_advice(rev: &str) -> String {
    format!(
        "Note: switching to '{rev}'.

You are in 'detached HEAD' state. You can look around, make experimental
changes and commit them, and you can discard any commits you make in this
state without impacting any branches by switching back to a branch.

If you want to create a new branch to retain commits you create, you may
do so (now or later) by using -c with the switch command. Example:

  git switch -c <new-branch-name>

Or undo this operation with:

  git switch -

Turn off this advice by setting config variable advice.detachedHead to false

"
    )
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
/// for-each-ref `--format`, colored and in columns as git's build_format and
/// print_columns do.
fn render_branch_rows(
    backend: &Arc<dyn GitBackend>,
    rows: &[BranchRow],
    opts: &BranchOpts,
) -> anyhow::Result<String> {
    let all = opts.all && !opts.remotes;
    let colopts = colopts(backend, "branch", opts.column.as_deref(), opts.no_column)?;
    if opts.verbose > 0 && opts.column.is_some() && colopts.active() {
        return Err(CliError::usage(
            "options '--column' and '--verbose' cannot be used together",
        ));
    }
    let lines = if let Some(fmt) = &opts.format {
        crate::plumbing::format_refs(backend, rows.iter().map(|r| &r.detail), fmt)?
            .into_iter()
            .filter(|l| !(opts.omit_empty && l.is_empty()))
            .collect()
    } else {
        branch_lines(backend, rows, opts, all)?
    };
    if opts.verbose > 0 || !colopts.active() {
        return Ok(lines.iter().map(|l| format!("{l}\n")).collect());
    }
    Ok(render::columns(&lines, colopts, "", 1))
}

/// `command`'s column options: `column.ui`, then `column.<command>`, then
/// `--column[=<options>]` / `--no-column`, as git's builtins read them.
pub(crate) fn colopts(
    backend: &Arc<dyn GitBackend>,
    command: &str,
    flag: Option<&str>,
    no_column: bool,
) -> anyhow::Result<render::Colopts> {
    let mut c = render::Colopts::default();
    for key in ["column.ui".to_owned(), format!("column.{command}")] {
        if let Some(v) = backend.config_get(&key).ok().flatten() {
            c.parse(&v)
                .map_err(|e| anyhow::anyhow!("invalid column.{command} mode {v}: {e}"))?;
        }
    }
    if no_column {
        c.enable = render::ColEnable::Never;
    } else if let Some(f) = flag {
        c.enable = render::ColEnable::Always;
        c.parse(f).map_err(CliError::usage)?;
    }
    Ok(c)
}

/// git's `(HEAD detached at X)` label for the branch list, or None on a
/// branch.
fn head_description(backend: &Arc<dyn GitBackend>) -> Option<String> {
    if backend.symbolic_ref("HEAD").ok().flatten().is_some() {
        return None;
    }
    let git_dir = backend.git_dir();
    let read = |p: &str| std::fs::read_to_string(git_dir.join(p)).ok();
    let short = |r: &str| r.trim().trim_start_matches("refs/heads/").to_owned();
    if let Some(name) = read("rebase-merge/head-name").or_else(|| read("rebase-apply/head-name")) {
        return Some(format!("(no branch, rebasing {})", short(&name)));
    }
    if let Some(start) = read("BISECT_START") {
        return Some(format!("(no branch, bisect started on {})", short(&start)));
    }
    let head = backend.rev_parse("HEAD").ok()?;
    let from = backend.reflog("HEAD").ok().and_then(|log| {
        log.into_iter().find_map(|e| {
            let to = e.message.strip_prefix("checkout: moving from ")?;
            let to = to.rsplit_once(" to ")?.1.to_owned();
            Some((to, e.id))
        })
    });
    let Some((to, id)) = from else {
        return Some("(no branch)".to_owned());
    };
    let named = ["refs/tags/", "refs/remotes/"].iter().find_map(|p| {
        let full = backend.full_ref_name(&to).ok().flatten()?;
        let name = full.strip_prefix(p)?.to_owned();
        let tip = backend.rev_parse(&format!("{full}^{{commit}}")).ok()?;
        (tip == id).then_some(name)
    });
    let label = named.unwrap_or_else(|| backend.abbrev_id(&id, 0).unwrap_or(id.clone()));
    let at = if head == id { "at" } else { "from" };
    Some(format!("(HEAD detached {at} {label})"))
}

/// Each branch other worktrees have checked out, by full ref name, with the
/// worktree's path.
fn worktree_branches(backend: &Arc<dyn GitBackend>) -> std::collections::HashMap<String, String> {
    let git_dir = backend.git_dir();
    let common = common_dir(backend);
    let mut map = std::collections::HashMap::new();
    let mut admin: Vec<(PathBuf, String)> = Vec::new();
    if common.file_name().is_some_and(|n| n == ".git") {
        let top = common.parent().map(|p| p.to_string_lossy().into_owned());
        admin.extend(top.map(|t| (common.clone(), t)));
    }
    for d in std::fs::read_dir(common.join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if let Ok(g) = std::fs::read_to_string(d.path().join("gitdir")) {
            let g = g.trim();
            admin.push((d.path(), g.strip_suffix("/.git").unwrap_or(g).to_owned()));
        }
    }
    for (dir, path) in admin {
        let same = dir.canonicalize().ok() == git_dir.canonicalize().ok();
        if let Some(r) = std::fs::read_to_string(dir.join("HEAD"))
            .ok()
            .and_then(|h| h.trim().strip_prefix("ref: ").map(str::to_owned))
            && !same
        {
            map.insert(r, path);
        }
    }
    map
}

/// The lines of git's default branch format (build_format).
fn branch_lines(
    backend: &Arc<dyn GitBackend>,
    rows: &[BranchRow],
    opts: &BranchOpts,
    all: bool,
) -> anyhow::Result<Vec<String>> {
    let on = render::color_on();
    let c = |code: &str| {
        if on {
            format!("\x1b[{code}m")
        } else {
            String::new()
        }
    };
    let reset = if on { "\x1b[m" } else { "" };
    let worktrees = worktree_branches(backend);
    let listing_heads = !opts.remotes && opts.args.is_empty() && opts.format.is_none();
    let detached = listing_heads
        .then(|| head_description(backend))
        .flatten()
        .filter(|_| {
            let head = |r: &Option<String>, want| {
                r.as_ref()
                    .is_none_or(|rev| backend.is_ancestor("HEAD", rev).is_ok_and(|m| m == want))
            };
            let contains = |r: &Option<String>, want| {
                r.as_ref()
                    .is_none_or(|rev| backend.is_ancestor(rev, "HEAD").is_ok_and(|m| m == want))
            };
            head(&opts.merged, true)
                && head(&opts.no_merged, false)
                && contains(&opts.contains, true)
                && contains(&opts.no_contains, false)
                && opts
                    .points_at
                    .as_ref()
                    .is_none_or(|p| backend.rev_parse(p).ok() == backend.rev_parse("HEAD").ok())
        });
    let oid = |id: &str| -> String {
        match opts.abbrev {
            _ if opts.no_abbrev => id.to_owned(),
            Some(n) => backend.abbrev_id(id, n).unwrap_or_else(|_| id.to_owned()),
            None => backend.abbrev_id(id, 0).unwrap_or_else(|_| id.to_owned()),
        }
    };
    let mut lines = Vec::new();
    let width = rows
        .iter()
        .map(|r| {
            let w = r.name.chars().count();
            if r.remote && all { w + 8 } else { w }
        })
        .chain(detached.iter().map(|d| d.chars().count()))
        .max()
        .unwrap_or(0);
    if let Some(desc) = &detached {
        let mut line = format!("* {}{desc}", c("32"));
        if opts.verbose > 0 {
            let tip = backend.rev_parse("HEAD")?;
            let subject = backend
                .log(&LogOptions {
                    limit: 1,
                    revs: vec!["HEAD".to_owned()],
                    ..LogOptions::default()
                })?
                .into_iter()
                .next()
                .map(|e| e.summary)
                .unwrap_or_default();
            let pad = width - desc.chars().count();
            line = format!("{line}{}{reset} {} {subject}", " ".repeat(pad), oid(&tip));
        } else {
            line.push_str(reset);
        }
        lines.push(line);
    }
    for r in rows {
        let name = if r.remote && all {
            format!("remotes/{}", r.name)
        } else {
            r.name.clone()
        };
        let wt = worktrees.get(&r.detail.name).filter(|_| !r.current);
        let (mark, color) = if r.remote {
            ("  ", c("31"))
        } else if r.current {
            ("* ", c("32"))
        } else if wt.is_some() {
            ("+ ", c("36"))
        } else {
            ("  ", String::new())
        };
        let mut line = format!("{mark}{color}{name}");
        if opts.verbose == 0 {
            line.push_str(reset);
            if let Some(t) = &r.symref {
                line.push_str(&format!(" -> {t}"));
            }
            lines.push(line);
            continue;
        }
        line.push_str(&" ".repeat(width - name.chars().count()));
        line.push_str(reset);
        if let Some(t) = &r.symref {
            line.push_str(&format!(" -> {t}"));
            lines.push(line);
            continue;
        }
        line.push_str(&format!(" {} ", oid(&r.detail.id)));
        if opts.verbose > 1
            && let Some(path) = wt
        {
            line.push_str(&format!("({}{path}{reset}) ", c("36")));
        }
        if let Some((up, ahead, behind)) = &r.upstream {
            let mut counts = Vec::new();
            if *ahead > 0 {
                counts.push(format!("ahead {ahead}"));
            }
            if *behind > 0 {
                counts.push(format!("behind {behind}"));
            }
            let counts = counts.join(", ");
            line.push_str(&match (opts.verbose > 1, counts.is_empty()) {
                (true, true) => format!("[{}{up}{reset}] ", c("34")),
                (true, false) => format!("[{}{up}{reset}: {counts}] ", c("34")),
                (false, false) => format!("[{counts}] "),
                (false, true) => String::new(),
            });
        }
        line.push_str(&r.summary);
        lines.push(line);
    }
    Ok(lines)
}

/// Delete each of `names`, reporting every failure rather than stopping at
/// the first.
fn delete_branches(
    backend: &Arc<dyn GitBackend>,
    names: &[String],
    force: bool,
) -> anyhow::Result<String> {
    let mut deleted = String::new();
    let failed: Vec<String> = names
        .iter()
        .filter_map(|n| {
            let was = backend
                .rev_parse(&format!("refs/heads/{n}"))
                .and_then(|id| backend.abbrev_id(&id, 7))
                .unwrap_or_default();
            match backend.delete_branch(n, force) {
                Ok(()) => {
                    deleted.push_str(&format!("Deleted branch {n} (was {was}).\n"));
                    None
                }
                Err(e) => Some(format!("{n}: {e}")),
            }
        })
        .collect();
    if !failed.is_empty() {
        anyhow::bail!("could not delete {}", failed.join("; "));
    }
    if render::text_mode() {
        return Ok(deleted);
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
        if render::text_mode() {
            return Ok(String::new());
        }
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
    if o.create_reflog {
        ensure_reflog(backend, &format!("refs/heads/{name}"))?;
    }
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
    if render::text_mode() {
        return Ok(String::new());
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
    let guess = remote_head_guess(&heads, head);
    match guess.as_slice() {
        _ if !query => out.push("  HEAD branch: (not queried)".to_owned()),
        [] => out.push("  HEAD branch: (unknown)".to_owned()),
        [one] => out.push(format!("  HEAD branch: {one}")),
        many => {
            out.push(
                "  HEAD branch (remote HEAD is ambiguous, may be one of the following):".to_owned(),
            );
            out.extend(many.iter().map(|b| format!("    {b}")));
        }
    }

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
            Some("merges" | "m") => "rebases interactively (with merges) onto remote",
            Some(v) if !matches!(v, "false" | "no" | "off" | "0") => "rebases onto remote",
            _ => "merges with remote",
        };
        pulls.push((b, how, merge));
    }
    // A merging branch lines up with rebasing ones, as in git.
    if pulls.iter().any(|(_, how, _)| how.starts_with("rebases")) {
        for p in &mut pulls {
            if p.1 == "merges with remote" {
                p.1 = " merges with remote";
            }
        }
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
    let mut pushes = if query {
        push_states(backend, &specs, &heads)?
    } else if specs.is_empty() {
        vec![("(matching)".to_owned(), "(matching)".to_owned(), false, "")]
    } else {
        specs
            .iter()
            .filter(|s| !s.starts_with('^'))
            .map(|spec| {
                let forced = spec.starts_with('+');
                let spec = spec.trim_start_matches('+');
                let (src, dst) = spec.split_once(':').unwrap_or((spec, ""));
                let src = match (src, dst) {
                    ("", "") => "(matching)",
                    ("", _) => "(delete)",
                    (s, _) => s,
                };
                let dst = if dst.is_empty() { src } else { dst };
                (src.to_owned(), dst.to_owned(), forced, "")
            })
            .collect()
    };
    pushes.sort();
    if !pushes.is_empty() {
        out.push(format!(
            "  {} configured for 'git push'{not_queried}:",
            plural(pushes.len(), "Local ref", "Local refs")
        ));
        let w1 = pushes.iter().map(|(s, ..)| s.len()).max().unwrap_or(0);
        let w2 = pushes.iter().map(|(_, d, ..)| d.len()).max().unwrap_or(0);
        for (src, dst, forced, status) in &pushes {
            let verb = if *forced { "forces to" } else { "pushes to" };
            out.push(if status.is_empty() {
                format!("    {src:<w1$} {verb} {dst}")
            } else {
                format!("    {src:<w1$} {verb} {dst:<w2$} ({status})")
            });
        }
    }
    Ok(out.join("\n"))
}

/// The branches a remote's HEAD may be: the one it names, else every branch
/// at its commit, as git's guess_remote_head.
fn remote_head_guess(heads: &[(String, String)], head: Option<String>) -> Vec<String> {
    if let Some(h) = head {
        return vec![h];
    }
    let Some((_, id)) = heads.iter().find(|(n, _)| n == "HEAD") else {
        return Vec::new();
    };
    heads
        .iter()
        .filter(|(n, i)| i == id && n.starts_with("refs/heads/"))
        .map(|(n, _)| n["refs/heads/".len()..].to_owned())
        .collect()
}

/// What `git push` would do with each ref the push refspecs (or `:`, the
/// matching branches) pick, as git's get_push_ref_states: local name,
/// remote name, forced, and status against the remote's `heads`.
fn push_states(
    backend: &Arc<dyn GitBackend>,
    specs: &[&str],
    heads: &[(String, String)],
) -> anyhow::Result<Vec<(String, String, bool, &'static str)>> {
    let theirs: std::collections::HashMap<&str, &str> = heads
        .iter()
        .filter(|(n, _)| !n.ends_with("^{}"))
        .map(|(n, i)| (n.as_str(), i.as_str()))
        .collect();
    let ours: Vec<(String, String)> = backend
        .ref_details()?
        .into_iter()
        .filter(|r| r.symref.is_none())
        .map(|r| (r.name, r.id))
        .collect();
    let find = |name: &str| ours.iter().find(|(n, _)| n == name).map(|(_, i)| i.clone());
    let dwim = |s: &str| {
        [
            s.to_owned(),
            format!("refs/{s}"),
            format!("refs/tags/{s}"),
            format!("refs/heads/{s}"),
            format!("refs/remotes/{s}"),
        ]
        .into_iter()
        .find(|n| find(n).is_some())
    };
    let specs = if specs.is_empty() { &[":"][..] } else { specs };
    let mut picked: Vec<(String, String, String, bool)> = Vec::new();
    for spec in specs {
        let forced = spec.starts_with('+');
        let spec = spec.trim_start_matches('+');
        if spec.starts_with('^') {
            continue;
        }
        let (src, dst) = spec.split_once(':').unwrap_or((spec, ""));
        if src.is_empty() {
            if dst.is_empty() {
                for (n, id) in &ours {
                    if n.starts_with("refs/heads/") && theirs.contains_key(n.as_str()) {
                        picked.push((n.clone(), n.clone(), id.clone(), forced));
                    }
                }
            }
            continue;
        }
        if let Some((pre, post)) = src.split_once('*') {
            for (n, id) in &ours {
                if let Some(mid) = n
                    .strip_prefix(pre)
                    .and_then(|m| m.strip_suffix(post))
                    .filter(|_| n.len() >= pre.len() + post.len())
                {
                    let to = if dst.is_empty() { src } else { dst };
                    picked.push((n.clone(), to.replacen('*', mid, 1), id.clone(), forced));
                }
            }
            continue;
        }
        let (label, full) = if src == "HEAD" {
            let Some(full) = backend.symbolic_ref("HEAD")? else {
                continue;
            };
            ("HEAD".to_owned(), full)
        } else {
            let Some(full) = dwim(src) else { continue };
            (full.clone(), full)
        };
        let Some(id) = find(&full) else { continue };
        let to = if dst.is_empty() {
            full.clone()
        } else if dst.starts_with("refs/") {
            dst.to_owned()
        } else {
            ["refs/heads/", "refs/tags/"]
                .iter()
                .map(|p| format!("{p}{dst}"))
                .find(|n| theirs.contains_key(n.as_str()))
                .unwrap_or_else(|| {
                    let kind = if full.starts_with("refs/tags/") {
                        "refs/tags/"
                    } else {
                        "refs/heads/"
                    };
                    format!("{kind}{dst}")
                })
        };
        picked.push((label, to, id, forced));
    }
    let short = |s: &str| s.strip_prefix("refs/heads/").unwrap_or(s).to_owned();
    let mut out = Vec::new();
    for (src, dst, id, forced) in picked {
        let status = match theirs.get(dst.as_str()) {
            None => "create",
            Some(old) if *old == id => "up to date",
            Some(old) if backend.is_ancestor(old, &id).unwrap_or(false) => "fast-forwardable",
            Some(_) => "local out of date",
        };
        out.push((short(&src), short(&dst), forced, status));
    }
    Ok(out)
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
    let Some(n) = n.filter(|&n| n > 0) else {
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
    let annotated = message.is_some() || o.annotate || signed || !o.trailer.is_empty();
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
    if !o.trailer.is_empty() {
        message = message.map(|m| add_trailers(&m, &o.trailer));
    }
    if o.create_reflog {
        ensure_reflog(backend, &format!("refs/tags/{name}"))?;
    }
    ok(backend.tag_with(
        name,
        rev,
        message.as_deref(),
        o.cleanup.as_deref().unwrap_or("strip"),
        key.as_deref(),
        o.force,
    ))
}

/// `git verify-commit` / `git verify-tag`: each object's contents (with
/// -v) and the verifier's report; exit 1 unless every signature is good.
fn verify_signatures(
    backend: &Arc<dyn GitBackend>,
    revs: &[String],
    tag: bool,
    verbose: bool,
    raw: bool,
) -> String {
    let mut out = String::new();
    let mut failed = false;
    for rev in revs {
        let c = match backend.signature_check(rev, tag) {
            Ok(c) => c,
            Err(e) => {
                out.push_str(&format!("error: {e}\n"));
                failed = true;
                continue;
            }
        };
        if verbose {
            out.push_str(&String::from_utf8_lossy(&c.payload));
        }
        if c.result == 'N' && tag {
            out.push_str("error: no signature found\n");
        }
        out.push_str(if raw { &c.status } else { &c.output });
        failed |= !c.good;
    }
    set_exit(failed);
    out
}

/// `msg` with each `<token>: <value>` (or `=`) trailer appended, in its own
/// paragraph unless the last one is already trailers, as interpret-trailers
/// does.
fn add_trailers(msg: &str, trailers: &[String]) -> String {
    let mut body = msg.trim_end().to_owned();
    for t in trailers {
        let (k, v) = t.split_once([':', '=']).unwrap_or((t, ""));
        let last = body.rsplit("\n\n").next().unwrap_or("");
        let block = body.contains("\n\n")
            && last.lines().all(|l| {
                l.split_once(": ")
                    .is_some_and(|(k, _)| !k.is_empty() && !k.contains(' '))
            });
        let sep = match (body.is_empty(), block) {
            (true, _) => "",
            (false, true) => "\n",
            (false, false) => "\n\n",
        };
        body = format!("{body}{sep}{}: {}", k.trim(), v.trim());
    }
    body + "\n"
}

/// The repository's common git dir (the main one's, from a linked
/// worktree).
fn common_dir(backend: &Arc<dyn GitBackend>) -> PathBuf {
    let git_dir = backend.git_dir();
    std::fs::read_to_string(git_dir.join("commondir"))
        .map(|c| git_dir.join(c.trim()))
        .unwrap_or(git_dir)
}

/// Start an empty reflog for `refname` so its updates are logged, as git's
/// `--create-reflog` does.
fn ensure_reflog(backend: &Arc<dyn GitBackend>, refname: &str) -> anyhow::Result<()> {
    let path = common_dir(backend).join("logs").join(refname);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    Ok(())
}

/// `text` without its `#` lines.
/// git's strbuf_stripspace: no trailing spaces, no leading, trailing or
/// repeated blank lines, and a final newline.
fn stripspace(text: &str) -> String {
    let mut out = String::new();
    let mut blank = false;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

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
    run_editor(Some(&backend.git_dir()), &path)?;
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

/// Take out of `paths` the ones that match only paths outside the sparse
/// checkout, which git's add, rm and mv refuse without `--sparse`.
fn sparse_refused(
    backend: &Arc<dyn GitBackend>,
    paths: &mut Vec<String>,
    untracked: bool,
) -> anyhow::Result<Vec<String>> {
    let refused = rgit_git::outside_only(&backend.git_dir(), paths, untracked)?;
    paths.retain(|p| !refused.contains(p));
    Ok(refused)
}

/// git's advice for paths outside the sparse checkout; exits 1.
fn sparse_advice(paths: &[String]) -> anyhow::Error {
    let mut message = format!(
        "The following paths and/or pathspecs matched paths that exist\n\
         outside of your sparse-checkout definition, so will not be\n\
         updated in the index:\n{}",
        paths.join("\n")
    );
    let off =
        |v: Option<String>| v.is_some_and(|v| matches!(v.as_str(), "0" | "false" | "no" | "off"));
    if !off(rgit_git::config_get("advice.updateSparsePath"))
        && !off(std::env::var("GIT_ADVICE").ok())
    {
        message.push_str(
            "\nhint: If you intend to update such entries, try one of the following:\n\
             hint: * Use the --sparse option.\n\
             hint: * Disable or modify the sparsity rules.\n\
             hint: Disable this message with \"git config set advice.updateSparsePath false\"",
        );
    }
    anyhow::Error::new(CliError {
        message,
        help: None,
        code: 1,
    })
}

/// Move --pathspec-from-file's pathspecs (`-` for stdin; one per line, or
/// NUL-separated) into the command's paths, failing as git does.
fn take_pathspec_file(command: &mut Command) {
    let (file, paths, more) = match command {
        Command::Add {
            pathspec_file,
            paths,
            ..
        }
        | Command::Restore {
            pathspec_file,
            paths,
            ..
        }
        | Command::Commit {
            pathspec_file,
            paths,
            ..
        }
        | Command::Rm {
            pathspec_file,
            paths,
            ..
        }
        | Command::Stash {
            cmd:
                Some(StashCmd::Push {
                    push: StashPush { pathspec_file, .. },
                    paths,
                }),
            ..
        }
        | Command::Stash {
            cmd: None,
            push: StashPush { pathspec_file, .. },
            paths,
        } => (pathspec_file, paths, false),
        Command::Checkout {
            pathspec_file,
            pathspec,
            paths,
            ..
        }
        | Command::Reset {
            pathspec_file,
            pathspec,
            paths,
            ..
        } => (pathspec_file, paths, !pathspec.is_empty()),
        _ => return,
    };
    let Some(name) = file.pathspec_from_file.take() else {
        return;
    };
    let fatal = |msg: String| -> ! {
        eprintln!("fatal: {msg}");
        std::process::exit(128)
    };
    if more || !paths.is_empty() {
        fatal("'--pathspec-from-file' and pathspec arguments cannot be used together".into());
    }
    let text = if name == "-" {
        std::io::read_to_string(std::io::stdin())
    } else {
        std::fs::read_to_string(&name)
    }
    .unwrap_or_else(|e| {
        let why = e.to_string();
        let why = why.split(" (os error").next().unwrap_or(&why);
        fatal(format!("could not open '{name}' for reading: {why}"))
    });
    paths.extend(
        text.split(if file.pathspec_file_nul { '\0' } else { '\n' })
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .filter(|l| !l.is_empty())
            .map(str::to_owned),
    );
}

/// HEAD's commit id, or git's all-zero id before the first commit.
fn head_or_zero(backend: &Arc<dyn GitBackend>) -> String {
    backend.rev_parse("HEAD").unwrap_or_else(|_| "0".repeat(40))
}

/// Run the `post-checkout` hook after a checkout from `old`: `branch` for a
/// switch of HEAD, else a checkout of paths.
fn post_checkout(backend: &Arc<dyn GitBackend>, old: &str, branch: bool) -> anyhow::Result<()> {
    let new = head_or_zero(backend);
    let flag = if branch { "1" } else { "0" };
    let old = if branch { old } else { &new };
    post_hook(
        backend,
        backend.workdir(),
        "post-checkout",
        &[old, &new, flag],
    )
}

/// Run a hook git runs after a command (`post-checkout`, `post-merge`), in
/// `cwd`, its output on stderr as git shows it. A failing post-checkout hook
/// fails the command, as in git.
pub(crate) fn post_hook(
    backend: &Arc<dyn GitBackend>,
    cwd: &Path,
    name: &str,
    args: &[&str],
) -> anyhow::Result<()> {
    use std::io::Write;
    let Some(out) = rgit_git::run_hook(&backend.hooks_dir(), cwd, name, args, None)? else {
        return Ok(());
    };
    let mut err = std::io::stderr();
    let _ = err.write_all(&out.stdout);
    let _ = err.write_all(&out.stderr);
    if !out.status.success() && name == "post-checkout" {
        set_exit(true);
    }
    Ok(())
}

/// `git hook run`: the hook's output goes to stderr and its exit status is
/// ours; 1 when there is no such hook (0 with `ignore_missing`).
pub(crate) fn hook_run(
    backend: &Arc<dyn GitBackend>,
    ignore_missing: bool,
    to_stdin: Option<&str>,
    name: &str,
    args: &[String],
) -> i32 {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    let top = backend.workdir().to_path_buf();
    let hook = backend.hooks_dir().join(name);
    let meta = hook.metadata().ok().filter(|m| m.is_file());
    let runnable = meta
        .as_ref()
        .is_some_and(|m| m.permissions().mode() & 0o111 != 0);
    if meta.is_some()
        && !runnable
        && backend
            .config_get("advice.ignoredHook")
            .ok()
            .flatten()
            .is_none_or(|v| !matches!(v.as_str(), "false" | "no" | "off" | "0"))
    {
        let shown = hook.strip_prefix(&top).unwrap_or(&hook);
        eprintln!(
            "hint: The '{}' hook was ignored because it's not set as executable.\n\
             hint: You can disable this warning with `git config set advice.ignoredHook false`.",
            shown.display()
        );
    }
    if !runnable {
        if ignore_missing {
            return 0;
        }
        eprintln!("error: cannot find a hook named {name}");
        return 1;
    }
    let stdin = match to_stdin {
        Some(path) => match std::fs::File::open(top.join(path)) {
            Ok(f) => std::process::Stdio::from(f),
            Err(e) => {
                let e = e.to_string();
                let e = e.split(" (os error").next().unwrap_or_default();
                eprintln!("fatal: could not open '{path}' for reading: {e}");
                return 128;
            }
        },
        None => std::process::Stdio::null(),
    };
    let prefix = std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok())
        .zip(top.canonicalize().ok())
        .and_then(|(c, t)| c.strip_prefix(t).ok().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| format!("{}/", p.display()))
        .unwrap_or_default();
    let status = std::process::Command::new(&hook)
        .args(args)
        .current_dir(&top)
        .env("GIT_PREFIX", prefix)
        .stdin(stdin)
        .stdout(std::io::stderr())
        .status();
    match status {
        Ok(s) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
        Err(e) => {
            eprintln!("error: cannot run {}: {e}", hook.display());
            1
        }
    }
}

/// `out`, or nothing with -q.
/// git's `Dropped refs/stash@{<i>} (<id>)` line, for human output.
fn stash_dropped(backend: &Arc<dyn GitBackend>, i: usize) -> Option<String> {
    let id = backend.rev_parse(&format!("refs/stash@{{{i}}}")).ok()?;
    render::text_mode().then(|| format!("Dropped refs/stash@{{{i}}} ({id})\n"))
}

/// After a stash is applied, git prints the long status, then any drop line.
fn stash_applied(
    backend: &Arc<dyn GitBackend>,
    out: String,
    dropped: Option<String>,
) -> anyhow::Result<String> {
    if !render::text_mode() {
        return Ok(out);
    }
    let mut opts = rgit_git::StatusOpts {
        format: Some(rgit_git::StatusFormat::Long),
        ..Default::default()
    };
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    status_env(backend, &mut opts, tty && !render::color_on(), tty);
    let status = String::from_utf8_lossy(&backend.status_text(&opts)?.text).into_owned();
    Ok(status + &dropped.unwrap_or_default())
}

fn quietly(quiet: bool, out: String) -> String {
    if quiet { String::new() } else { out }
}

/// `git stash list`: `git log -g refs/stash` in the `%gd: %gs` format or
/// the one asked for, with each stash's diff against its base when asked.
fn stash_log(
    backend: &Arc<dyn GitBackend>,
    mut args: PrettyArgs,
    format: DiffFormat,
    max: Option<usize>,
) -> anyhow::Result<String> {
    if backend.status()?.stashes.is_empty() {
        return Ok(String::new());
    }
    if args.format.is_none() && args.pretty.is_none() && !args.oneline {
        args.format = Some("%gd: %gs".to_owned());
    }
    let pretty = crate::pretty::Pretty::new(backend, &args, None)?.expect("a format");
    let (mut commits, mut diffs) = (Vec::new(), Vec::new());
    let stashes = reflog_walk(backend, &["refs/stash".to_owned()], args.date.as_deref())?;
    for c in stashes.into_iter().take(max.unwrap_or(usize::MAX)) {
        diffs.push(if format.any() {
            let files = backend.diff(&rgit_git::DiffSpec {
                from: c.parents.first().cloned(),
                to: Some(c.id.clone()),
                ..Default::default()
            })?;
            vec![(None, Some(Changes::Files(files)))]
        } else {
            vec![(None, None)]
        });
        commits.push(c);
    }
    Ok(pretty_log(&pretty, &commits, &diffs, format))
}

/// Status limited to `paths`, with untracked files per git's `-u` mode and
/// ignored files when asked.
pub(crate) fn status_view(
    backend: &Arc<dyn GitBackend>,
    paths: &[String],
    untracked: Option<&str>,
    ignored: Option<&str>,
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
    if ignored.is_some_and(|m| m != "no") {
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

/// Fill in what git's status reads from its surroundings: the current
/// folder, the terminal and its width.
pub fn status_env(
    backend: &Arc<dyn GitBackend>,
    opts: &mut rgit_git::StatusOpts,
    no_color: bool,
    tty: bool,
) {
    opts.prefix = crate::plumbing::top_and_prefix(backend)
        .map(|(_, p)| p)
        .unwrap_or_default();
    opts.tty = tty;
    if no_color {
        opts.color = Some(false);
    }
    opts.width = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n: &usize| *n > 0)
        .or_else(|| {
            tty.then(crossterm::terminal::size)
                .and_then(Result::ok)
                .map(|(w, _)| usize::from(w))
        })
        .unwrap_or(80);
}

/// git's `add -e`: the unstaged diff (7 lines of context) in the editor,
/// then what is left of it applied to the index.
fn add_edit(backend: &Arc<dyn GitBackend>, paths: &[String]) -> anyhow::Result<String> {
    let path = backend.git_dir().join("ADD_EDIT.patch");
    std::fs::write(
        &path,
        backend.patch_diff(None, false, false, Some(7), paths)?,
    )?;
    crate::interactive::launch_editor(backend, &path)
        .map_err(|_| anyhow::anyhow!("editing patch failed"))?;
    let patch = std::fs::read(&path)?;
    if patch.is_empty() {
        anyhow::bail!("empty patch. aborted");
    }
    let opts = rgit_git::ApplyOpts {
        cached: true,
        recount: true,
        quiet: true,
        ..Default::default()
    };
    rgit_git::parse_patch(&patch, &opts)
        .and_then(|files| backend.apply_patch(&files, &opts))
        .map_err(|e| anyhow::anyhow!("could not apply '{}': {e}", path.display()))?;
    let _ = std::fs::remove_file(&path);
    Ok(String::new())
}

/// git's stripspace on a message, dropping `#` lines too with `comments`.
pub(crate) fn clean_message(text: &str, comments: bool) -> String {
    String::from_utf8_lossy(&rgit_git::stripspace(
        text.as_bytes(),
        comments.then_some("#"),
    ))
    .into_owned()
}

/// `~/x` as a path under the home folder, as git reads commit.template.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// The part of git's commit message template below the message: the
/// instructions, the author and date when they are not the committer's
/// own, and the `#` commented status (with the diff under `-v`).
fn commit_template_status(
    backend: &Arc<dyn GitBackend>,
    opts: &mut rgit_git::StatusOpts,
    author: Option<&str>,
    reuse_from: Option<&str>,
    date: Option<&str>,
) -> anyhow::Result<String> {
    let git_dir = backend.git_dir();
    let mut out = String::new();
    let whence = if git_dir.join("MERGE_HEAD").exists() {
        Some(("merge", "MERGE_HEAD"))
    } else if backend.rev_parse("CHERRY_PICK_HEAD").is_ok() {
        Some(("cherry-pick", "CHERRY_PICK_HEAD"))
    } else {
        None
    };
    if let Some((what, head)) = whence {
        out.push_str(&format!(
            "#\n# It looks like you may be committing a {what}.\n\
             # If this is not correct, please run\n\
             #\tgit update-ref -d {head}\n# and try again.\n\n"
        ));
    }
    out.push_str(
        "\n# Please enter the commit message for your changes. Lines starting\n\
         # with '#' will be ignored, and an empty message aborts the commit.\n",
    );
    let committer = crate::pretty::ident(&backend.ident(true)?);
    let mut author_ident = match (author, reuse_from) {
        (Some(a), _) if a.contains('<') => crate::pretty::ident(&format!("{a} 0 +0000")),
        (_, Some(rev)) => crate::pretty::parse(&backend.read_object(rev)?).author,
        _ => committer.clone(),
    };
    if author.is_some_and(|a| !a.contains('<')) {
        author_ident = committer.clone();
    }
    let mut shown = false;
    let mut line = |out: &mut String, text: String| {
        if !shown {
            out.push_str("#\n");
            shown = true;
        }
        out.push_str(&text);
    };
    if (author_ident.name.as_str(), author_ident.email.as_str())
        != (committer.name.as_str(), committer.email.as_str())
    {
        line(
            &mut out,
            format!(
                "# Author:    {} <{}>\n",
                author_ident.name, author_ident.email
            ),
        );
    }
    let when = match date {
        Some(d) => Some(parse_git_date(d)?),
        None => reuse_from.map(|_| (author_ident.time, author_ident.offset)),
    };
    if let Some((secs, offset)) = when {
        line(
            &mut out,
            format!(
                "# Date:      {}\n",
                crate::pretty::format_date(secs, offset, "default")
            ),
        );
    }
    out.push_str("#\n");
    opts.template = true;
    opts.nowarn = true;
    status_env(backend, opts, true, false);
    let report = backend.status_text(opts)?;
    out.push_str(&String::from_utf8_lossy(&report.text));
    Ok(out)
}

/// Rewrite path arguments typed in a subfolder of the repo into repo-root
/// paths, since git reads pathspecs relative to the current folder.
pub fn from_cwd(mut command: Command, backend: &Arc<dyn GitBackend>) -> Command {
    take_pathspec_file(&mut command);
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
        Command::Archive(a) => {
            a.cwd = prefix
                .components()
                .map(|c| format!("{}/", c.as_os_str().to_string_lossy()))
                .collect();
        }
        Command::Blame { args, .. } => args.last_mut().into_iter().for_each(fix),
        Command::Show { paths, .. } => paths.iter_mut().for_each(fix),
        Command::Log { revs, paths, .. }
        | Command::Diff {
            revs,
            paths,
            no_index: false,
            ..
        } => {
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
            | Plumbing::Grep {
                paths,
                no_index: false,
                ..
            }
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
    rgit_git::relocate_pathspec(p, |p| plain_repo_path(root, prefix, p))
}

fn plain_repo_path(root: &Path, prefix: &Path, p: &str) -> String {
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
    diff_out_in(files, format, 0)
}

/// A file's line of git's `--raw` format, ids abbreviated as its patch
/// header's `index` line is.
fn raw_line(f: &rgit_git::FileDiff) -> String {
    let n = f
        .header
        .lines()
        .find_map(|l| l.strip_prefix("index "))
        .and_then(|r| r.split("..").next())
        .map_or(7, str::len);
    let abbrev = |id: &str| {
        if id.bytes().all(|b| b == b'0') {
            "0".repeat(n)
        } else {
            id[..n.min(id.len())].to_owned()
        }
    };
    let (score, path) = match &f.old_path {
        Some(old) => (format!("{:03}", f.similarity), format!("{old}\t{}", f.path)),
        None => (String::new(), f.path.clone()),
    };
    format!(
        ":{:06o} {:06o} {} {} {}{score}\t{path}",
        f.modes.0,
        f.modes.1,
        abbrev(&f.ids.0),
        abbrev(&f.ids.1),
        f.status.letter()
    )
}

/// [`diff_out`] with a `--stat` `indent` columns narrower, for a graph beside it.
fn diff_out_in(files: &[rgit_git::FileDiff], format: DiffFormat, indent: usize) -> String {
    let r = crate::diffopts::current();
    if r.check {
        let (text, failed) = crate::diffcolor::check(files, &r, render::color_on());
        if failed {
            set_exit_code(exit_code() | 2);
        }
        return text.trim_end_matches('\n').to_owned();
    }
    let format = DiffFormat {
        stat: format.stat || r.compact_summary,
        ..format
    };
    if let Some(ds) = r.dirstat {
        let stats = DiffFormat {
            patch: false,
            ..format
        };
        let mut out = String::new();
        let stated = stats.stat || stats.numstat || stats.shortstat;
        if stated {
            out.push_str(diff_out_plain(files, stats, indent).trim_end_matches('\n'));
            out.push('\n');
        }
        out.push_str(&crate::diffcolor::dirstat(files, &r, ds));
        if format.patch && !files.is_empty() {
            // Only a stat block (a dirstat by lines is part of it) ends in
            // a blank line before the patch.
            if stated || ds.0 == 1 {
                out.push('\n');
            }
            out.push_str(&render::patch(files));
        }
        return out.trim_end_matches('\n').to_owned();
    }
    diff_out_plain(files, format, indent)
}

fn diff_out_plain(files: &[rgit_git::FileDiff], format: DiffFormat, indent: usize) -> String {
    let rows = |row: &dyn Fn(&rgit_git::FileDiff) -> String| {
        files.iter().map(row).collect::<Vec<_>>().join("\n")
    };
    if format.raw {
        let raw = rows(&raw_line);
        let rest = diff_out_in(
            files,
            DiffFormat {
                raw: false,
                ..format
            },
            indent,
        );
        return if rest.is_empty()
            || !(DiffFormat {
                raw: false,
                ..format
            })
            .any()
        {
            raw
        } else {
            format!("{raw}\n\n{rest}")
        };
    }
    if format.name_only {
        rows(&|f| f.path.clone())
    } else if format.name_status {
        rows(&|f| match &f.old_path {
            // `--no-index` pairs two names without a rename.
            Some(old) if f.similarity == 0 => format!("{}\t{old}", f.status.letter()),
            Some(old) => format!(
                "{}{:03}\t{old}\t{}",
                f.status.letter(),
                f.similarity,
                f.path
            ),
            // A rewrite (-B) carries its dissimilarity.
            None if f.similarity > 0 => {
                format!("{}{:03}\t{}", f.status.letter(), f.similarity, f.path)
            }
            None => format!("{}\t{}", f.status.letter(), f.path),
        })
    } else if format.numstat {
        rows(&|f| {
            let (add, del) = crate::axi::line_counts(f);
            let path = match &f.old_path {
                Some(old) => render::rename_name(old, &f.path),
                None => f.path.clone(),
            };
            if f.binary {
                format!("-\t-\t{path}")
            } else {
                format!("{add}\t{del}\t{path}")
            }
        })
    } else if format.shortstat {
        if files.is_empty() {
            String::new()
        } else {
            render::stat_summary(files)
        }
    } else if format.patch && format.stat {
        format!(
            "{}\n\n{}",
            render::stat(files, indent),
            render::patch(files)
        )
    } else if format.patch {
        render::patch(files)
    } else if format.stat {
        if files.is_empty() {
            String::new()
        } else {
            render::stat(files, indent)
        }
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
        committer,
        occurrences,
        changes_matching,
        since,
        until,
        grep,
        ignore_case,
        first_parent,
        merges,
        no_merges,
        reverse,
        follow,
        pretty,
        walk,
        parents,
        revs,
        paths,
        line_ranges,
        ..
    } = command
    else {
        anyhow::bail!("not a log command");
    };
    let (revs, paths) = split_revs(backend, revs, paths)?;
    let mut opts = LogOptions {
        limit: limit.unwrap_or(if pretty.any() || !line_ranges.is_empty() {
            usize::MAX
        } else {
            20
        }),
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
        rewrite_parents: pretty.graph || *parents,
        committer: committer.clone(),
        occurrences: occurrences.clone(),
        changes_matching: changes_matching.clone(),
        order: if pretty.graph {
            rgit_git::LogOrder::Topo
        } else {
            rgit_git::LogOrder::Walk
        },
        ..LogOptions::default()
    };
    walk.apply(&mut opts);
    Ok(opts)
}

/// What `log -p` or `show` prints under one commit.
pub(crate) enum Changes {
    Files(Vec<rgit_git::FileDiff>),
    /// A merge's combined diff; `first` is the diff against the first
    /// parent, for a stat.
    Combined {
        first: Vec<rgit_git::FileDiff>,
        files: Vec<rgit_git::CombinedFile>,
    },
    /// A line git prints in place of a diff.
    Warning(&'static str),
    /// `log -L`'s diffs of the tracked lines, after a blank line even for a
    /// merge that shows none.
    LineLog(Vec<rgit_git::FileDiff>),
}

/// One commit's diff sections as git's log prints them, each `(from,
/// changes)`: several for a merge under `-m`, each against the parent `from`;
/// `changes` None leaves the diff part out.
type Sections = Vec<(Option<String>, Option<Changes>)>;

/// The merge diff mode the flags ask for, else `default`, and the diff
/// format with the -p it may imply.
fn merge_mode(
    args: &MergeDiffArgs,
    default: MergeDiff,
    format: DiffFormat,
) -> anyhow::Result<(MergeDiff, DiffFormat)> {
    let (mode, imply) = args.resolve(default)?;
    let format = if imply && !format.any() {
        DiffFormat {
            patch: true,
            ..DiffFormat::default()
        }
    } else {
        format
    };
    Ok((mode, format))
}

/// Commit `id`'s changes in `format`, a merge's as `mode` asks.
fn commit_changes(
    backend: &Arc<dyn GitBackend>,
    id: &str,
    parents: &[String],
    paths: &[String],
    mode: MergeDiff,
    format: DiffFormat,
) -> anyhow::Result<Sections> {
    if !format.any() {
        return Ok(vec![(None, None)]);
    }
    let diff = |from: Option<&String>| {
        backend.diff(&rgit_git::DiffSpec {
            from: from.cloned(),
            to: Some(id.to_owned()),
            paths: paths.to_vec(),
            function_context: function_context(),
            ..Default::default()
        })
    };
    let one = |files: Vec<rgit_git::FileDiff>| {
        vec![(None, (!files.is_empty()).then_some(Changes::Files(files)))]
    };
    if parents.len() < 2 {
        return Ok(one(diff(parents.first())?));
    }
    Ok(match mode {
        MergeDiff::Off => vec![(None, None)],
        MergeDiff::FirstParent => one(diff(parents.first())?),
        MergeDiff::Separate => {
            let mut out = Vec::new();
            for p in parents {
                let files = diff(Some(p))?;
                if !files.is_empty() {
                    out.push((Some(p.clone()), Some(Changes::Files(files))));
                }
            }
            if out.is_empty() {
                out.push((None, None));
            }
            out
        }
        MergeDiff::Combined | MergeDiff::Dense => {
            let first = if format.stat || format.numstat || format.shortstat {
                diff(parents.first())?
            } else {
                Vec::new()
            };
            let files = backend.combined_diff(id, paths, mode == MergeDiff::Dense)?;
            vec![(None, Some(Changes::Combined { first, files }))]
        }
        MergeDiff::Remerge if parents.len() > 2 => vec![(
            None,
            Some(Changes::Warning(
                "diff: warning: Skipping remerge-diff for octopus merges.",
            )),
        )],
        MergeDiff::Remerge => one(backend.remerge_diff(id, paths)?),
    })
}

/// A combined diff in `format`, a stat `indent` columns narrower.
fn combined_out(
    first: &[rgit_git::FileDiff],
    files: &[rgit_git::CombinedFile],
    format: DiffFormat,
    indent: usize,
) -> String {
    let mut out = String::new();
    if format.name_only || format.name_status {
        for f in files {
            if format.name_status {
                out.extend(&f.status);
                out.push('\t');
            }
            out.push_str(&f.path);
            out.push('\n');
        }
    } else if format.stat || format.numstat || format.shortstat {
        let stat = DiffFormat {
            patch: false,
            ..format
        };
        out.push_str(diff_out_in(first, stat, indent).trim_end_matches('\n'));
        if !out.is_empty() {
            out.push('\n');
        }
    }
    if format.patch {
        if !out.is_empty() {
            out.push('\n');
        }
        for f in files {
            out.push_str(&render::combined_patch(&f.patch, f.status.len()));
        }
    }
    out
}

/// A commit's changes as newline-terminated lines, a stat `indent` columns
/// narrower.
fn changes_out(changes: &Changes, format: DiffFormat, indent: usize) -> String {
    let text = match changes {
        Changes::Files(files) => diff_out_in(files, format, indent),
        // A combined diff keeps the blank line after its stat even with no
        // patch to follow.
        Changes::Combined { first, files } => return combined_out(first, files, format, indent),
        Changes::Warning(text) => text.to_string(),
        Changes::LineLog(files) => {
            // line-log colors an added line whole, without git diff's
            // whitespace-error split.
            let color = render::color_on();
            let mut files = files.clone();
            for l in files
                .iter_mut()
                .flat_map(|f| &mut f.hunks)
                .flat_map(|h| &mut h.lines)
            {
                if l.origin == rgit_git::LineOrigin::Added {
                    l.origin = rgit_git::LineOrigin::Meta;
                    l.text = if color {
                        format!("\x1b[32m+{}", l.text)
                    } else {
                        format!("+{}", l.text)
                    };
                }
            }
            render::patch(&files)
        }
    };
    let text = text.trim_end_matches('\n');
    if text.is_empty() {
        String::new()
    } else {
        format!("{text}\n")
    }
}

/// The blank line (or `---`) git puts between a commit's message and its
/// changes.
fn diff_separator(
    pretty: &crate::pretty::Pretty,
    changes: &Changes,
    format: DiffFormat,
) -> &'static str {
    match changes {
        Changes::Combined { .. } if pretty.blank_before_diff(true) => "\n",
        Changes::LineLog(_) => "\n",
        Changes::Files(_) if pretty.blank_before_diff(false) => {
            if format.patch && format.stat {
                "---\n"
            } else {
                "\n"
            }
        }
        _ => "",
    }
}

/// Each log entry's changes as git's log prints them (see [`commit_changes`]),
/// with `--follow` only the followed file, under the name it had in that
/// commit.
fn log_diffs(
    backend: &Arc<dyn GitBackend>,
    opts: &LogOptions,
    entries: &[rgit_git::LogEntry],
    format: DiffFormat,
    mode: MergeDiff,
) -> anyhow::Result<Vec<Sections>> {
    let mut out: Vec<Sections> = (0..entries.len()).map(|_| vec![(None, None)]).collect();
    if !format.any() {
        return Ok(out);
    }
    let mut follow = opts.paths.first().filter(|_| opts.follow).cloned();
    let mut order: Vec<usize> = (0..entries.len()).collect();
    if opts.reverse {
        order.reverse();
    }
    for i in order {
        let e = &entries[i];
        let Some(path) = &mut follow else {
            out[i] = commit_changes(backend, &e.oid, &e.parents, &opts.paths, mode, format)?;
            continue;
        };
        if e.parents.len() > 1 {
            continue;
        }
        let mut files = backend.diff(&rgit_git::DiffSpec {
            from: e.parents.first().cloned(),
            to: Some(e.oid.clone()),
            function_context: function_context(),
            ..Default::default()
        })?;
        files.retain(|f| f.path == *path);
        if let Some(old) = files.first().and_then(|f| f.old_path.clone()) {
            *path = old;
        }
        if !files.is_empty() {
            out[i] = vec![(None, Some(Changes::Files(files)))];
        }
    }
    Ok(out)
}

/// `log -g`: the commits reflog entries name, newest first, each with its
/// entry's selector and message.
/// A ref name as git shortens it for `%gd`: `refs/heads/main` is `main`.
fn short_ref(name: &str) -> &str {
    ["refs/heads/", "refs/tags/", "refs/remotes/", "refs/"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .unwrap_or(name)
}

pub(crate) fn reflog_log(
    backend: &Arc<dyn GitBackend>,
    command: &Command,
) -> anyhow::Result<String> {
    let Command::Log {
        limit,
        skip,
        revs,
        pretty,
        format,
        walk,
        ..
    } = command
    else {
        anyhow::bail!("not a log command");
    };
    let mut p = crate::pretty::Pretty::new(backend, pretty, Some("medium"))?.expect("a format");
    p.walk = walk.clone();
    let names = if revs.is_empty() {
        vec!["HEAD".to_owned()]
    } else {
        revs.clone()
    };
    let commits = reflog_walk(backend, &names, pretty.date.as_deref())?;
    let commits: Vec<_> = commits
        .into_iter()
        .skip(*skip)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let mut diffs = Vec::new();
    for c in &commits {
        diffs.push(commit_changes(
            backend,
            &c.id,
            &c.parents,
            &[],
            MergeDiff::Off,
            *format,
        )?);
    }
    Ok(pretty_log(&p, &commits, &diffs, *format))
}

/// git's reflog walk over `names` (`ref`, `ref@{n}` or `ref@{date}`): the
/// entries of all the reflogs, newest first by when they were written, each
/// named by its index or, for a date selector or an explicit `date` style,
/// its date.
fn reflog_walk(
    backend: &Arc<dyn GitBackend>,
    names: &[String],
    date: Option<&str>,
) -> anyhow::Result<Vec<crate::pretty::Commit>> {
    struct Log {
        name: String,
        items: Vec<rgit_git::ReflogItem>,
        next: usize,
        by_date: bool,
    }
    let mut logs = Vec::new();
    for name in names {
        let (base, sel) = match name.strip_suffix('}').and_then(|n| n.split_once("@{")) {
            Some((base, sel)) => (if base.is_empty() { "HEAD" } else { base }, Some(sel)),
            None => (name.as_str(), None),
        };
        let items = backend.reflog(base)?;
        let (next, by_date) = match sel.map(|s| (s, s.parse::<usize>())) {
            None => (0, false),
            Some((_, Ok(n))) => (n, false),
            Some((sel, Err(_))) => {
                let at = parse_date(sel)?;
                let n = items.iter().position(|i| i.who.time <= at);
                (n.unwrap_or(usize::MAX), true)
            }
        };
        logs.push(Log {
            name: base.to_owned(),
            items,
            next,
            by_date,
        });
    }
    let mut commits = Vec::new();
    loop {
        let mut best: Option<usize> = None;
        for (k, log) in logs.iter().enumerate() {
            let Some(item) = log.items.get(log.next) else {
                continue;
            };
            if best.is_none_or(|b| item.who.time > logs[b].items[logs[b].next].who.time) {
                best = Some(k);
            }
        }
        let Some(k) = best else { break };
        let log = &mut logs[k];
        let (i, item) = (log.next, log.items[log.next].clone());
        log.next += 1;
        let sel = if log.by_date || date.is_some() {
            crate::pretty::format_date(item.who.time, item.who.offset, date.unwrap_or("default"))
        } else {
            i.to_string()
        };
        let mut c = crate::pretty::parse(&backend.read_object(&item.id)?);
        c.reflog = Some(crate::pretty::Reflog {
            selector: format!("{}@{{{sel}}}", log.name),
            short: format!("{}@{{{sel}}}", short_ref(&log.name)),
            who: item.who,
            message: item.message.trim_end().to_owned(),
        });
        commits.push(c);
    }
    Ok(commits)
}

/// Commits in a git format, each followed by its changes, as `git log` prints them.
fn pretty_log(
    pretty: &crate::pretty::Pretty,
    commits: &[crate::pretty::Commit],
    diffs: &[Sections],
    format: DiffFormat,
) -> String {
    let mut out = String::new();
    let sections = commits
        .iter()
        .zip(diffs)
        .flat_map(|(c, s)| s.iter().map(move |s| (c, s)));
    for (i, (c, (from, changes))) in sections.enumerate() {
        if i > 0 && !pretty.terminator {
            out.push('\n');
        }
        let mut c = c.clone();
        c.from = from.clone();
        out.push_str(&pretty.show(&c));
        if pretty.terminator {
            out.push('\n');
        }
        if let Some(changes) = changes {
            out.push_str(diff_separator(pretty, changes, format));
            out.push_str(&changes_out(changes, format, 0));
        }
    }
    out
}

/// `git log -L`: the commits that changed the ranges, in git's topological
/// order, each with the ranges' diff.
fn line_log(
    backend: &Arc<dyn GitBackend>,
    opts: &LogOptions,
    pretty: &PrettyArgs,
    specs: &[String],
) -> anyhow::Result<String> {
    let entries = backend.log(&LogOptions {
        limit: usize::MAX,
        offset: 0,
        reverse: false,
        order: rgit_git::LogOrder::Topo,
        ..opts.clone()
    })?;
    let order: Vec<String> = entries.iter().map(|e| e.oid.clone()).collect();
    let tip = opts
        .revs
        .iter()
        .find(|r| !r.starts_with('^'))
        .map_or("HEAD", |r| r.rsplit("..").next().unwrap_or(r));
    let tip = if tip.is_empty() { "HEAD" } else { tip };
    let shown = backend.line_log(tip, &order, specs, opts.first_parent)?;
    let mut picked: Vec<(String, Vec<rgit_git::FileDiff>)> = order
        .into_iter()
        .zip(shown)
        .filter_map(|(id, files)| files.map(|f| (id, f)))
        .skip(opts.offset)
        .take(opts.limit)
        .collect();
    if opts.reverse {
        picked.reverse();
    }
    let pretty = crate::pretty::Pretty::new(backend, pretty, Some("medium"))?.expect("a format");
    let mut commits = Vec::new();
    let mut diffs = Vec::new();
    for (id, files) in picked {
        commits.push(crate::pretty::parse(&backend.read_object(&id)?));
        diffs.push(vec![(None, Some(Changes::LineLog(files)))]);
    }
    let format = DiffFormat {
        patch: true,
        ..DiffFormat::default()
    };
    Ok(pretty_log(&pretty, &commits, &diffs, format))
}

/// [`pretty_log`] with git's `--graph` drawn to the left of every line.
fn graph_log(
    pretty: &crate::pretty::Pretty,
    commits: &[crate::pretty::Commit],
    parents: &[Vec<String>],
    diffs: &[Sections],
    format: DiffFormat,
) -> String {
    let mut graph = crate::graph::Graph::new();
    let mut out = String::new();
    let mut missing_newline = false;
    let mut shown = false;
    for (i, c) in commits.iter().enumerate() {
        for (k, (from, changes)) in diffs[i].iter().enumerate() {
            // Later `-m` sections of a merge continue below its graph line.
            if k == 0 {
                graph.update(&c.id, parents[i].clone());
                graph.mark = pretty.graph_mark(c);
            }
            if shown && !pretty.terminator {
                if !missing_newline {
                    out.push_str(&graph.padding_line());
                }
                out.push('\n');
            }
            shown = true;
            let mut c = c.clone();
            c.from = from.clone();
            out.push_str(&graph.show_commit());
            let (head, msg) = pretty.parts(&c);
            out.push_str(&head);
            if head.ends_with('\n') {
                out.push_str(&graph.oneline());
            }
            out.push_str(&graph.commit_msg(&msg));
            missing_newline = !msg.ends_with('\n');
            if pretty.terminator {
                if !missing_newline {
                    out.push_str(&graph.padding_line());
                }
                out.push('\n');
            }
            if let Some(changes) = changes {
                let prefix = graph.padding_line();
                let sep = diff_separator(pretty, changes, format);
                if !sep.is_empty() {
                    out.push_str(&prefix);
                    out.push_str(sep);
                }
                let text = changes_out(changes, format, prefix.chars().count());
                let combined = matches!(changes, Changes::Combined { .. });
                for line in text.lines() {
                    // git prints a combined diff's `mode a,b..c` line without
                    // the graph.
                    if !(combined && line.starts_with("mode ")) {
                        out.push_str(&prefix);
                    }
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out
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
        exit_code,
        quiet,
        no_index,
        reverse,
        diff_filter,
        ..
    } = command
    else {
        anyhow::bail!("not a diff command");
    };
    let spec = |from, to, paths| rgit_git::DiffSpec {
        from,
        to,
        cached: *cached,
        paths,
        context: *unified,
        ignore_all_space: *ignore_all_space,
        ignore_space_change: *ignore_space_change,
        reverse: *reverse,
        function_context: function_context(),
    };
    if *no_index {
        let [a, b] = &[&revs[..], &paths[..]].concat()[..] else {
            return Err(CliError::usage("diff --no-index takes two paths"));
        };
        let files = rgit_git::diff_no_index(Path::new(a), Path::new(b), &spec(None, None, vec![]))?;
        set_exit(!files.is_empty());
        return Ok((files, format!("{a}..{b}")));
    }
    let (revs, paths) = split_revs(backend, revs, paths)?;
    let side = if *cached { "index" } else { "working tree" };
    let (from, to, scope) = match &revs[..] {
        [] if *cached => (None, None, "staged".to_owned()),
        [] => (None, None, "unstaged".to_owned()),
        [range] if range.contains("..") => (Some(range.clone()), None, range.clone()),
        [rev] if rgit_git::range_base(rev) != rev.as_str() && !rev.ends_with("^@") => {
            let base = rgit_git::range_base(rev);
            let n = rev.rsplit_once("^-").map_or("1", |(_, n)| n);
            let n = if n.is_empty() { "1" } else { n };
            let from = format!("{base}^{n}");
            let scope = format!("{from}..{base}");
            (Some(from), Some(base.to_owned()), scope)
        }
        [rev] => (Some(rev.clone()), None, format!("{rev}..{side}")),
        [from, to] => (
            Some(from.clone()),
            Some(to.clone()),
            format!("{from}..{to}"),
        ),
        _ => return Err(CliError::usage("diff takes at most two revisions")),
    };
    let mut files = backend.diff(&spec(from, to, paths))?;
    if let Some(filter) = diff_filter {
        let letter = |f: &rgit_git::FileDiff| f.status.letter().chars().next().unwrap_or('M');
        let want: Vec<char> = filter.chars().filter(char::is_ascii_uppercase).collect();
        files.retain(|f| {
            let l = letter(f);
            (want.is_empty() || want.contains(&l)) && !filter.contains(l.to_ascii_lowercase())
        });
    }
    if *exit_code || *quiet {
        set_exit(!files.is_empty());
    }
    Ok((files, scope))
}

static EXIT_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// A verify's complaints on stderr, failing the run as git exits 1.
fn complaints(errors: Vec<String>) -> String {
    for e in &errors {
        eprintln!("{e}");
    }
    if !errors.is_empty() {
        set_exit_code(1);
    }
    String::new()
}

/// A size as git's options take it: bytes, or with a k, m or g suffix.
fn parse_size(v: &str) -> Result<u64, String> {
    let lower = v.to_ascii_lowercase();
    let (num, unit) = match lower.char_indices().last() {
        Some((i, 'k')) => (&lower[..i], 1 << 10),
        Some((i, 'm')) => (&lower[..i], 1 << 20),
        Some((i, 'g')) => (&lower[..i], 1 << 30),
        _ => (lower.as_str(), 1),
    };
    num.parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(unit))
        .ok_or_else(|| format!("invalid size '{v}'"))
}

/// `rgit fsck`'s report; a problem sets the exit code as git's does.
pub fn fsck(
    backend: &Arc<dyn GitBackend>,
    command: Command,
) -> anyhow::Result<rgit_git::FsckReport> {
    let Command::Fsck {
        no_full,
        strict,
        unreachable,
        no_dangling,
        connectivity_only,
        lost_found,
        name_objects,
        root,
        tags,
        cache,
        no_reflogs,
        objects,
        ..
    } = command
    else {
        unreachable!("fsck takes fsck's arguments")
    };
    let report = backend.fsck(&rgit_git::FsckOptions {
        full: !no_full,
        strict,
        unreachable,
        dangling: !no_dangling,
        connectivity_only,
        lost_found,
        name_objects,
        root,
        tags,
        cache,
        reflogs: !no_reflogs,
        objects,
    })?;
    EXIT_CODE.store(report.code, std::sync::atomic::Ordering::Relaxed);
    Ok(report)
}

/// Make a successful run exit 1, as `diff --exit-code` does on differences.
pub(crate) fn set_exit(differs: bool) {
    set_exit_code(i32::from(differs));
}

/// The exit status of a run that succeeds, as `merge-file`'s conflict count.
pub(crate) fn set_exit_code(code: i32) {
    EXIT_CODE.store(code, std::sync::atomic::Ordering::Relaxed);
}

/// The exit status of a run that succeeded.
pub fn exit_code() -> i32 {
    EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed)
}

static AS_IS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Print the text without the usual final newline, as `log --format=format:`
/// does in git.
pub(crate) fn print_as_is() {
    AS_IS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the text is printed exactly as it is.
pub fn text_as_is() -> bool {
    AS_IS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Split `log`/`diff` arguments into revisions and paths as git does: the
/// leading ones that name revisions, then paths (and everything after `--`).
fn split_revs(
    backend: &Arc<dyn GitBackend>,
    args: &[String],
    after: &[String],
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let is_rev = |arg: &str| {
        if backend.resolve_object(arg).is_ok() {
            return true;
        }
        let arg = arg.strip_prefix('^').unwrap_or(arg);
        let arg = rgit_git::range_base(arg);
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
    // git's looks_like_pathspec and check_filename: globs and long magic need
    // no `--`, and `:/`, `:!`, `:^` name the path after them.
    let names_path = |p: &str| {
        let bare = [":/", ":!", ":^"]
            .iter()
            .find_map(|m| p.strip_prefix(m))
            .unwrap_or(p);
        p.contains(['*', '?', '['])
            || p.starts_with(":(")
            || bare.is_empty()
            || backend.workdir().join(bare).exists()
    };
    if let Some(p) = paths.iter().find(|p| !names_path(p)) {
        if p.contains("@{")
            && let Err(e) = backend.rev_parse(p)
            && crate::plumbing::rev_dies(&e.to_string())
        {
            return Err(CliError {
                message: e.to_string(),
                help: None,
                code: 128,
            }
            .into());
        }
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
    rgit_git::check_pathspecs(&paths)?;
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

/// Blame `[rev...] path` over the `-L` `ranges`.
pub(crate) fn blame(
    backend: &Arc<dyn GitBackend>,
    args: &[String],
    ranges: &[String],
    opts: &BlameArgs,
) -> Result<rgit_git::Blame, GitError> {
    let Some((path, revs)) = args.split_last() else {
        return Ok(rgit_git::Blame::default());
    };
    if revs.is_empty()
        && render::text_mode()
        && !backend.workdir().join(path).exists()
        && backend.read_blob("HEAD", path).is_err()
    {
        return Err(GitError::Other(format!("no such path '{path}' in HEAD")));
    }
    let contents = match opts.contents.as_deref() {
        None => None,
        Some("-") => {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
            Some(buf)
        }
        Some(file) => Some(
            std::fs::read(file)
                .map_err(|e| GitError::Other(format!("cannot open or read '{file}': {e}")))?,
        ),
    };
    backend.blame_with(&rgit_git::BlameOptions {
        contents,
        path: path.clone(),
        revs: revs.to_vec(),
        ranges: ranges.to_vec(),
        ignore_whitespace: opts.ignore_whitespace,
        moves: (opts.moves || opts.move_score.is_some()).then(|| opts.move_score.unwrap_or(0)),
        copies: opts.copies,
        copy_score: opts.copy_score.unwrap_or(0),
        first_parent: opts.first_parent,
        reverse: opts.reverse,
        show_root: opts.root,
        ignore_revs: opts.ignore_rev.clone(),
        ignore_revs_files: opts.ignore_revs_file.clone(),
    })
}

/// git's own blame format: `id (author date line) text`, with `-f`, `-n`,
/// `-t`, `-c`, `-b` and the blame.* display settings.
fn blame_git(
    backend: &Arc<dyn GitBackend>,
    blame: &rgit_git::Blame,
    path: &str,
    format: &BlameFormat,
) -> anyhow::Result<String> {
    let config = |key: &str| backend.config_get(key).ok().flatten();
    let flag = |key: &str| {
        config(key).is_some_and(|v| matches!(v.as_str(), "true" | "yes" | "on" | "1" | ""))
    };
    let style = format
        .date
        .clone()
        .or_else(|| config("blame.date"))
        .unwrap_or_else(|| "iso".to_owned());
    let base = style.strip_suffix("-local").unwrap_or(&style);
    let width = match base {
        "relative" => 22,
        "iso" | "iso8601" | "iso-strict" | "iso8601-strict" => 25,
        "rfc" | "rfc2822" => 31,
        "short" | "unix" => 10,
        "raw" | "human" => 16,
        f if f.starts_with("format:") => crate::pretty::format_date(0, 0, &style).len(),
        _ => 30,
    };
    let email = format.show_email || flag("blame.showemail");
    let blank = format.blank_boundary || flag("blame.blankboundary");
    let (mark_unblamable, mark_ignored) = (
        flag("blame.markunblamablelines"),
        flag("blame.markignoredlines"),
    );
    let lines = &blame.lines;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let mut people: std::collections::HashMap<&str, (String, i64, i32)> = Default::default();
    let mut abbrev = 7;
    for b in lines {
        if people.contains_key(b.id.as_str()) {
            continue;
        }
        let who = if b.id.is_empty() {
            let name = if email {
                format!("<{}>", b.email)
            } else {
                b.author.clone()
            };
            (name, now, crate::pretty::local_offset(now))
        } else {
            abbrev = abbrev.max(backend.abbrev_id(&b.id, 0)?.len());
            let a = crate::pretty::parse(&backend.read_object(&b.id)?).author;
            let name = if email {
                format!("<{}>", a.email)
            } else {
                a.name
            };
            (name, a.time, a.offset)
        };
        people.insert(&b.id, who);
    }
    let length = match format.abbrev {
        _ if format.long_ids => 40,
        Some(0) => 40,
        Some(n) => (n.max(4) + 1).min(40),
        None => abbrev + 1,
    };
    let show_name = format.show_name || lines.iter().any(|b| b.orig_path != path);
    let longest_file = lines.iter().map(|b| b.orig_path.len()).max().unwrap_or(0);
    let longest_author = people
        .values()
        .map(|p| p.0.chars().count())
        .max()
        .unwrap_or(0);
    let digits = |n: usize| n.max(1).to_string().len();
    let max_digits = digits(lines.iter().map(|b| b.final_line).max().unwrap_or(0));
    let orig_digits = digits(lines.iter().map(|b| b.orig_line).max().unwrap_or(0));
    let mut out = String::new();
    for b in lines {
        let (name, time, offset) = &people[b.id.as_str()];
        let hex = if b.id.is_empty() {
            "0".repeat(40)
        } else {
            b.id.clone()
        };
        let mut length = length;
        if b.boundary {
            if blank {
                out.push_str(&" ".repeat(length));
                length = 0;
            } else if !format.annotate {
                length -= 1;
                out.push('^');
            }
        }
        if mark_unblamable && b.unblamable && length > 0 {
            length -= 1;
            out.push('*');
        }
        if mark_ignored && b.ignored && length > 0 {
            length -= 1;
            out.push('?');
        }
        out.push_str(&hex[..length.min(40)]);
        let date = if format.raw_time {
            let tz = crate::pretty::format_date(*time, *offset, "raw");
            format!("{time} {}", tz.split(' ').nth(1).unwrap_or("+0000"))
        } else {
            let d = crate::pretty::format_date(*time, *offset, &style);
            let pad = width.saturating_sub(d.chars().count());
            format!("{d}{}", " ".repeat(pad))
        };
        if format.annotate {
            out.push_str(&format!("\t({name:>10}\t{date:>10}\t{})", b.final_line));
        } else {
            if show_name {
                let p: String = b.orig_path.chars().take(longest_file).collect();
                out.push_str(&format!(" {p:<longest_file$}"));
            }
            if format.show_number {
                out.push_str(&format!(" {:>orig_digits$}", b.orig_line));
            }
            if !format.no_author {
                let pad = longest_author - name.chars().count();
                out.push_str(&format!(" ({name}{} {date:>10}", " ".repeat(pad)));
            }
            out.push_str(&format!(" {:>max_digits$}) ", b.final_line));
        }
        out.push_str(&b.line);
        out.push('\n');
    }
    if format.show_stats {
        let [blobs, patches, commits] = blame.stats;
        out.push_str(&format!(
            "num read blob: {blobs}\nnum get patch: {patches}\nnum commits: {commits}\n"
        ));
    }
    Ok(out)
}

/// `describe --contains`: the commit `id` named from the oldest tag that
/// contains it, as git's name-rev does (`v1^0`, `v1~2`, `v1~1^2~3`).
fn describe_contains(
    backend: &Arc<dyn GitBackend>,
    id: &str,
    patterns: &[String],
    excludes: &[String],
) -> anyhow::Result<Option<String>> {
    struct Name {
        tip: String,
        tagger_date: i64,
        generation: usize,
        distance: usize,
    }
    let mut commits: std::collections::HashMap<String, (i64, Vec<String>)> = Default::default();
    let mut commit = |id: &str| -> anyhow::Result<(i64, Vec<String>)> {
        if let Some(c) = commits.get(id) {
            return Ok(c.clone());
        }
        let c = crate::pretty::parse(&backend.read_object(id)?);
        let entry = (c.committer.time, c.parents);
        commits.insert(id.to_owned(), entry.clone());
        Ok(entry)
    };
    // Commits a day older than the target cannot lead to it (git's cutoff).
    let cutoff = commit(id)?.0 - 86400;
    let mut tips = Vec::new();
    for r in backend.ref_details()? {
        let Some(tag) = r.name.strip_prefix("refs/tags/") else {
            continue;
        };
        let glob = |p: &String| crate::plumbing::glob(p.as_bytes(), tag.as_bytes());
        if !patterns.is_empty() && !patterns.iter().any(glob) || excludes.iter().any(glob) {
            continue;
        }
        let (target, date, deref) = match (&r.peeled, &r.tagger) {
            (Some(peeled), Some(tagger)) => (peeled.clone(), tagger.time, true),
            _ => match &r.committer {
                Some(c) => (r.id.clone(), c.time, false),
                None => continue,
            },
        };
        if backend.read_object(&target)?.kind != "commit" {
            continue;
        }
        tips.push((date, tag.to_owned(), target, deref));
    }
    tips.sort_by_key(|t| t.0);
    let mut names: std::collections::HashMap<String, Name> = Default::default();
    // A name wins when its tag is older, then when it is fewer hops away.
    let better = |n: Option<&Name>, date: i64, distance: usize| {
        n.is_none_or(|n| n.tagger_date > date || n.tagger_date == date && n.distance > distance)
    };
    for (date, tag, target, deref) in tips {
        if commit(&target)?.0 < cutoff || !better(names.get(&target), date, 0) {
            continue;
        }
        let tip = if deref { format!("{tag}^0") } else { tag };
        names.insert(
            target.clone(),
            Name {
                tip,
                tagger_date: date,
                generation: 0,
                distance: 0,
            },
        );
        let mut stack = vec![target];
        while let Some(c) = stack.pop() {
            let (tip, generation, distance) = {
                let n = &names[&c];
                (n.tip.clone(), n.generation, n.distance)
            };
            let mut queue = Vec::new();
            for (i, p) in commit(&c)?.1.into_iter().enumerate() {
                if commit(&p)?.0 < cutoff {
                    continue;
                }
                let (generation, distance, tip) = if i == 0 {
                    (generation + 1, distance + 1, tip.clone())
                } else {
                    let base = tip.strip_suffix("^0").unwrap_or(&tip);
                    let tip = if generation > 0 {
                        format!("{base}~{generation}^{}", i + 1)
                    } else {
                        format!("{base}^{}", i + 1)
                    };
                    (0, distance + 65535, tip)
                };
                if better(names.get(&p), date, distance) {
                    names.insert(
                        p.clone(),
                        Name {
                            tip,
                            tagger_date: date,
                            generation,
                            distance,
                        },
                    );
                    queue.push(p);
                }
            }
            // The first parent comes off the stack first.
            stack.extend(queue.into_iter().rev());
        }
    }
    Ok(names.get(id).map(|n| {
        if n.generation == 0 {
            n.tip.clone()
        } else {
            format!(
                "{}~{}",
                n.tip.strip_suffix("^0").unwrap_or(&n.tip),
                n.generation
            )
        }
    }))
}

/// `blame --porcelain` (or `--line-porcelain` with `repeat`), as git prints
/// it.
fn blame_porcelain(
    backend: &Arc<dyn GitBackend>,
    lines: &[rgit_git::BlameLine],
    repeat: bool,
    contents: Option<&str>,
) -> anyhow::Result<String> {
    const ZERO: &str = "0000000000000000000000000000000000000000";
    // Lines of one group: the same commit and file, both numbers running on.
    let follows = |p: &rgit_git::BlameLine, b: &rgit_git::BlameLine| {
        p.id == b.id
            && p.orig_path == b.orig_path
            && p.orig_line + 1 == b.orig_line
            && p.final_line + 1 == b.final_line
            && p.ignored == b.ignored
            && p.unblamable == b.unblamable
    };
    let mut shown = std::collections::HashSet::new();
    let mut out = String::new();
    for (i, b) in lines.iter().enumerate() {
        let id = if b.id.is_empty() { ZERO } else { &b.id };
        let starts = i == 0 || !follows(&lines[i - 1], b);
        out.push_str(&format!("{id} {} {}", b.orig_line, b.final_line));
        if starts {
            let n = 1 + lines[i..]
                .windows(2)
                .take_while(|w| follows(&w[0], &w[1]))
                .count();
            out.push_str(&format!(" {n}"));
        }
        out.push('\n');
        if repeat || starts && shown.insert(id) {
            out.push_str(&blame_details(backend, b, contents)?);
        }
        out.push('\t');
        out.push_str(&b.line);
        out.push('\n');
    }
    Ok(out)
}

/// git's `blame --incremental`: each group of lines as blame settled it,
/// with its commit's details the first time.
fn blame_incremental(
    backend: &Arc<dyn GitBackend>,
    blame: &rgit_git::Blame,
    contents: Option<&str>,
) -> anyhow::Result<String> {
    const ZERO: &str = "0000000000000000000000000000000000000000";
    let at: std::collections::HashMap<usize, &rgit_git::BlameLine> =
        blame.lines.iter().map(|b| (b.final_line, b)).collect();
    let mut shown = std::collections::HashSet::new();
    let mut out = String::new();
    for &(lno, num) in &blame.found {
        let b = at[&lno];
        let id = if b.id.is_empty() { ZERO } else { &b.id };
        out.push_str(&format!("{id} {} {lno} {num}\n", b.orig_line));
        let details = blame_details(backend, b, contents)?;
        if shown.insert(id.to_owned()) {
            out.push_str(&details);
        } else {
            let at = details
                .find("\nprevious ")
                .or_else(|| details.find("\nfilename "))
                .map_or(0, |i| i + 1);
            out.push_str(&details[at..]);
        }
    }
    Ok(out)
}

/// The `author ...` to `filename ...` lines of a porcelain blame entry.
fn blame_details(
    backend: &Arc<dyn GitBackend>,
    b: &rgit_git::BlameLine,
    contents: Option<&str>,
) -> anyhow::Result<String> {
    let path = &b.orig_path;
    if b.id.is_empty() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let tz = crate::pretty::format_date(now, crate::pretty::local_offset(now), "raw");
        let tz = tz.split(' ').nth(1).unwrap_or("+0000");
        let mut out = String::new();
        for role in ["author", "committer"] {
            out.push_str(&format!(
                "{role} {}\n{role}-mail <{}>\n{role}-time {now}\n{role}-tz {tz}\n",
                b.author, b.email
            ));
        }
        let from = contents.unwrap_or(path);
        out.push_str(&format!("summary Version of {path} from {from}\n"));
        if let Some((p, prev)) = &b.previous {
            out.push_str(&format!("previous {p} {prev}\n"));
        }
        out.push_str(&format!("filename {path}\n"));
        return Ok(out);
    }
    let c = crate::pretty::parse(&backend.read_object(&b.id)?);
    let mut out = String::new();
    for (role, who) in [("author", &c.author), ("committer", &c.committer)] {
        let tz = crate::pretty::format_date(who.time, who.offset, "raw");
        out.push_str(&format!(
            "{role} {}\n{role}-mail <{}>\n{role}-time {}\n{role}-tz {}\n",
            who.name,
            who.email,
            who.time,
            tz.split(' ').nth(1).unwrap_or("+0000")
        ));
    }
    let summary = c
        .message
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    out.push_str(&format!("summary {summary}\n"));
    if b.boundary {
        out.push_str("boundary\n");
    }
    if let Some((p, prev)) = &b.previous {
        out.push_str(&format!("previous {p} {prev}\n"));
    }
    out.push_str(&format!("filename {path}\n"));
    Ok(out)
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
    let lines = lines.into_inner().expect("report mutex");
    // A stop reports what the operation said first (CONFLICT lines), as git:
    // on stdout for humans, in the error for agents.
    let text = render::text_mode();
    if result.is_err() && text {
        for l in &lines {
            // rerere and merge-one-file's failures speak on stderr, as in git.
            let stderr = ["Recorded ", "Resolved '", "Staged '", "ERROR: ", "fatal: "];
            if stderr.iter().any(|p| l.starts_with(p)) {
                eprintln!("{l}");
            } else {
                println!("{l}");
            }
        }
    }
    let result = match result {
        Err(GitError::Conflict(why)) if !lines.is_empty() && !text => {
            Err(GitError::Conflict(format!("{}\n{why}", lines.join("\n"))))
        }
        r => r,
    };
    result?;
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
            "skills/rgit/SKILL.md is stale; run `rgit --human agent skill > skills/rgit/SKILL.md`"
        );
        assert_eq!(
            include_str!("../../../skills/rgit/references/commands.md"),
            super::skill_reference(),
            "skills/rgit/references/commands.md is stale; run \
             `rgit --human agent skill --reference > skills/rgit/references/commands.md`"
        );
    }
}
