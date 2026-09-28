//! git's diff options that `diff`, `log` and `show` share beyond the output
//! format: the algorithm, labels, rewrites, stat layout, whitespace checks
//! and moved-line colors.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use rgit_git::GitBackend;
use rgit_git::xdiff::{Algorithm, DiffTweaks};

#[derive(clap::Args, Clone, Default, Debug)]
pub struct DiffOptArgs {
    /// Spend extra time to find the smallest diff.
    #[arg(long)]
    pub minimal: bool,
    /// Diff with the patience algorithm.
    #[arg(long)]
    pub patience: bool,
    /// Diff with the histogram algorithm.
    #[arg(long)]
    pub histogram: bool,
    /// Keep lines starting with TEXT unchanged if possible (patience diff).
    #[arg(long, value_name = "TEXT")]
    pub anchored: Vec<String>,
    /// myers (default), minimal, patience or histogram.
    #[arg(long, value_name = "ALGORITHM")]
    pub diff_algorithm: Option<String>,
    /// Join hunks up to N lines apart.
    #[arg(long, value_name = "N")]
    pub inter_hunk_context: Option<u32>,
    /// Only changes under PATH (default: the current folder), named from it.
    #[arg(
        long,
        value_name = "PATH",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub relative: Option<String>,
    /// Undo --relative and diff.relative.
    #[arg(long)]
    pub no_relative: bool,
    /// The old side's prefix instead of `a/`.
    #[arg(long, value_name = "PREFIX")]
    pub src_prefix: Option<String>,
    /// The new side's prefix instead of `b/`.
    #[arg(long, value_name = "PREFIX")]
    pub dst_prefix: Option<String>,
    /// No `a/` and `b/` prefixes.
    #[arg(long)]
    pub no_prefix: bool,
    /// `a/` and `b/`, whatever diff.noprefix or diff.mnemonicPrefix say.
    #[arg(long)]
    pub default_prefix: bool,
    /// Start every output line with PREFIX.
    #[arg(long, value_name = "PREFIX")]
    pub line_prefix: Option<String>,
    /// Write the output to FILE instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Run external diff tools (GIT_EXTERNAL_DIFF, diff.external, diff.<driver>.command).
    #[arg(long, overrides_with = "no_ext_diff")]
    pub ext_diff: bool,
    /// Never run external diff tools.
    #[arg(long)]
    pub no_ext_diff: bool,
    /// Diff files through their diff.<driver>.textconv filters (the default).
    #[arg(long, overrides_with = "no_textconv")]
    pub textconv: bool,
    /// Diff files as they are stored, without textconv filters.
    #[arg(long)]
    pub no_textconv: bool,
    /// A binary patch for binary files, applicable with `git apply`.
    #[arg(long)]
    pub binary: bool,
    /// Full object names on `index` lines.
    #[arg(long)]
    pub full_index: bool,
    /// Show rewritten files as a whole removal and addition (git's -B[n][/m]).
    #[arg(
        long,
        value_name = "N[/M]",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub break_rewrites: Option<String>,
    /// Leave the removed lines out of a deleted file's patch.
    #[arg(short = 'D', long)]
    pub irreversible_delete: bool,
    /// Give up rename detection past N files (git's -l<n>).
    #[arg(long, value_name = "N")]
    pub rename_limit: Option<usize>,
    /// How submodule changes show: short, log or diff.
    #[arg(
        long,
        value_name = "FORMAT",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "log",
        value_parser = ["short", "log", "diff"]
    )]
    pub submodule: Option<String>,
    /// Share of changes per folder: changes, lines, files, cumulative, <limit%>.
    #[arg(
        long,
        short = 'X',
        value_name = "PARAM,...",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub dirstat: Option<String>,
    /// --dirstat=files.
    #[arg(
        long,
        value_name = "PARAM,...",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub dirstat_by_file: Option<String>,
    /// --dirstat=cumulative.
    #[arg(long)]
    pub cumulative: bool,
    /// A diffstat that also marks new, gone and mode-changed files.
    #[arg(long)]
    pub compact_summary: bool,
    /// The diffstat's total width.
    #[arg(long, value_name = "N")]
    pub stat_width: Option<usize>,
    /// The diffstat's file name width.
    #[arg(long, value_name = "N")]
    pub stat_name_width: Option<usize>,
    /// List only the first N files in the diffstat.
    #[arg(long, value_name = "N")]
    pub stat_count: Option<usize>,
    /// The diffstat's graph width.
    #[arg(long, value_name = "N")]
    pub stat_graph_width: Option<usize>,
    /// Report whitespace errors and conflict markers instead of a patch.
    #[arg(long)]
    pub check: bool,
    /// Color moved lines: no, default, plain, blocks, zebra or dimmed-zebra.
    #[arg(
        long,
        value_name = "MODE",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "default"
    )]
    pub color_moved: Option<String>,
    /// Turn --color-moved off.
    #[arg(long)]
    pub no_color_moved: bool,
    /// Whitespace moved lines may differ in: ignore-space-at-eol,
    /// ignore-space-change, ignore-all-space, allow-indentation-change, no.
    #[arg(long, value_name = "MODES")]
    pub color_moved_ws: Option<String>,
    /// Turn --color-moved-ws off.
    #[arg(long)]
    pub no_color_moved_ws: bool,
    /// Which lines show whitespace errors: context, old, new, all, none, default.
    #[arg(long, value_name = "KIND")]
    pub ws_error_highlight: Option<String>,
}

/// Where the diff runs, for diff.mnemonicPrefix's labels.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sides {
    IndexWorktree,
    CommitWorktree,
    CommitIndex,
    Commits,
}

/// Whitespace problems git's core.whitespace knows.
pub const WS_BLANK_AT_EOL: u32 = 1 << 6;
pub const WS_SPACE_BEFORE_TAB: u32 = 1 << 7;
pub const WS_INDENT_WITH_NON_TAB: u32 = 1 << 8;
pub const WS_CR_AT_EOL: u32 = 1 << 9;
pub const WS_BLANK_AT_EOF: u32 = 1 << 10;
pub const WS_TAB_IN_INDENT: u32 = 1 << 11;
pub const WS_TAB_WIDTH_MASK: u32 = 0o77;

/// What the renderer needs from the options, for the running command.
#[derive(Clone, Default, Debug)]
pub struct Render {
    pub line_prefix: String,
    pub output: Option<PathBuf>,
    pub stat_width: Option<usize>,
    pub stat_name_width: Option<usize>,
    pub stat_count: Option<usize>,
    pub stat_graph_width: Option<usize>,
    pub compact_summary: bool,
    /// Dirstat: (by: 0 changes, 1 lines, 2 files; permille; cumulative).
    pub dirstat: Option<(u8, usize, bool)>,
    pub check: bool,
    /// Moved-line mode (0 off) and its whitespace flags.
    pub color_moved: u8,
    pub color_moved_ws: u32,
    /// Bits of WSEH_NEW (1), WSEH_CONTEXT (2), WSEH_OLD (4).
    pub ws_highlight: u8,
    /// core.whitespace's rule.
    pub ws_rule: u32,
    /// An external diff command to run per file.
    pub external: Option<String>,
    pub allow_external: bool,
    pub submodule: Option<String>,
    pub src_prefix: String,
    pub dst_prefix: String,
    /// The new side is the work tree, unhashed.
    pub worktree: bool,
    /// The old side is the index, as diff-files has it.
    pub index_worktree: bool,
}

thread_local! {
    static RENDER: RefCell<Render> = RefCell::new(Render::default());
}

pub fn current() -> Render {
    RENDER.with(|r| r.borrow().clone())
}

pub fn set(r: Render) {
    RENDER.with(|c| *c.borrow_mut() = r);
}

/// Whether the running command asks for an output git's -p would not
/// imply: a dirstat, `--check` or `--compact-summary`.
pub fn formats() -> bool {
    RENDER.with(|r| {
        let r = r.borrow();
        r.dirstat.is_some() || r.check || r.compact_summary
    })
}

/// git's parse_whitespace_rule over core.whitespace.
pub fn whitespace_rule(value: Option<&str>) -> u32 {
    let mut rule = WS_BLANK_AT_EOL | WS_SPACE_BEFORE_TAB | WS_BLANK_AT_EOF | 8;
    let Some(value) = value else {
        return rule;
    };
    for item in value
        .split([',', ' ', '\t', '\n'])
        .filter(|s| !s.is_empty())
    {
        let (neg, name) = match item.strip_prefix('-') {
            Some(n) => (true, n),
            None => (false, item),
        };
        let bits = match name {
            "trailing-space" => WS_BLANK_AT_EOL | WS_BLANK_AT_EOF,
            "blank-at-eol" => WS_BLANK_AT_EOL,
            "space-before-tab" => WS_SPACE_BEFORE_TAB,
            "indent-with-non-tab" => WS_INDENT_WITH_NON_TAB,
            "cr-at-eol" => WS_CR_AT_EOL,
            "blank-at-eof" => WS_BLANK_AT_EOF,
            "tab-in-indent" => WS_TAB_IN_INDENT,
            _ => {
                if let Some(n) = name.strip_prefix("tabwidth=")
                    && let Ok(n) = n.parse::<u32>()
                    && (1..0o100).contains(&n)
                {
                    rule = (rule & !WS_TAB_WIDTH_MASK) | n;
                }
                continue;
            }
        };
        if neg {
            rule &= !bits;
        } else {
            rule |= bits;
        }
    }
    rule
}

/// git's parse_ws_error_highlight.
fn ws_highlight(value: &str) -> Option<u8> {
    let mut out = 0;
    for item in value.split(',') {
        out = match item {
            "none" => 0,
            "default" => 1,
            "all" => 7,
            "new" => out | 1,
            "context" => out | 2,
            "old" => out | 4,
            _ => return None,
        };
    }
    Some(out)
}

pub const MOVED_PLAIN: u8 = 1;
pub const MOVED_BLOCKS: u8 = 2;
pub const MOVED_ZEBRA: u8 = 3;
pub const MOVED_ZEBRA_DIM: u8 = 4;

fn moved_mode(value: &str) -> Option<u8> {
    Some(match value {
        "no" | "false" | "off" => 0,
        "plain" => MOVED_PLAIN,
        "blocks" => MOVED_BLOCKS,
        "zebra" | "default" | "true" | "yes" | "on" => MOVED_ZEBRA,
        "dimmed-zebra" | "dimmed_zebra" => MOVED_ZEBRA_DIM,
        _ => return None,
    })
}

pub const MOVED_WS_EOL: u32 = 1 << 3;
pub const MOVED_WS_CHANGE: u32 = 1 << 2;
pub const MOVED_WS_ALL: u32 = 1 << 1;
pub const MOVED_WS_INDENT: u32 = 1 << 5;

fn moved_ws(value: &str) -> Result<u32, String> {
    let mut out = 0;
    for item in value.split(',').map(str::trim) {
        match item {
            "no" => out = 0,
            "ignore-space-at-eol" => out |= MOVED_WS_EOL,
            "ignore-space-change" => out |= MOVED_WS_CHANGE,
            "ignore-all-space" => out |= MOVED_WS_ALL,
            "allow-indentation-change" => out |= MOVED_WS_INDENT,
            _ => {
                return Err(format!(
                    "unknown color-moved-ws mode '{item}', possible values are 'ignore-space-change', 'ignore-space-at-eol', 'ignore-all-space', 'allow-indentation-change'"
                ));
            }
        }
    }
    if out & MOVED_WS_INDENT != 0 && out & (MOVED_WS_EOL | MOVED_WS_CHANGE | MOVED_WS_ALL) != 0 {
        return Err(
            "color-moved-ws: allow-indentation-change cannot be combined with other whitespace modes"
                .to_owned(),
        );
    }
    Ok(out)
}

/// git's parse_dirstat_params, over diff.dirstat and then the option.
fn dirstat_params(mut at: (u8, usize, bool), value: &str) -> Result<(u8, usize, bool), String> {
    for p in value.split(',').filter(|p| !p.is_empty()) {
        match p {
            "changes" => at.0 = 0,
            "lines" => at.0 = 1,
            "files" => at.0 = 2,
            "noncumulative" => at.2 = false,
            "cumulative" => at.2 = true,
            _ => {
                let (int, frac) = p.split_once('.').unwrap_or((p, ""));
                let ok = !int.is_empty()
                    && int.bytes().all(|b| b.is_ascii_digit())
                    && frac.bytes().all(|b| b.is_ascii_digit())
                    && !(p.contains('.') && frac.is_empty());
                if !ok {
                    return Err(format!(
                        "  Failed to parse dirstat cut-off percentage '{p}'\n"
                    ));
                }
                at.1 = int.parse::<usize>().unwrap_or(0) * 10
                    + frac.bytes().next().map_or(0, |b| (b - b'0') as usize);
            }
        }
    }
    Ok(at)
}

fn usage(msg: String) -> anyhow::Error {
    crate::cli::CliError {
        message: msg,
        help: None,
        code: 128,
    }
    .into()
}

impl DiffOptArgs {
    /// Set the diff settings for this command: the backend's and the
    /// renderer's. `porcelain` is `git diff` (external tools on by default).
    pub fn apply(
        &self,
        backend: &Arc<dyn GitBackend>,
        sides: Sides,
        porcelain: bool,
        patch_only: bool,
    ) -> anyhow::Result<()> {
        let cfg = |k: &str| backend.config_get(k).ok().flatten();
        let truthy = |k: &str| {
            cfg(k).is_some_and(|v| {
                matches!(
                    v.to_ascii_lowercase().as_str(),
                    "true" | "yes" | "on" | "1" | ""
                )
            })
        };
        let mut algorithm = match cfg("diff.algorithm") {
            Some(a) => Algorithm::parse(&a).unwrap_or_default(),
            None => Algorithm::Myers,
        };
        if let Some(a) = &self.diff_algorithm {
            algorithm = Algorithm::parse(a).ok_or_else(|| {
                usage("option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"".to_owned())
            })?;
        }
        for (on, a) in [
            (self.minimal, Algorithm::Minimal),
            (self.patience, Algorithm::Patience),
            (self.histogram, Algorithm::Histogram),
        ] {
            if on {
                algorithm = a;
            }
        }
        let (mut src, mut dst) = ("a/".to_owned(), "b/".to_owned());
        if !self.default_prefix {
            if truthy("diff.noprefix") {
                (src, dst) = (String::new(), String::new());
            } else if truthy("diff.mnemonicPrefix") {
                let (a, b) = match sides {
                    Sides::IndexWorktree => ("i/", "w/"),
                    Sides::CommitWorktree => ("c/", "w/"),
                    Sides::CommitIndex => ("c/", "i/"),
                    Sides::Commits => ("a/", "b/"),
                };
                (src, dst) = (a.to_owned(), b.to_owned());
            }
        }
        if self.no_prefix {
            (src, dst) = (String::new(), String::new());
        }
        if let Some(p) = &self.src_prefix {
            src = p.clone();
        }
        if let Some(p) = &self.dst_prefix {
            dst = p.clone();
        }
        let relative = match &self.relative {
            _ if self.no_relative => None,
            Some(p) if !p.is_empty() => Some(p.clone()),
            Some(_) => Some(cwd_prefix(backend)),
            None if truthy("diff.relative") => Some(cwd_prefix(backend)),
            None => None,
        }
        .map(|p| {
            let p = p.trim_end_matches('/');
            if p.is_empty() {
                String::new()
            } else {
                format!("{p}/")
            }
        })
        .filter(|p| !p.is_empty());
        let break_rewrites = match &self.break_rewrites {
            Some(b) => Some(
                rgit_git::xdiff::parse_break(b)
                    .ok_or_else(|| usage(format!("invalid argument to -B: {b}")))?,
            ),
            None => None,
        };
        // git's diffstat counts the stored text; only a patch converts it.
        let textconv = !self.no_textconv && patch_only;
        let mut anchors = self.anchored.clone();
        let chosen =
            self.diff_algorithm.is_some() || self.minimal || self.patience || self.histogram;
        if !anchors.is_empty() && !chosen {
            algorithm = Algorithm::Patience;
        }
        if algorithm != Algorithm::Patience {
            anchors.clear();
        }
        rgit_git::xdiff::set_tweaks(DiffTweaks {
            algorithm,
            anchors,
            inter_hunk: self
                .inter_hunk_context
                .or_else(|| cfg("diff.interHunkContext").and_then(|v| v.parse().ok())),
            src_prefix: Some(src.clone()),
            dst_prefix: Some(dst.clone()),
            full_index: self.full_index,
            binary: self.binary,
            break_rewrites,
            irreversible_delete: self.irreversible_delete,
            rename_limit: self.rename_limit,
            relative,
            textconv,
        });
        let mut dirstat = None;
        let by_file = self.dirstat_by_file.is_some();
        if self.dirstat.is_some() || by_file || self.cumulative {
            let mut at = (0, 30, false);
            if let Some(c) = cfg("diff.dirstat") {
                at = dirstat_params(at, &c).unwrap_or(at);
            }
            if by_file {
                at.0 = 2;
            }
            for v in [&self.dirstat, &self.dirstat_by_file].into_iter().flatten() {
                at = dirstat_params(at, v).map_err(|e| {
                    usage(format!(
                        "Failed to parse --dirstat/-X option parameter:\n{e}"
                    ))
                })?;
            }
            if self.cumulative {
                at.2 = true;
            }
            dirstat = Some(at);
        }
        let color_moved = if self.no_color_moved {
            0
        } else if let Some(m) = &self.color_moved {
            moved_mode(m).ok_or_else(|| usage(format!("bad --color-moved argument: {m}")))?
        } else {
            cfg("diff.colorMoved")
                .and_then(|m| moved_mode(&m))
                .unwrap_or(0)
        };
        let color_moved_ws = if self.no_color_moved_ws {
            0
        } else if let Some(w) = &self.color_moved_ws {
            moved_ws(w).map_err(usage)?
        } else {
            cfg("diff.colorMovedWS")
                .and_then(|w| moved_ws(&w).ok())
                .unwrap_or(0)
        };
        let ws_highlight = match self
            .ws_error_highlight
            .clone()
            .or_else(|| cfg("diff.wsErrorHighlight"))
        {
            Some(v) => ws_highlight(&v)
                .ok_or_else(|| usage(format!("unknown value after ws-error-highlight={v}")))?,
            None => 1,
        };
        let external = (porcelain && !self.no_ext_diff || self.ext_diff)
            .then(|| {
                std::env::var("GIT_EXTERNAL_DIFF")
                    .ok()
                    .or_else(|| cfg("diff.external"))
            })
            .flatten();
        let ws_rule = whitespace_rule(cfg("core.whitespace").as_deref());
        if ws_rule & WS_TAB_IN_INDENT != 0 && ws_rule & WS_INDENT_WITH_NON_TAB != 0 {
            return Err(usage(
                "cannot enforce both tab-in-indent and indent-with-non-tab".to_owned(),
            ));
        }
        BACKEND.with(|b| *b.borrow_mut() = Some(backend.clone()));
        set(Render {
            line_prefix: self.line_prefix.clone().unwrap_or_default(),
            output: self.output.clone(),
            stat_width: self.stat_width,
            stat_name_width: self
                .stat_name_width
                .or_else(|| cfg("diff.statNameWidth").and_then(|v| v.parse().ok())),
            stat_count: self.stat_count,
            stat_graph_width: self
                .stat_graph_width
                .or_else(|| cfg("diff.statGraphWidth").and_then(|v| v.parse().ok())),
            compact_summary: self.compact_summary,
            dirstat,
            check: self.check,
            color_moved,
            color_moved_ws,
            ws_highlight,
            ws_rule,
            external: external.clone(),
            allow_external: ((porcelain && !self.no_ext_diff) || self.ext_diff)
                && (external.is_some()
                    || backend
                        .config_entries(rgit_git::ConfigScope::Any, None)
                        .unwrap_or_default()
                        .iter()
                        .any(|(k, _)| {
                            let k = k.to_ascii_lowercase();
                            k.starts_with("diff.") && k.ends_with(".command")
                        })),
            submodule: self.submodule.clone().or_else(|| cfg("diff.submodule")),
            src_prefix: src,
            dst_prefix: dst,
            worktree: matches!(sides, Sides::IndexWorktree | Sides::CommitWorktree),
            index_worktree: sides == Sides::IndexWorktree,
        });
        Ok(())
    }
}

/// The current folder from the top of the work tree, with a trailing `/`.
fn cwd_prefix(backend: &Arc<dyn GitBackend>) -> String {
    let top = backend.workdir().canonicalize().ok();
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok());
    match (top, cwd) {
        (Some(top), Some(cwd)) => cwd
            .strip_prefix(&top)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Reset the diff settings, for a command that takes none.
pub fn clear() {
    BACKEND.with(|b| *b.borrow_mut() = None);
    rgit_git::xdiff::set_tweaks(DiffTweaks::default());
    set(Render::default());
}

/// Put `prefix` before every line of `text`, as git's --line-prefix does.
pub fn prefix_lines(text: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(prefix);
        out.push_str(line);
    }
    out
}

thread_local! {
    static BACKEND: RefCell<Option<Arc<dyn GitBackend>>> = const { RefCell::new(None) };
}

pub(crate) fn set_backend(b: Option<Arc<dyn GitBackend>>) {
    BACKEND.with(|c| *c.borrow_mut() = b);
}

pub(crate) fn backend() -> Option<Arc<dyn GitBackend>> {
    BACKEND.with(|b| b.borrow().clone())
}
