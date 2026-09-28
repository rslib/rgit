//! `git status` in git's own formats (long, short, porcelain v1 and v2),
//! collected and printed the way wt-status.c does.

use crate::rev::RevParse;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use git2::{Delta, Diff, DiffFindOptions, DiffFormat, DiffOptions, Index, Oid, Repository};

use crate::GitError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFormat {
    Long,
    Short,
    Porcelain,
    PorcelainV2,
}

/// What `git status` (or `git commit`'s template and `--dry-run`) was asked
/// for. `None` fields fall back to config, then to git's defaults.
#[derive(Debug, Clone, Default)]
pub struct StatusOpts {
    pub format: Option<StatusFormat>,
    pub branch: Option<bool>,
    /// `-z`: NUL-terminated entries.
    pub null: bool,
    pub show_stash: Option<bool>,
    pub ahead_behind: Option<bool>,
    /// `-u` mode: `no`, `normal` or `all`.
    pub untracked: Option<String>,
    /// `--ignored` mode: `traditional`, `matching` or `no`.
    pub ignored: Option<String>,
    /// `--ignore-submodules` mode: `none`, `untracked`, `dirty` or `all`.
    pub ignore_submodules: Option<String>,
    /// `--column[=<opts>]`; `--no-column` is `never`.
    pub column: Option<String>,
    /// `--renames` / `--no-renames`.
    pub renames: Option<bool>,
    /// `-M<n>`: find renames at this similarity.
    pub find_renames: Option<String>,
    pub verbose: u8,
    /// Pathspecs from the top of the worktree.
    pub paths: Vec<String>,
    /// The current folder from the top (`sub/dir/`), for relative paths.
    pub prefix: String,
    /// Color forced on or off; `None` follows color.status and the terminal.
    pub color: Option<bool>,
    /// stdout is a terminal (for `auto` color and columns).
    pub tty: bool,
    /// Terminal width for `--column` (git's term_columns; 0 is 80).
    pub width: usize,
    /// Compare with HEAD^ (commit --amend).
    pub amend: bool,
    /// Status for `git commit` (its `--dry-run` or template): "Initial
    /// commit", no divergence advice.
    pub commit: bool,
    /// Print as commit's message template: `#` prefixed, no hints, a cut
    /// line before the `-v` diff.
    pub template: bool,
    /// Leave out the closing "nothing to commit" advice.
    pub nowarn: bool,
    /// Show the index `commit -a` would record.
    pub commit_all: bool,
    /// Paths `commit` records: on top of the index with `commit_include`
    /// (git's -i), else on top of HEAD (git's --only).
    pub commit_paths: Vec<String>,
    pub commit_include: bool,
}

/// The rendered status and whether a commit would record anything.
pub struct StatusReport {
    pub text: Vec<u8>,
    pub committable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Untracked {
    No,
    Normal,
    All,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ignored {
    No,
    Traditional,
    Matching,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AheadBehind {
    Full,
    Quick,
}

const DIRTY_MODIFIED: u8 = 1;
const DIRTY_UNTRACKED: u8 = 2;

const RESET: &str = "\x1b[m";

/// Color slots, in wt-status.h order.
#[derive(Clone, Copy)]
enum Slot {
    Header = 0,
    Updated,
    Changed,
    Untracked,
    NoBranch,
    Unmerged,
    LocalBranch,
    RemoteBranch,
    OnBranch,
}

struct Change {
    index_status: u8,
    worktree_status: u8,
    stagemask: u8,
    rename_source: Option<String>,
    rename_status: u8,
    rename_score: u16,
    mode_head: u32,
    mode_index: u32,
    mode_worktree: u32,
    oid_head: Oid,
    oid_index: Oid,
    dirty_submodule: u8,
    new_submodule_commits: bool,
}

impl Change {
    fn new() -> Self {
        Self {
            index_status: 0,
            worktree_status: 0,
            stagemask: 0,
            rename_source: None,
            rename_status: 0,
            rename_score: 0,
            mode_head: 0,
            mode_index: 0,
            mode_worktree: 0,
            oid_head: Oid::ZERO_SHA1,
            oid_index: Oid::ZERO_SHA1,
            dirty_submodule: 0,
            new_submodule_commits: false,
        }
    }
}

#[derive(Default)]
struct State {
    merge: bool,
    am: bool,
    am_empty_patch: bool,
    rebase: bool,
    rebase_interactive: bool,
    cherry_pick: bool,
    cherry_pick_head: Option<Oid>,
    revert: bool,
    revert_head: Option<Oid>,
    bisect: bool,
    branch: Option<String>,
    onto: Option<String>,
    bisecting_from: Option<String>,
    detached_from: Option<String>,
    detached_at: bool,
    sparse: Option<u32>,
}

/// One tracking comparison: the upstream's short name and how the branch
/// stands against it.
struct Tracking {
    base: String,
    gone: bool,
    /// ahead, behind; `None` when only "different" is known (quick mode).
    counts: Option<(usize, usize)>,
    same: bool,
}

struct Wt<'r> {
    repo: &'r Repository,
    index: &'r Index,
    cfg: git2::Config,
    opts: &'r StatusOpts,
    format: StatusFormat,
    show_branch: bool,
    show_stash: bool,
    ahead_behind: AheadBehind,
    untracked_mode: Untracked,
    ignored_mode: Ignored,
    detect_rename: u8, // 0 off, 1 renames, 2 copies
    rename_score: Option<u16>,
    rename_limit: Option<usize>,
    hints: bool,
    submodule_summary: i64,
    use_color: bool,
    palette: [String; 9],
    prefix: Option<String>,
    comment: bool,
    from_commit: bool,
    reference: String,
    branch: Option<String>,
    is_initial: bool,
    oid_commit: Oid,
    change: BTreeMap<String, Change>,
    untracked: BTreeSet<String>,
    ignored: BTreeSet<String>,
    state: State,
    committable: bool,
    workdir_dirty: bool,
    /// git's column option bits (text::column_mode).
    colopts: u32,
    spec: Option<crate::pathspec::Pathspec>,
    out: Vec<u8>,
}

/// Collect and print the status of `repo` (against `index`, which may be a
/// temporary one for `commit --dry-run`), as git would.
pub fn status(
    repo: &Repository,
    index: &Index,
    opts: &StatusOpts,
) -> Result<StatusReport, GitError> {
    let cfg = crate::config::open_config(repo, crate::ConfigScope::Any, false)?;
    let mut wt = Wt::new(repo, index, cfg, opts)?;
    wt.collect()?;
    wt.print()?;
    Ok(StatusReport {
        text: wt.out,
        committable: wt.committable,
    })
}

fn bool_value(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

impl<'r> Wt<'r> {
    fn new(
        repo: &'r Repository,
        index: &'r Index,
        cfg: git2::Config,
        opts: &'r StatusOpts,
    ) -> Result<Self, GitError> {
        let get = |k: &str| cfg.get_string(k).ok();
        let get_bool = |k: &str| cfg.get_bool(k).ok();
        let porcelain = matches!(
            opts.format,
            Some(StatusFormat::Porcelain | StatusFormat::PorcelainV2)
        );
        let deferred = !porcelain && !opts.null;
        let mut format = opts.format;
        if opts.null {
            match format {
                None => format = Some(StatusFormat::Porcelain),
                Some(StatusFormat::Long) => {
                    return Err(GitError::Other(
                        "options '--long' and '-z' cannot be used together".to_owned(),
                    ));
                }
                _ => {}
            }
        }
        if deferred && format.is_none() && !opts.commit {
            format = get_bool("status.short").map(|short| {
                if short {
                    StatusFormat::Short
                } else {
                    StatusFormat::Long
                }
            });
        }
        let format = format.unwrap_or(StatusFormat::Long);
        let show_branch = opts
            .branch
            .or_else(|| deferred.then(|| get_bool("status.branch")).flatten())
            .unwrap_or(false);
        let ahead_behind = match opts
            .ahead_behind
            .or_else(|| deferred.then(|| get_bool("status.aheadBehind")).flatten())
        {
            Some(false) => AheadBehind::Quick,
            _ => AheadBehind::Full,
        };
        let untracked_arg = opts
            .untracked
            .clone()
            .or_else(|| get("status.showUntrackedFiles"));
        let untracked_mode = match untracked_arg.as_deref() {
            None | Some("normal") => Untracked::Normal,
            Some("all") => Untracked::All,
            Some("no") => Untracked::No,
            Some(v) => match bool_value(v) {
                Some(true) => Untracked::Normal,
                Some(false) => Untracked::No,
                None => {
                    return Err(GitError::Other(format!(
                        "Invalid untracked files mode '{v}'"
                    )));
                }
            },
        };
        let ignored_mode = match opts.ignored.as_deref() {
            None | Some("no") => Ignored::No,
            Some("traditional") => Ignored::Traditional,
            Some("matching") => Ignored::Matching,
            Some(v) => return Err(GitError::Other(format!("Invalid ignored mode '{v}'"))),
        };
        if ignored_mode == Ignored::Matching && untracked_mode == Untracked::No {
            return Err(GitError::Other(
                "Unsupported combination of ignored and untracked-files arguments".to_owned(),
            ));
        }
        let rename_cfg = |v: String| match v.to_ascii_lowercase().as_str() {
            "copies" | "copy" => 2,
            other => u8::from(bool_value(other).unwrap_or(true)),
        };
        let mut detect_rename = get("status.renames")
            .or_else(|| get("diff.renames"))
            .map_or(1, rename_cfg);
        if let Some(r) = opts.renames {
            detect_rename = u8::from(r);
        }
        let mut rename_score = None;
        if let Some(n) = &opts.find_renames {
            detect_rename = detect_rename.max(1);
            if !n.is_empty() {
                rename_score = Some(parse_rename_score(n));
            }
        }
        let rename_limit = cfg
            .get_i64("status.renameLimit")
            .or_else(|_| cfg.get_i64("diff.renameLimit"))
            .ok()
            .map(|n| n.max(0) as usize);
        let use_color = match opts.color {
            Some(c) => c,
            None if porcelain || opts.template => false,
            None => {
                let v = get("color.status")
                    .or_else(|| get("status.color"))
                    .or_else(|| get("color.ui"));
                let auto = opts.tty && std::env::var("TERM").map_or(true, |t| t != "dumb");
                match v.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    Some("never") => false,
                    Some("always") => true,
                    None | Some("auto") => auto,
                    Some(other) => bool_value(other).unwrap_or(false) && auto,
                }
            }
        };
        let mut palette: [String; 9] = [
            String::new(),
            "\x1b[32m".to_owned(),
            "\x1b[31m".to_owned(),
            "\x1b[31m".to_owned(),
            "\x1b[31m".to_owned(),
            "\x1b[31m".to_owned(),
            "\x1b[32m".to_owned(),
            "\x1b[31m".to_owned(),
            String::new(),
        ];
        let mut onbranch_set = false;
        if let Ok(entries) = cfg.entries(None) {
            let mut entries = entries;
            while let Some(Ok(e)) = entries.next() {
                let (Ok(name), Ok(value)) = (e.name(), e.value()) else {
                    continue;
                };
                let lower = name.to_ascii_lowercase();
                let Some(slot) = lower
                    .strip_prefix("color.status.")
                    .or_else(|| lower.strip_prefix("status.color."))
                else {
                    continue;
                };
                let idx = match slot {
                    "header" => Slot::Header,
                    "branch" => Slot::OnBranch,
                    "updated" | "added" => Slot::Updated,
                    "changed" => Slot::Changed,
                    "untracked" => Slot::Untracked,
                    "nobranch" => Slot::NoBranch,
                    "unmerged" => Slot::Unmerged,
                    "localbranch" => Slot::LocalBranch,
                    "remotebranch" => Slot::RemoteBranch,
                    _ => continue,
                } as usize;
                if let Some(c) = crate::ansi_color(value) {
                    palette[idx] = c;
                    onbranch_set |= idx == Slot::OnBranch as usize;
                }
            }
        }
        if !onbranch_set {
            palette[Slot::OnBranch as usize] = palette[Slot::Header as usize].clone();
        }
        let relative = get_bool("status.relativePaths").unwrap_or(true);
        let prefix = (relative && format != StatusFormat::Porcelain)
            .then(|| opts.prefix.clone())
            .filter(|p| !p.is_empty());
        let comment = opts.template || get_bool("status.displayCommentPrefix").unwrap_or(false);
        let mut colopts = 0;
        for key in ["column.ui", "column.status"] {
            if let Some(v) = get(key) {
                column_config(&mut colopts, &v);
            }
        }
        if let Some(v) = &opts.column {
            let _ = crate::text::column_mode(&mut colopts, "always");
            column_config(&mut colopts, v);
        }
        crate::text::column_finalize(&mut colopts, opts.tty);
        let from_commit = !repo.path().join("MERGE_HEAD").exists()
            && repo.refname_to_id("CHERRY_PICK_HEAD").is_err();
        let reference = if opts.amend { "HEAD^1" } else { "HEAD" }.to_owned();
        let head_commit = repo
            .rev_single(&reference)
            .and_then(|o| o.peel_to_commit())
            .ok();
        let branch = match repo.find_reference("HEAD") {
            Ok(h) => match h.symbolic_target().ok().flatten() {
                Some(t) => Some(t.to_owned()),
                None => Some("HEAD".to_owned()),
            },
            Err(_) => None,
        };
        let spec = (!opts.paths.is_empty())
            .then(|| crate::pathspec::Pathspec::new(opts.paths.iter()))
            .transpose()?;
        Ok(Self {
            repo,
            index,
            opts,
            format,
            show_branch,
            show_stash: opts
                .show_stash
                .or_else(|| get_bool("status.showStash"))
                .unwrap_or(false),
            ahead_behind,
            untracked_mode,
            ignored_mode,
            detect_rename,
            rename_score,
            rename_limit,
            hints: !opts.template && get_bool("advice.statusHints").unwrap_or(true),
            // A number limits the commits shown; true (-1) shows them all.
            submodule_summary: match get("status.submoduleSummary") {
                Some(v) => match v.trim().parse::<i64>() {
                    Ok(n) => n,
                    Err(_) => -i64::from(bool_value(&v).unwrap_or(false)),
                },
                None => 0,
            },
            use_color,
            palette,
            prefix,
            comment,
            from_commit,
            reference,
            branch,
            is_initial: head_commit.is_none(),
            oid_commit: head_commit.map_or(Oid::ZERO_SHA1, |c| c.id()),
            change: BTreeMap::new(),
            untracked: BTreeSet::new(),
            ignored: BTreeSet::new(),
            state: State::default(),
            committable: false,
            workdir_dirty: false,
            colopts,
            spec,
            cfg,
            out: Vec::new(),
        })
    }

    fn in_spec(&self, path: &str) -> bool {
        self.spec
            .as_ref()
            .is_none_or(|s| s.matches_path(Path::new(path)))
    }

    fn entry(&mut self, path: &str) -> &mut Change {
        self.change
            .entry(path.to_owned())
            .or_insert_with(Change::new)
    }

    // ----- collection -----

    fn collect(&mut self) -> Result<(), GitError> {
        let mut ita = HashSet::new();
        let mut unmerged: BTreeMap<String, u8> = BTreeMap::new();
        let mut gitlinks = Vec::new();
        for e in self.index.iter() {
            let path = String::from_utf8_lossy(&e.path).into_owned();
            let stage = (e.flags >> 12) & 3;
            if stage > 0 {
                *unmerged.entry(path).or_default() |= 1 << (stage - 1);
            } else if e.flags_extended & (1 << 13) != 0 {
                ita.insert(path);
            } else if e.mode == 0o160000 {
                gitlinks.push((path, e.id));
            }
        }
        self.collect_worktree(&ita, &unmerged, &gitlinks)?;
        if self.is_initial {
            self.collect_initial(&unmerged);
        } else {
            self.collect_index(&ita, &unmerged)?;
        }
        self.collect_untracked()?;
        self.get_state();
        if self.state.merge && !self.change.values().any(|d| d.stagemask != 0) {
            self.committable = true;
        }
        Ok(())
    }

    fn worktree_mode(&self, path: &str) -> u32 {
        let Some(wd) = self.repo.workdir() else {
            return 0;
        };
        match std::fs::symlink_metadata(wd.join(path)) {
            Ok(m) if m.file_type().is_symlink() => 0o120000,
            Ok(m) if m.is_dir() => 0o160000,
            Ok(m) => {
                use std::os::unix::fs::PermissionsExt;
                if m.permissions().mode() & 0o100 != 0 {
                    0o100755
                } else {
                    0o100644
                }
            }
            Err(_) => 0,
        }
    }

    fn collect_worktree(
        &mut self,
        ita: &HashSet<String>,
        unmerged: &BTreeMap<String, u8>,
        gitlinks: &[(String, Oid)],
    ) -> Result<(), GitError> {
        let mut o = DiffOptions::new();
        o.include_typechange(true).ignore_submodules(true);
        let diff = self
            .repo
            .diff_index_to_workdir(Some(self.index), Some(&mut o))?;
        let skips = crate::sparse::skipped_paths(self.index);
        for delta in diff.deltas() {
            let path = delta_path(&delta);
            if ita.contains(&path) || unmerged.contains_key(&path) || !self.in_spec(&path) {
                continue;
            }
            if delta.status() == git2::Delta::Deleted && skips.contains(&path) {
                continue;
            }
            if delta.old_file().mode() == git2::FileMode::Commit
                || delta.new_file().mode() == git2::FileMode::Commit
            {
                continue;
            }
            let Some(status) = delta_char(delta.status()) else {
                continue;
            };
            self.workdir_dirty = true;
            let (one, two) = (delta.old_file(), delta.new_file());
            let d = self.entry(&path);
            if d.worktree_status == 0 {
                d.worktree_status = status;
            }
            match status {
                b'A' => d.mode_worktree = two.mode().into(),
                b'D' => {
                    d.mode_index = one.mode().into();
                    d.oid_index = one.id();
                }
                _ => {
                    d.mode_index = one.mode().into();
                    d.mode_worktree = two.mode().into();
                    d.oid_index = one.id();
                }
            }
        }
        let workdir = self.repo.workdir().map(Path::to_path_buf);
        for path in ita {
            if !self.in_spec(path) {
                continue;
            }
            let mode = self.worktree_mode(path);
            if mode == 0 {
                continue;
            }
            self.workdir_dirty = true;
            let d = self.entry(path);
            d.worktree_status = b'A';
            d.mode_worktree = mode;
        }
        self.pair_ita_renames(ita)?;
        for path in unmerged.keys() {
            if !self.in_spec(path) {
                continue;
            }
            self.workdir_dirty = true;
            let mode = self.worktree_mode(path);
            let d = self.entry(path);
            d.worktree_status = b'U';
            d.mode_worktree = mode;
        }
        for (path, oid) in gitlinks {
            let ignore = self.sub_ignore(path);
            if ignore == "all" || !self.in_spec(path) {
                continue;
            }
            let Some(dir) = workdir.as_ref().map(|w| w.join(path)) else {
                continue;
            };
            let (new_commits, dirty) = if !dir.exists() {
                self.workdir_dirty = true;
                let d = self.entry(path);
                d.worktree_status = b'D';
                d.mode_index = 0o160000;
                d.oid_index = *oid;
                continue;
            } else {
                let Ok(sub) = Repository::open(&dir) else {
                    continue;
                };
                let head = sub.head().ok().and_then(|h| h.target());
                let new_commits = head.is_some_and(|h| h != *oid);
                let mut dirty = 0;
                if ignore != "dirty" {
                    dirty = submodule_dirty(
                        &sub,
                        ignore != "untracked" && self.untracked_mode != Untracked::No,
                    );
                }
                (new_commits, dirty)
            };
            if !new_commits && dirty == 0 {
                continue;
            }
            self.workdir_dirty = true;
            let short = self.format == StatusFormat::Short;
            let d = self.entry(path);
            d.dirty_submodule = dirty;
            d.new_submodule_commits = new_commits;
            d.worktree_status = if !short || new_commits {
                b'M'
            } else if dirty & DIRTY_MODIFIED != 0 {
                b'm'
            } else {
                b'?'
            };
            d.mode_index = 0o160000;
            d.mode_worktree = 0o160000;
            d.oid_index = *oid;
        }
        Ok(())
    }

    /// Pair files deleted from the work tree with intent-to-add ones as
    /// renames, as git's diff-files does, seeing the latter as new files.
    fn pair_ita_renames(&mut self, ita: &HashSet<String>) -> Result<(), GitError> {
        let deleted: Vec<String> = self
            .change
            .iter()
            .filter(|(_, d)| d.worktree_status == b'D')
            .map(|(p, _)| p.clone())
            .collect();
        let added: Vec<&String> = ita
            .iter()
            .filter(|p| {
                self.change
                    .get(*p)
                    .is_some_and(|d| d.worktree_status == b'A')
            })
            .collect();
        if deleted.is_empty() || added.is_empty() {
            return Ok(());
        }
        let (Some(mut find), Some(index)) = (self.rename_find(), self.tracked_index(ita)?) else {
            return Ok(());
        };
        let mut o = DiffOptions::new();
        o.include_untracked(true)
            .recurse_untracked_dirs(true)
            .disable_pathspec_match(true)
            .ignore_submodules(true);
        for p in deleted.iter().chain(added) {
            o.pathspec(p);
        }
        let mut diff = self
            .repo
            .diff_index_to_workdir(Some(&index), Some(&mut o))?;
        find.for_untracked(true);
        diff.find_similar(Some(&mut find))?;
        for delta in diff.deltas() {
            if delta.status() != Delta::Renamed {
                continue;
            }
            let (one, two) = (delta.old_file(), delta.new_file());
            let from = one
                .path()
                .map_or(String::new(), |p| p.to_string_lossy().into());
            let score = crate::git_repo::similarity(self.repo, &delta);
            if self.change.get(&from).is_some_and(|d| d.index_status == 0) {
                self.change.remove(&from);
            } else if let Some(d) = self.change.get_mut(&from) {
                d.worktree_status = 0;
            }
            let d = self.entry(&delta_path(&delta));
            d.worktree_status = b'R';
            d.rename_source = Some(from);
            d.rename_status = b'R';
            d.rename_score = score;
            d.mode_index = one.mode().into();
            d.oid_index = one.id();
            d.mode_worktree = two.mode().into();
        }
        Ok(())
    }

    /// The submodule at `path`'s ignore mode: `--ignore-submodules`, else
    /// its own `submodule.<name>.ignore` (config, then .gitmodules), else
    /// the default.
    fn sub_ignore(&self, path: &str) -> String {
        if let Some(v) = &self.opts.ignore_submodules {
            return v.clone();
        }
        crate::submodule::ignore_of(self.repo, path).unwrap_or_else(|| self.submodule_ignore())
    }

    fn submodule_ignore(&self) -> String {
        if let Some(v) = &self.opts.ignore_submodules {
            return v.clone();
        }
        if let Ok(v) = self.cfg.get_string("diff.ignoreSubmodules") {
            return v;
        }
        if self.untracked_mode == Untracked::No {
            "untracked".to_owned()
        } else {
            "none".to_owned()
        }
    }

    fn collect_initial(&mut self, unmerged: &BTreeMap<String, u8>) {
        for e in self.index.iter() {
            let path = String::from_utf8_lossy(&e.path).into_owned();
            if !self.in_spec(&path) || e.flags_extended & (1 << 13) != 0 {
                continue;
            }
            self.committable = true;
            if let Some(mask) = unmerged.get(&path) {
                let d = self.entry(&path);
                d.index_status = b'U';
                d.stagemask = *mask;
            } else {
                let d = self.entry(&path);
                d.index_status = b'A';
                d.mode_index = e.mode;
                d.oid_index = e.id;
            }
        }
    }

    fn rename_find(&self) -> Option<DiffFindOptions> {
        if self.detect_rename == 0 {
            return None;
        }
        let mut f = DiffFindOptions::new();
        f.renames(true);
        if self.detect_rename == 2 {
            f.copies(true);
        }
        if let Some(s) = self.rename_score {
            f.rename_threshold(s).copy_threshold(s);
        }
        if let Some(n) = self.rename_limit {
            f.rename_limit(if n == 0 { usize::MAX >> 1 } else { n });
        }
        git_metric(&mut f);
        Some(f)
    }

    fn tracked_index(&self, ita: &HashSet<String>) -> Result<Option<Index>, GitError> {
        if ita.is_empty() {
            return Ok(None);
        }
        let mut idx = Index::new()?;
        for e in self.index.iter() {
            if e.flags_extended & (1 << 13) == 0 {
                idx.add(&e)?;
            }
        }
        Ok(Some(idx))
    }

    fn reference_tree(&self) -> Option<git2::Tree<'r>> {
        self.repo
            .rev_single(&self.reference)
            .ok()?
            .peel_to_tree()
            .ok()
    }

    fn collect_index(
        &mut self,
        ita: &HashSet<String>,
        unmerged: &BTreeMap<String, u8>,
    ) -> Result<(), GitError> {
        let tree = self.reference_tree();
        let filtered = self.tracked_index(ita)?;
        let index = filtered.as_ref().unwrap_or(self.index);
        let mut o = DiffOptions::new();
        o.include_typechange(true);
        let mut diff = self
            .repo
            .diff_tree_to_index(tree.as_ref(), Some(index), Some(&mut o))?;
        if let Some(mut f) = self.rename_find() {
            diff.find_similar(Some(&mut f))?;
        }
        let hide_gitlinks = self.submodule_ignore() == "all";
        for delta in diff.deltas() {
            let path = delta_path(&delta);
            if unmerged.contains_key(&path) || !self.in_spec(&path) {
                continue;
            }
            let gitlink = |f: git2::DiffFile| f.mode() == git2::FileMode::Commit;
            if hide_gitlinks && (gitlink(delta.old_file()) || gitlink(delta.new_file())) {
                continue;
            }
            let Some(status) = delta_char(delta.status()) else {
                continue;
            };
            let score = matches!(status, b'R' | b'C')
                .then(|| crate::git_repo::similarity(self.repo, &delta));
            let (one, two) = (delta.old_file(), delta.new_file());
            self.committable = true;
            let d = self.entry(&path);
            if d.index_status == 0 {
                d.index_status = status;
            }
            match status {
                b'A' => {
                    d.mode_index = two.mode().into();
                    d.oid_index = two.id();
                }
                b'D' => {
                    d.mode_head = one.mode().into();
                    d.oid_head = one.id();
                }
                _ => {
                    if let Some(score) = score {
                        d.rename_source = Some(
                            one.path()
                                .map_or(String::new(), |p| p.to_string_lossy().into()),
                        );
                        d.rename_score = score;
                        d.rename_status = status;
                    }
                    d.mode_head = one.mode().into();
                    d.mode_index = two.mode().into();
                    d.oid_head = one.id();
                    d.oid_index = two.id();
                }
            }
        }
        for (path, mask) in unmerged {
            if !self.in_spec(path) {
                continue;
            }
            let d = self.entry(path);
            d.index_status = b'U';
            d.stagemask = *mask;
        }
        Ok(())
    }

    fn collect_untracked(&mut self) -> Result<(), GitError> {
        if self.untracked_mode == Untracked::No {
            return Ok(());
        }
        let Some(workdir) = self.repo.workdir().map(Path::to_path_buf) else {
            return Ok(());
        };
        let mut files = HashSet::new();
        let mut dirs = HashSet::new();
        for e in self.index.iter() {
            let path = String::from_utf8_lossy(&e.path).into_owned();
            let mut p = path.as_str();
            while let Some(i) = p.rfind('/') {
                p = &p[..i];
                if !dirs.insert(p.to_owned()) {
                    break;
                }
            }
            files.insert(path);
        }
        let walk = Walk {
            repo: self.repo,
            workdir: &workdir,
            files: &files,
            dirs: &dirs,
            untracked: self.untracked_mode,
            ignored: self.ignored_mode,
            spec: self.spec.as_ref(),
        };
        let mut unt = Vec::new();
        let mut ign = Vec::new();
        walk.tracked_dir("", &mut unt, &mut ign);
        self.untracked.extend(unt);
        if self.ignored_mode != Ignored::No {
            self.ignored.extend(ign);
        }
        Ok(())
    }

    fn read_git(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.repo.path().join(name)).ok()
    }

    fn abbrev(&self, oid: Oid) -> String {
        self.repo
            .find_object(oid, None)
            .ok()
            .and_then(|o| o.short_id().ok())
            .and_then(|b| b.as_str().ok().map(str::to_owned))
            .unwrap_or_else(|| oid.to_string()[..7].to_owned())
    }

    /// git's get_branch(): a rebase or bisect state file naming a branch.
    fn state_branch(&self, name: &str) -> Option<String> {
        let text = self.read_git(name)?;
        let text = text.trim_end_matches('\n');
        if text.is_empty() {
            return None;
        }
        if let Some(b) = text.strip_prefix("refs/heads/") {
            return Some(b.to_owned());
        }
        if text.starts_with("refs/") {
            return Some(text.to_owned());
        }
        if let Ok(oid) = Oid::from_str(text)
            && text.len() == 40
        {
            return Some(self.abbrev(oid));
        }
        if text == "detached HEAD" {
            return None;
        }
        Some(text.to_owned())
    }

    fn get_state(&mut self) {
        let git = self.repo.path().to_path_buf();
        let check_rebase = |wt: &Self, st: &mut State| -> bool {
            if git.join("rebase-apply").is_dir() {
                if git.join("rebase-apply/applying").exists() {
                    st.am = true;
                    st.am_empty_patch = std::fs::metadata(git.join("rebase-apply/patch"))
                        .is_ok_and(|m| m.len() == 0);
                } else {
                    st.rebase = true;
                    st.branch = wt.state_branch("rebase-apply/head-name");
                    st.onto = wt.state_branch("rebase-apply/onto");
                }
            } else if git.join("rebase-merge").is_dir() {
                if git.join("rebase-merge/interactive").exists() {
                    st.rebase_interactive = true;
                } else {
                    st.rebase = true;
                }
                st.branch = wt.state_branch("rebase-merge/head-name");
                st.onto = wt.state_branch("rebase-merge/onto");
            } else {
                return false;
            }
            true
        };
        let mut st = State::default();
        if git.join("MERGE_HEAD").exists() {
            check_rebase(self, &mut st);
            st.merge = true;
        } else if check_rebase(self, &mut st) {
        } else if let Ok(oid) = self.repo.refname_to_id("CHERRY_PICK_HEAD") {
            st.cherry_pick = true;
            st.cherry_pick_head = Some(oid);
        }
        if git.join("BISECT_LOG").exists() {
            st.bisect = true;
            st.bisecting_from = self.state_branch("BISECT_START");
        }
        if let Ok(oid) = self.repo.refname_to_id("REVERT_HEAD") {
            st.revert = true;
            st.revert_head = Some(oid);
        }
        if let Some(todo) = self.read_git("sequencer/todo") {
            let line = todo.trim_start_matches([' ', '\t', '\r', '\n']);
            let word = |w: &str| {
                line.strip_prefix(w)
                    .is_some_and(|r| r.starts_with([' ', '\t']))
            };
            if (word("pick") || word("p")) && !st.cherry_pick {
                st.cherry_pick = true;
                st.cherry_pick_head = None;
            } else if word("revert") && !st.revert {
                st.revert = true;
                st.revert_head = None;
            }
        }
        if self.branch.as_deref() == Some("HEAD") {
            self.detached_from(&mut st);
        }
        if self.cfg.get_bool("core.sparseCheckout").unwrap_or(false) && !self.index.is_empty() {
            let skip = self
                .index
                .iter()
                .filter(|e| e.flags_extended & (1 << 14) != 0)
                .count();
            st.sparse = Some((100 - 100 * skip / self.index.len()) as u32);
        }
        self.state = st;
    }

    fn detached_from(&self, st: &mut State) {
        let Ok(log) = self.repo.reflog("HEAD") else {
            return;
        };
        for e in log.iter() {
            let Ok(Some(msg)) = e.message() else { continue };
            let Some(rest) = msg.strip_prefix("checkout: moving from ") else {
                continue;
            };
            let Some(i) = rest.find(" to ") else { continue };
            let target = rest[i + 4..].lines().next().unwrap_or("");
            let noid = e.id_new();
            let target = if target == "HEAD" {
                self.abbrev(noid)
            } else {
                target.to_owned()
            };
            let from = self
                .repo
                .resolve_reference_from_short_name(&target)
                .ok()
                .filter(|r| {
                    r.target() == Some(noid) || r.peel_to_commit().is_ok_and(|c| c.id() == noid)
                })
                .and_then(|r| r.name().ok().map(str::to_owned))
                .map(|name| {
                    name.strip_prefix("refs/tags/")
                        .or_else(|| name.strip_prefix("refs/remotes/"))
                        .unwrap_or(&name)
                        .to_owned()
                });
            st.detached_from = Some(from.unwrap_or_else(|| self.abbrev(noid)));
            st.detached_at = self
                .repo
                .head()
                .ok()
                .and_then(|h| h.target())
                .is_some_and(|h| h == noid);
            return;
        }
    }

    fn has_unmerged(&self) -> bool {
        self.change.values().any(|d| d.stagemask != 0)
    }

    // ----- output helpers -----

    fn color(&self, slot: Slot) -> String {
        if self.use_color {
            self.palette[slot as usize].clone()
        } else {
            String::new()
        }
    }

    fn put_colored(&mut self, color: &str, text: &str) {
        if !color.is_empty() {
            self.out.extend_from_slice(color.as_bytes());
        }
        self.out.extend_from_slice(text.as_bytes());
        if !color.is_empty() {
            self.out.extend_from_slice(RESET.as_bytes());
        }
    }

    /// wt-status.c's status_vprintf.
    fn vprintf(&mut self, mut at_bol: bool, color: &str, text: &str, trail: Option<&str>) {
        if text.is_empty() {
            let mut sb = String::new();
            if self.comment {
                sb.push('#');
                if trail.is_none() {
                    sb.push(' ');
                }
            }
            self.put_colored(color, &sb);
            if let Some(t) = trail {
                self.out.extend_from_slice(t.as_bytes());
            }
            return;
        }
        let mut rest = text;
        loop {
            let eol = rest.find('\n');
            let line = &rest[..eol.unwrap_or(rest.len())];
            let mut buf = String::new();
            if at_bol && self.comment {
                buf.push('#');
                if !rest.starts_with('\n') && !rest.starts_with('\t') {
                    buf.push(' ');
                }
            }
            buf.push_str(line);
            self.put_colored(color, &buf);
            match eol {
                Some(i) => {
                    self.out.push(b'\n');
                    rest = &rest[i + 1..];
                    if rest.is_empty() {
                        break;
                    }
                }
                None => break,
            }
            at_bol = true;
        }
        if let Some(t) = trail {
            self.out.extend_from_slice(t.as_bytes());
        }
    }

    fn ln(&mut self, color: &str, text: &str) {
        self.vprintf(true, color, text, Some("\n"));
    }

    fn printf(&mut self, color: &str, text: &str) {
        self.vprintf(true, color, text, None);
    }

    fn more(&mut self, color: &str, text: &str) {
        self.vprintf(false, color, text, None);
    }

    fn header_ln(&mut self, text: &str) {
        let c = self.color(Slot::Header);
        self.ln(&c, text);
    }

    fn raw(&mut self, text: &str) {
        self.out.extend_from_slice(text.as_bytes());
    }

    fn quote(&self, path: &str, quote_sp: bool) -> String {
        let rel = match &self.prefix {
            Some(p) => relative_path(path, p),
            None => path.to_owned(),
        };
        let quote_path = self.cfg.get_bool("core.quotePath").unwrap_or(true);
        quote_c(&rel, quote_sp, quote_path)
    }

    // ----- printing -----

    fn print(&mut self) -> Result<(), GitError> {
        match self.format {
            StatusFormat::Long => self.print_long()?,
            StatusFormat::Short => self.print_short(),
            StatusFormat::Porcelain => {
                self.use_color = false;
                self.prefix = None;
                self.print_short();
            }
            StatusFormat::PorcelainV2 => self.print_v2(),
        }
        Ok(())
    }

    fn tracking(&self, branch: &str) -> Option<Tracking> {
        let full = format!("refs/heads/{branch}");
        let upstream = self.repo.branch_upstream_name(&full).ok()?;
        let upstream = upstream.as_str().ok()?.to_owned();
        let base = shorten_ref(&upstream);
        let (Ok(ours), Ok(theirs)) = (
            self.repo.refname_to_id(&full),
            self.repo.refname_to_id(&upstream),
        ) else {
            return Some(Tracking {
                base,
                gone: true,
                counts: None,
                same: false,
            });
        };
        if ours == theirs {
            return Some(Tracking {
                base,
                gone: false,
                counts: Some((0, 0)),
                same: true,
            });
        }
        let counts = match self.ahead_behind {
            AheadBehind::Quick => None,
            AheadBehind::Full => self.repo.graph_ahead_behind(ours, theirs).ok(),
        };
        Some(Tracking {
            base,
            gone: false,
            same: counts == Some((0, 0)),
            counts,
        })
    }

    fn print_tracking_long(&mut self, branch: &str) {
        let Some(t) = self.tracking(branch) else {
            return;
        };
        let hints = self.cfg.get_bool("advice.statusHints").unwrap_or(true);
        let base = &t.base;
        let mut sb = String::new();
        if t.gone {
            sb.push_str(&format!(
                "Your branch is based on '{base}', but the upstream is gone.\n"
            ));
            if hints {
                sb.push_str("  (use \"git branch --unset-upstream\" to fixup)\n");
            }
        } else if t.same {
            sb.push_str(&format!("Your branch is up to date with '{base}'.\n"));
        } else if let Some((ours, theirs)) = t.counts {
            let s = |n: usize| if n == 1 { "" } else { "s" };
            if theirs == 0 {
                sb.push_str(&format!(
                    "Your branch is ahead of '{base}' by {ours} commit{}.\n",
                    s(ours)
                ));
                if hints {
                    sb.push_str("  (use \"git push\" to publish your local commits)\n");
                }
            } else if ours == 0 {
                sb.push_str(&format!(
                    "Your branch is behind '{base}' by {theirs} commit{}, and can be fast-forwarded.\n",
                    s(theirs)
                ));
                if hints {
                    sb.push_str("  (use \"git pull\" to update your local branch)\n");
                }
            } else {
                sb.push_str(&format!(
                    "Your branch and '{base}' have diverged,\nand have {ours} and {theirs} different commit{} each, respectively.\n",
                    s(ours + theirs)
                ));
                if hints && !self.opts.commit {
                    sb.push_str(
                        "  (use \"git pull\" if you want to integrate the remote branch with yours)\n",
                    );
                }
            }
        } else {
            sb.push_str(&format!(
                "Your branch and '{base}' refer to different commits.\n"
            ));
            if hints {
                sb.push_str("  (use \"git status --ahead-behind\" for details)\n");
            }
        }
        let c = self.color(Slot::Header);
        for line in sb.lines() {
            let text = if self.comment {
                format!("# {line}")
            } else {
                line.to_owned()
            };
            self.put_colored(&c, &text);
            self.raw("\n");
        }
        if self.comment {
            self.put_colored(&c, "#");
            self.raw("\n");
        } else {
            self.raw("\n");
        }
    }

    fn print_long(&mut self) -> Result<(), GitError> {
        let header = self.color(Slot::Header);
        if let Some(full) = self.branch.clone() {
            let mut branch_status_color = header.clone();
            let branch_color = self.color(Slot::OnBranch);
            let (on_what, name) = if full == "HEAD" {
                branch_status_color = self.color(Slot::NoBranch);
                if self.state.rebase || self.state.rebase_interactive {
                    let what = if self.state.rebase_interactive {
                        "interactive rebase in progress; onto "
                    } else {
                        "rebase in progress; onto "
                    };
                    (what, self.state.onto.clone().unwrap_or_default())
                } else if let Some(from) = &self.state.detached_from {
                    let what = if self.state.detached_at {
                        "HEAD detached at "
                    } else {
                        "HEAD detached from "
                    };
                    (what, from.clone())
                } else {
                    ("Not currently on any branch.", String::new())
                }
            } else {
                (
                    "On branch ",
                    full.strip_prefix("refs/heads/").unwrap_or(&full).to_owned(),
                )
            };
            self.printf(&header, "");
            self.more(&branch_status_color, on_what);
            self.more(&branch_color, &format!("{name}\n"));
            if !self.is_initial
                && let Some(b) = full.strip_prefix("refs/heads/")
            {
                self.print_tracking_long(b);
            }
        }
        self.print_state();
        if self.is_initial {
            self.header_ln("");
            self.header_ln(if self.opts.commit {
                "Initial commit"
            } else {
                "No commits yet"
            });
            self.header_ln("");
        }
        self.print_updated();
        self.print_unmerged();
        self.print_changed();
        if self.submodule_summary != 0 && self.submodule_ignore() != "all" {
            self.print_submodule_summary(false);
            self.print_submodule_summary(true);
        }
        if self.untracked_mode != Untracked::No {
            let list: Vec<String> = self.untracked.iter().cloned().collect();
            self.print_other(&list, "Untracked files", "add");
            if self.ignored_mode != Ignored::No {
                let list: Vec<String> = self.ignored.iter().cloned().collect();
                self.print_other(&list, "Ignored files", "add -f");
            }
        } else if self.committable {
            let hint = if self.hints {
                " (use -u option to show untracked files)"
            } else {
                ""
            };
            self.ln("", &format!("Untracked files not listed{hint}"));
        }
        if self.opts.verbose > 0 {
            self.print_verbose()?;
        }
        if !self.committable {
            if self.opts.amend {
                self.ln("", "No changes");
            } else if self.opts.nowarn {
            } else if self.workdir_dirty {
                self.raw(if self.hints {
                    "no changes added to commit (use \"git add\" and/or \"git commit -a\")\n"
                } else {
                    "no changes added to commit\n"
                });
            } else if !self.untracked.is_empty() {
                self.raw(if self.hints {
                    "nothing added to commit but untracked files present (use \"git add\" to track)\n"
                } else {
                    "nothing added to commit but untracked files present\n"
                });
            } else if self.is_initial {
                self.raw(if self.hints {
                    "nothing to commit (create/copy files and use \"git add\" to track)\n"
                } else {
                    "nothing to commit\n"
                });
            } else if self.untracked_mode == Untracked::No {
                self.raw(if self.hints {
                    "nothing to commit (use -u to show untracked files)\n"
                } else {
                    "nothing to commit\n"
                });
            } else {
                self.raw("nothing to commit, working tree clean\n");
            }
        }
        if self.show_stash {
            let n = self.stash_count();
            if n > 0 {
                let s = if n == 1 { "entry" } else { "entries" };
                self.ln("", &format!("Your stash currently has {n} {s}"));
            }
        }
        Ok(())
    }

    /// git's wt_longstatus_print_submodule_summary: `submodule summary
    /// --for-status` of the staged (`--cached`) or unstaged (`--files`)
    /// submodule changes, under a heading.
    fn print_submodule_summary(&mut self, unstaged: bool) {
        use crate::submodule::{checked_out, index_gitlinks, summary_lines, tree_gitlinks};
        let index = index_gitlinks(self.index);
        let (src, dst) = match unstaged {
            true => (index.clone(), checked_out(self.repo, &index)),
            false => {
                let head = self.reference_tree().and_then(|t| tree_gitlinks(&t).ok());
                (head.unwrap_or_default(), index)
            }
        };
        let limit = self.submodule_summary.max(0) as usize;
        let lines = summary_lines(self.repo, &src, &dst, &[], limit, true);
        if lines.is_empty() {
            return;
        }
        let mut text = match unstaged {
            true => "Submodules changed but not updated:\n\n",
            false => "Submodule changes to be committed:\n\n",
        }
        .to_owned();
        for l in lines {
            text.push_str(&l);
            text.push('\n');
        }
        self.printf("", &text);
    }

    fn stash_count(&self) -> usize {
        self.repo.reflog("refs/stash").map_or(0, |r| r.len())
    }

    fn trailer(&mut self) {
        self.header_ln("");
    }

    fn print_state(&mut self) {
        let c = self.color(Slot::Header);
        if self.state.merge {
            if self.state.rebase_interactive {
                self.show_rebase_information(&c);
                self.raw("\n");
            }
            if self.has_unmerged() {
                self.ln(&c, "You have unmerged paths.");
                if self.hints {
                    self.ln(&c, "  (fix conflicts and run \"git commit\")");
                    self.ln(&c, "  (use \"git merge --abort\" to abort the merge)");
                }
            } else {
                self.ln(&c, "All conflicts fixed but you are still merging.");
                if self.hints {
                    self.ln(&c, "  (use \"git commit\" to conclude merge)");
                }
            }
            self.trailer();
        } else if self.state.am {
            self.ln(&c, "You are in the middle of an am session.");
            let empty = self.state.am_empty_patch;
            if empty {
                self.ln(&c, "The current patch is empty.");
            }
            if self.hints {
                if !empty {
                    self.ln(&c, "  (fix conflicts and then run \"git am --continue\")");
                }
                self.ln(&c, "  (use \"git am --skip\" to skip this patch)");
                if empty {
                    self.ln(
                        &c,
                        "  (use \"git am --allow-empty\" to record this patch as an empty commit)",
                    );
                }
                self.ln(
                    &c,
                    "  (use \"git am --abort\" to restore the original branch)",
                );
            }
            self.trailer();
        } else if self.state.rebase || self.state.rebase_interactive {
            self.show_rebase_in_progress(&c);
        } else if self.state.cherry_pick {
            match self.state.cherry_pick_head {
                None => self.ln(&c, "Cherry-pick currently in progress."),
                Some(oid) => {
                    let a = self.abbrev(oid);
                    self.ln(&c, &format!("You are currently cherry-picking commit {a}."));
                }
            }
            if self.hints {
                if self.has_unmerged() {
                    self.ln(
                        &c,
                        "  (fix conflicts and run \"git cherry-pick --continue\")",
                    );
                } else if self.state.cherry_pick_head.is_none() {
                    self.ln(&c, "  (run \"git cherry-pick --continue\" to continue)");
                } else {
                    self.ln(
                        &c,
                        "  (all conflicts fixed: run \"git cherry-pick --continue\")",
                    );
                }
                self.ln(&c, "  (use \"git cherry-pick --skip\" to skip this patch)");
                self.ln(
                    &c,
                    "  (use \"git cherry-pick --abort\" to cancel the cherry-pick operation)",
                );
            }
            self.trailer();
        } else if self.state.revert {
            match self.state.revert_head {
                None => self.ln(&c, "Revert currently in progress."),
                Some(oid) => {
                    let a = self.abbrev(oid);
                    self.ln(&c, &format!("You are currently reverting commit {a}."));
                }
            }
            if self.hints {
                if self.has_unmerged() {
                    self.ln(&c, "  (fix conflicts and run \"git revert --continue\")");
                } else if self.state.revert_head.is_none() {
                    self.ln(&c, "  (run \"git revert --continue\" to continue)");
                } else {
                    self.ln(&c, "  (all conflicts fixed: run \"git revert --continue\")");
                }
                self.ln(&c, "  (use \"git revert --skip\" to skip this patch)");
                self.ln(
                    &c,
                    "  (use \"git revert --abort\" to cancel the revert operation)",
                );
            }
            self.trailer();
        }
        if self.state.bisect {
            match self.state.bisecting_from.clone() {
                Some(b) => self.ln(
                    &c,
                    &format!("You are currently bisecting, started from branch '{b}'."),
                ),
                None => self.ln(&c, "You are currently bisecting."),
            }
            if self.hints {
                self.ln(
                    &c,
                    "  (use \"git bisect reset\" to get back to the original branch)",
                );
            }
            self.trailer();
        }
        if let Some(pct) = self.state.sparse {
            self.ln(
                &c,
                &format!("You are in a sparse checkout with {pct}% of tracked files present."),
            );
            self.trailer();
        }
    }

    fn todo_lines(&self, name: &str) -> Option<Vec<String>> {
        let text = self.read_git(name)?;
        Some(
            text.lines()
                .filter(|l| !l.starts_with('#'))
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| self.abbrev_line(l))
                .collect(),
        )
    }

    fn abbrev_line(&self, line: &str) -> String {
        if ["exec ", "x ", "label ", "l "]
            .iter()
            .any(|p| line.starts_with(p))
        {
            return line.to_owned();
        }
        let mut parts = line.splitn(3, ' ');
        let (Some(cmd), Some(id)) = (parts.next(), parts.next()) else {
            return line.to_owned();
        };
        let rest = parts.next();
        let Ok(obj) = self.repo.rev_single(id.trim()) else {
            return line.to_owned();
        };
        let mut out = format!("{cmd} {}", self.abbrev(obj.id()));
        match rest {
            Some(r) => {
                out.push(' ');
                out.push_str(r);
            }
            None if id.ends_with(' ') => out.push(' '),
            None => {}
        }
        out
    }

    fn show_rebase_information(&mut self, c: &str) {
        if !self.state.rebase_interactive {
            return;
        }
        let done = self.todo_lines("rebase-merge/done").unwrap_or_default();
        let todo = self.todo_lines("rebase-merge/git-rebase-todo");
        if todo.is_none() {
            self.ln(c, "git-rebase-todo is missing.");
        }
        let todo = todo.unwrap_or_default();
        if done.is_empty() {
            self.ln(c, "No commands done.");
        } else {
            let n = done.len();
            if n == 1 {
                self.ln(c, "Last command done (1 command done):");
            } else {
                self.ln(c, &format!("Last commands done ({n} commands done):"));
            }
            for l in &done[n.saturating_sub(2)..] {
                self.ln(c, &format!("   {l}"));
            }
            if n > 2 && self.hints {
                let path = self.git_path("rebase-merge/done");
                self.ln(c, &format!("  (see more in file {path})"));
            }
        }
        if todo.is_empty() {
            self.ln(c, "No commands remaining.");
        } else {
            let n = todo.len();
            if n == 1 {
                self.ln(c, "Next command to do (1 remaining command):");
            } else {
                self.ln(c, &format!("Next commands to do ({n} remaining commands):"));
            }
            for l in todo.iter().take(2) {
                self.ln(c, &format!("   {l}"));
            }
            if self.hints {
                self.ln(c, "  (use \"git rebase --edit-todo\" to view and edit)");
            }
        }
    }

    fn git_path(&self, name: &str) -> String {
        let git = self.repo.path();
        if let Some(wd) = self.repo.workdir()
            && git.strip_prefix(wd).is_ok_and(|p| p == Path::new(".git"))
        {
            return format!(".git/{name}");
        }
        git.join(name).to_string_lossy().into_owned()
    }

    fn split_commit_in_progress(&self) -> bool {
        if (!self.opts.amend && !self.opts.nowarn && !self.workdir_dirty)
            || self.branch.as_deref() != Some("HEAD")
        {
            return false;
        }
        let (Ok(head), Ok(orig)) = (
            self.repo.refname_to_id("HEAD"),
            self.repo.refname_to_id("ORIG_HEAD"),
        ) else {
            return false;
        };
        let line = |n: &str| {
            self.read_git(n)
                .map(|s| s.lines().next().unwrap_or("").to_owned())
        };
        match (line("rebase-merge/amend"), line("rebase-merge/orig-head")) {
            (Some(amend), Some(orig_head)) if amend == orig_head => head.to_string() != amend,
            (Some(_), Some(orig_head)) => orig.to_string() != orig_head,
            _ => false,
        }
    }

    fn show_rebase_in_progress(&mut self, c: &str) {
        self.show_rebase_information(c);
        let rebasing = |wt: &mut Self| match (wt.state.branch.clone(), wt.state.onto.clone()) {
            (Some(b), onto) => wt.ln(
                c,
                &format!(
                    "You are currently rebasing branch '{b}' on '{}'.",
                    onto.unwrap_or_default()
                ),
            ),
            (None, _) => wt.ln(c, "You are currently rebasing."),
        };
        if self.has_unmerged() {
            rebasing(self);
            if self.hints {
                self.ln(
                    c,
                    "  (fix conflicts and then run \"git rebase --continue\")",
                );
                self.ln(c, "  (use \"git rebase --skip\" to skip this patch)");
                self.ln(
                    c,
                    "  (use \"git rebase --abort\" to check out the original branch)",
                );
            }
        } else if self.state.rebase || self.repo.path().join("MERGE_MSG").exists() {
            rebasing(self);
            if self.hints {
                self.ln(c, "  (all conflicts fixed: run \"git rebase --continue\")");
            }
        } else if self.split_commit_in_progress() {
            match (self.state.branch.clone(), self.state.onto.clone()) {
                (Some(b), onto) => self.ln(
                    c,
                    &format!(
                        "You are currently splitting a commit while rebasing branch '{b}' on '{}'.",
                        onto.unwrap_or_default()
                    ),
                ),
                (None, _) => self.ln(c, "You are currently splitting a commit during a rebase."),
            }
            if self.hints {
                self.ln(
                    c,
                    "  (Once your working directory is clean, run \"git rebase --continue\")",
                );
            }
        } else {
            match (self.state.branch.clone(), self.state.onto.clone()) {
                (Some(b), onto) => self.ln(
                    c,
                    &format!(
                        "You are currently editing a commit while rebasing branch '{b}' on '{}'.",
                        onto.unwrap_or_default()
                    ),
                ),
                (None, _) => self.ln(c, "You are currently editing a commit during a rebase."),
            }
            if self.hints && !self.opts.amend {
                self.ln(
                    c,
                    "  (use \"git commit --amend\" to amend the current commit)",
                );
                self.ln(
                    c,
                    "  (use \"git rebase --continue\" once you are satisfied with your changes)",
                );
            }
        }
        self.trailer();
    }

    fn print_updated(&mut self) {
        let paths: Vec<String> = self
            .change
            .iter()
            .filter(|(_, d)| d.index_status != 0 && d.index_status != b'U')
            .map(|(p, _)| p.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        let c = self.color(Slot::Header);
        self.ln(&c, "Changes to be committed:");
        if self.hints && self.from_commit {
            if self.is_initial {
                self.ln(&c, "  (use \"git rm --cached <file>...\" to unstage)");
            } else if self.reference == "HEAD" {
                self.ln(&c, "  (use \"git restore --staged <file>...\" to unstage)");
            } else {
                let r = self.reference.clone();
                self.ln(
                    &c,
                    &format!("  (use \"git restore --source={r} --staged <file>...\" to unstage)"),
                );
            }
        }
        for p in paths {
            self.print_change_data(true, &p);
        }
        self.trailer();
    }

    fn print_unmerged(&mut self) {
        let paths: Vec<(String, u8)> = self
            .change
            .iter()
            .filter(|(_, d)| d.stagemask != 0)
            .map(|(p, d)| (p.clone(), d.stagemask))
            .collect();
        if paths.is_empty() {
            return;
        }
        let c = self.color(Slot::Header);
        self.ln(&c, "Unmerged paths:");
        let (mut both_deleted, mut del_mod, mut not_deleted) = (false, false, false);
        for (_, m) in &paths {
            match m {
                1 => both_deleted = true,
                3 | 5 => del_mod = true,
                _ => not_deleted = true,
            }
        }
        if self.hints {
            if self.from_commit {
                if self.is_initial {
                    self.ln(&c, "  (use \"git rm --cached <file>...\" to unstage)");
                } else if self.reference == "HEAD" {
                    self.ln(&c, "  (use \"git restore --staged <file>...\" to unstage)");
                } else {
                    let r = self.reference.clone();
                    self.ln(
                        &c,
                        &format!(
                            "  (use \"git restore --source={r} --staged <file>...\" to unstage)"
                        ),
                    );
                }
            }
            if !both_deleted {
                if !del_mod {
                    self.ln(&c, "  (use \"git add <file>...\" to mark resolution)");
                } else {
                    self.ln(
                        &c,
                        "  (use \"git add/rm <file>...\" as appropriate to mark resolution)",
                    );
                }
            } else if !del_mod && !not_deleted {
                self.ln(&c, "  (use \"git rm <file>...\" to mark resolution)");
            } else {
                self.ln(
                    &c,
                    "  (use \"git add/rm <file>...\" as appropriate to mark resolution)",
                );
            }
        }
        let uc = self.color(Slot::Unmerged);
        for (p, m) in paths {
            let how = match m {
                1 => "both deleted:",
                2 => "added by us:",
                3 => "deleted by them:",
                4 => "added by them:",
                5 => "deleted by us:",
                6 => "both added:",
                _ => "both modified:",
            };
            let one = self.quote(&p, false);
            self.printf(&c, "\t");
            self.more(&uc, &format!("{how:<17}{one}\n"));
        }
        self.trailer();
    }

    fn print_changed(&mut self) {
        let mut deleted = false;
        let mut dirty_sub = false;
        let paths: Vec<String> = self
            .change
            .iter()
            .filter(|(_, d)| d.worktree_status != 0 && d.worktree_status != b'U')
            .inspect(|(_, d)| {
                deleted |= d.worktree_status == b'D';
                dirty_sub |= d.dirty_submodule != 0;
            })
            .map(|(p, _)| p.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        let c = self.color(Slot::Header);
        self.ln(&c, "Changes not staged for commit:");
        if self.hints {
            if deleted {
                self.ln(
                    &c,
                    "  (use \"git add/rm <file>...\" to update what will be committed)",
                );
            } else {
                self.ln(
                    &c,
                    "  (use \"git add <file>...\" to update what will be committed)",
                );
            }
            self.ln(
                &c,
                "  (use \"git restore <file>...\" to discard changes in working directory)",
            );
            if dirty_sub {
                self.ln(
                    &c,
                    "  (commit or discard the untracked or modified content in submodules)",
                );
            }
        }
        for p in paths {
            self.print_change_data(false, &p);
        }
        self.trailer();
    }

    fn print_change_data(&mut self, updated: bool, path: &str) {
        let d = &self.change[path];
        let mut extra = String::new();
        let status = if updated {
            d.index_status
        } else {
            if d.new_submodule_commits || d.dirty_submodule != 0 {
                extra.push_str(" (");
                if d.new_submodule_commits {
                    extra.push_str("new commits, ");
                }
                if d.dirty_submodule & DIRTY_MODIFIED != 0 {
                    extra.push_str("modified content, ");
                }
                if d.dirty_submodule & DIRTY_UNTRACKED != 0 {
                    extra.push_str("untracked content, ");
                }
                extra.truncate(extra.len() - 2);
                extra.push(')');
            }
            d.worktree_status
        };
        let one_name = if d.rename_status == status && status != 0 {
            d.rename_source.clone()
        } else {
            None
        };
        let what = match status {
            b'A' => "new file:",
            b'C' => "copied:",
            b'D' => "deleted:",
            b'M' | b'm' | b'?' => "modified:",
            b'R' => "renamed:",
            b'T' => "typechange:",
            b'X' => "unknown:",
            _ => "unmerged:",
        };
        let c = self.color(if updated {
            Slot::Updated
        } else {
            Slot::Changed
        });
        let header = self.color(Slot::Header);
        let two = self.quote(path, false);
        self.printf(&header, "\t");
        match one_name {
            Some(src) => {
                let one = self.quote(&src, false);
                self.more(&c, &format!("{what:<12}{one} -> {two}"));
            }
            None => self.more(&c, &format!("{what:<12}{two}")),
        }
        if !extra.is_empty() {
            self.more(&header, &extra);
        }
        self.more("", "\n");
    }

    fn print_other(&mut self, list: &[String], what: &str, how: &str) {
        if list.is_empty() {
            return;
        }
        let c = self.color(Slot::Header);
        self.ln(&c, &format!("{what}:"));
        if self.hints {
            self.ln(
                &c,
                &format!("  (use \"git {how} <file>...\" to include in what will be committed)"),
            );
        }
        let uc = self.color(Slot::Untracked);
        let paths: Vec<String> = list.iter().map(|p| self.quote(p, false)).collect();
        if crate::text::column_active(self.colopts) {
            let indent = format!("{c}{}\t{uc}", if self.comment { "#" } else { "" });
            let nl = if self.use_color {
                format!("{RESET}\n")
            } else {
                "\n".to_owned()
            };
            let width = if self.opts.width == 0 {
                80
            } else {
                self.opts.width
            };
            let text = crate::text::columns(
                &paths,
                self.colopts,
                &crate::text::ColumnOpts {
                    width: width - 1,
                    indent: &indent,
                    nl: &nl,
                    padding: 1,
                },
            );
            self.raw(&text);
        } else {
            for p in paths {
                self.printf(&c, "\t");
                self.more(&uc, &format!("{p}\n"));
            }
        }
        self.ln("", "");
    }

    fn prefixes(&self) -> (String, String) {
        if self.cfg.get_bool("diff.noprefix").unwrap_or(false) {
            (String::new(), String::new())
        } else {
            ("a/".to_owned(), "b/".to_owned())
        }
    }

    fn print_verbose(&mut self) -> Result<(), GitError> {
        let c = self.color(Slot::Header);
        let color = self.use_color && !self.opts.template;
        if self.opts.template {
            self.out.extend_from_slice(cut_line().as_bytes());
        }
        let (mut a, mut b) = self.prefixes();
        if self.opts.verbose > 1 && self.committable {
            if self.opts.template {
                self.trailer();
            }
            self.ln(&c, "Changes to be committed:");
            (a, b) = ("c/".to_owned(), "i/".to_owned());
        }
        let mut ita = Vec::new();
        let mut unmerged = BTreeSet::new();
        for e in self.index.iter() {
            let path = String::from_utf8_lossy(&e.path).into_owned();
            if (e.flags >> 12) & 3 != 0 {
                unmerged.insert(path);
            } else if e.flags_extended & (1 << 13) != 0 {
                ita.push(path);
            }
        }
        let unmerged: Vec<String> = unmerged.into_iter().collect();
        let tree = self.reference_tree();
        let filtered = self.tracked_index(&ita.iter().cloned().collect())?;
        let index = filtered.as_ref().unwrap_or(self.index);
        // git shows a type change as a deletion and an addition.
        let mut o = DiffOptions::new();
        o.old_prefix(&a).new_prefix(&b);
        let mut diff = self
            .repo
            .diff_tree_to_index(tree.as_ref(), Some(index), Some(&mut o))?;
        if let Some(mut f) = self.rename_find() {
            diff.find_similar(Some(&mut f))?;
        }
        let text = patch_text(self.repo, &[&diff], color, &unmerged)?;
        self.out.extend_from_slice(&text);
        let worktree = self
            .change
            .values()
            .any(|d| d.worktree_status != 0 && d.worktree_status != b'U');
        if self.opts.verbose > 1 && worktree {
            self.ln(&c, "--------------------------------------------------");
            self.ln(&c, "Changes not staged for commit:");
            // The deletions paired with intent-to-add files show as renames.
            let sources: Vec<String> = self
                .change
                .values()
                .filter(|d| d.worktree_status == b'R')
                .filter_map(|d| d.rename_source.clone())
                .collect();
            let mut rest = Index::new()?;
            for e in index.iter() {
                if !sources.contains(&String::from_utf8_lossy(&e.path).into_owned()) {
                    rest.add(&e)?;
                }
            }
            let mut o = DiffOptions::new();
            o.old_prefix("i/").new_prefix("w/");
            let rest = if sources.is_empty() { index } else { &rest };
            let diff = self.repo.diff_index_to_workdir(Some(rest), Some(&mut o))?;
            let mut diffs = vec![diff];
            if !ita.is_empty() {
                // Intent-to-add files diff as new files.
                let mut o = DiffOptions::new();
                o.old_prefix("i/")
                    .new_prefix("w/")
                    .include_untracked(true)
                    .show_untracked_content(true)
                    .recurse_untracked_dirs(true)
                    .disable_pathspec_match(true);
                for p in ita.iter().chain(&sources) {
                    o.pathspec(p);
                }
                let mut diff = self.repo.diff_index_to_workdir(Some(index), Some(&mut o))?;
                if let (false, Some(mut f)) = (sources.is_empty(), self.rename_find()) {
                    diff.find_similar(Some(f.for_untracked(true)))?;
                }
                diffs.push(diff);
            }
            let refs: Vec<&Diff> = diffs.iter().collect();
            let text = patch_text(self.repo, &refs, color, &unmerged)?;
            self.out.extend_from_slice(&text);
        }
        Ok(())
    }

    fn print_short(&mut self) {
        let eol = if self.opts.null { '\0' } else { '\n' };
        if self.show_branch {
            self.print_short_tracking();
        }
        let paths: Vec<String> = self.change.keys().cloned().collect();
        let updated = self.color(Slot::Updated);
        let changed = self.color(Slot::Changed);
        let unmerged = self.color(Slot::Unmerged);
        for p in paths {
            let d = &self.change[&p];
            if d.stagemask != 0 {
                let how = unmerged_key(d.stagemask);
                self.put_colored(&unmerged, how);
                if self.opts.null {
                    self.raw(&format!(" {p}\0"));
                } else {
                    let one = self.quote(&p, true);
                    self.raw(&format!(" {one}\n"));
                }
                continue;
            }
            let (x, y, src) = (d.index_status, d.worktree_status, d.rename_source.clone());
            if x != 0 {
                self.put_colored(&updated, &(x as char).to_string());
            } else {
                self.raw(" ");
            }
            if y != 0 {
                self.put_colored(&changed, &(y as char).to_string());
            } else {
                self.raw(" ");
            }
            self.raw(" ");
            if self.opts.null {
                self.raw(&format!("{p}\0"));
                if let Some(s) = src {
                    self.raw(&format!("{s}\0"));
                }
            } else {
                if let Some(s) = src {
                    let one = self.quote(&s, true);
                    self.raw(&format!("{one} -> "));
                }
                let one = self.quote(&p, true);
                self.raw(&format!("{one}{eol}"));
            }
        }
        let untracked = self.color(Slot::Untracked);
        let others: Vec<(&str, String)> = self
            .untracked
            .iter()
            .map(|p| ("??", p.clone()))
            .chain(self.ignored.iter().map(|p| ("!!", p.clone())))
            .collect();
        for (sign, p) in others {
            if self.opts.null {
                self.raw(&format!("{sign} {p}\0"));
            } else {
                let one = self.quote(&p, true);
                self.put_colored(&untracked, sign);
                self.raw(&format!(" {one}\n"));
            }
        }
    }

    fn print_short_tracking(&mut self) {
        let header = self.color(Slot::Header);
        let local = self.color(Slot::LocalBranch);
        let remote = self.color(Slot::RemoteBranch);
        self.put_colored(&header, "## ");
        let Some(full) = self.branch.clone() else {
            return;
        };
        if self.is_initial {
            self.put_colored(&header, "No commits yet on ");
        }
        if full == "HEAD" {
            let c = self.color(Slot::NoBranch);
            self.put_colored(&c, "HEAD (no branch)");
        } else {
            let name = full.strip_prefix("refs/heads/").unwrap_or(&full).to_owned();
            self.put_colored(&local, &name);
            if let Some(t) = self.tracking(&name) {
                self.put_colored(&header, "...");
                self.put_colored(&remote, &t.base);
                if t.gone || !t.same {
                    self.put_colored(&header, " [");
                    if t.gone {
                        self.put_colored(&header, "gone");
                    } else if let Some((ours, theirs)) = t.counts {
                        if ours == 0 {
                            self.put_colored(&header, "behind ");
                            self.put_colored(&remote, &theirs.to_string());
                        } else if theirs == 0 {
                            self.put_colored(&header, "ahead ");
                            self.put_colored(&local, &ours.to_string());
                        } else {
                            self.put_colored(&header, "ahead ");
                            self.put_colored(&local, &ours.to_string());
                            self.put_colored(&header, ", behind ");
                            self.put_colored(&remote, &theirs.to_string());
                        }
                    } else {
                        self.put_colored(&header, "different");
                    }
                    self.put_colored(&header, "]");
                }
            }
        }
        self.raw(if self.opts.null { "\0" } else { "\n" });
    }

    fn print_v2(&mut self) {
        let eol = if self.opts.null { '\0' } else { '\n' };
        if self.show_branch {
            let oid = if self.is_initial {
                "(initial)".to_owned()
            } else {
                self.oid_commit.to_string()
            };
            self.raw(&format!("# branch.oid {oid}{eol}"));
            match self.branch.clone() {
                None => self.raw(&format!("# branch.head (unknown){eol}")),
                Some(full) => {
                    let name = if full == "HEAD" {
                        self.raw(&format!("# branch.head (detached){eol}"));
                        None
                    } else {
                        let n = full.strip_prefix("refs/heads/").unwrap_or(&full).to_owned();
                        self.raw(&format!("# branch.head {n}{eol}"));
                        Some(n)
                    };
                    if let Some(t) = name.and_then(|n| self.tracking(&n)) {
                        self.raw(&format!("# branch.upstream {}{eol}", t.base));
                        if !t.gone {
                            match t.counts {
                                Some((a, b)) => self.raw(&format!("# branch.ab +{a} -{b}{eol}")),
                                None => self.raw(&format!("# branch.ab +? -?{eol}")),
                            }
                        }
                    }
                }
            }
        }
        if self.show_stash {
            let n = self.stash_count();
            if n > 0 {
                self.raw(&format!("# stash {n}{eol}"));
            }
        }
        let paths: Vec<String> = self.change.keys().cloned().collect();
        let mut unmerged = Vec::new();
        for p in paths {
            let d = self.change.get_mut(&p).expect("path");
            if d.stagemask != 0 {
                unmerged.push(p);
                continue;
            }
            if d.index_status == 0 {
                d.mode_head = d.mode_index;
                d.oid_head = d.oid_index;
            }
            if d.worktree_status == 0 {
                d.mode_worktree = d.mode_index;
            }
            let d = &self.change[&p];
            let sub = submodule_token(d);
            let key = format!(
                "{}{}",
                if d.index_status != 0 {
                    d.index_status as char
                } else {
                    '.'
                },
                if d.worktree_status != 0 {
                    d.worktree_status as char
                } else {
                    '.'
                }
            );
            let modes = format!(
                "{:06o} {:06o} {:06o} {} {}",
                d.mode_head, d.mode_index, d.mode_worktree, d.oid_head, d.oid_index
            );
            let (path, from) = if self.opts.null {
                (p.clone(), d.rename_source.clone())
            } else {
                (
                    self.quote(&p, false),
                    d.rename_source.as_ref().map(|s| self.quote(s, false)),
                )
            };
            let sep = if self.opts.null { '\0' } else { '\t' };
            let line = match from {
                Some(from) => format!(
                    "2 {key} {sub} {modes} {}{} {path}{sep}{from}{eol}",
                    d.rename_status as char, d.rename_score
                ),
                None => format!("1 {key} {sub} {modes} {path}{eol}"),
            };
            self.raw(&line);
        }
        for p in unmerged {
            let d = &self.change[&p];
            let sub = submodule_token(d);
            let key = unmerged_key(d.stagemask);
            let mut stages = [(0u32, Oid::ZERO_SHA1); 3];
            for e in self.index.iter() {
                if e.path == p.as_bytes() {
                    let s = ((e.flags >> 12) & 3) as usize;
                    if s > 0 {
                        stages[s - 1] = (e.mode, e.id);
                    }
                }
            }
            let path = if self.opts.null {
                p.clone()
            } else {
                self.quote(&p, false)
            };
            let line = format!(
                "u {key} {sub} {:06o} {:06o} {:06o} {:06o} {} {} {} {path}{eol}",
                stages[0].0,
                stages[1].0,
                stages[2].0,
                d.mode_worktree,
                stages[0].1,
                stages[1].1,
                stages[2].1
            );
            self.raw(&line);
        }
        let others: Vec<(char, String)> = self
            .untracked
            .iter()
            .map(|p| ('?', p.clone()))
            .chain(self.ignored.iter().map(|p| ('!', p.clone())))
            .collect();
        for (sign, p) in others {
            let path = if self.opts.null {
                p
            } else {
                self.quote(&p, false)
            };
            self.raw(&format!("{sign} {path}{eol}"));
        }
    }
}

/// A file's span hashes and size, for git's similarity score.
struct Sig {
    hashes: std::collections::HashMap<u32, u64>,
    size: u64,
}

fn sig_out(out: *mut *mut std::ffi::c_void, data: &[u8]) -> std::ffi::c_int {
    let sig = Box::new(Sig {
        hashes: crate::git_repo::span_hashes(data),
        size: data.len() as u64,
    });
    // SAFETY: libgit2 hands us a valid out pointer.
    unsafe { *out = Box::into_raw(sig).cast() };
    0
}

extern "C" fn file_sig(
    out: *mut *mut std::ffi::c_void,
    _file: *const libgit2_sys::git_diff_file,
    path: *const std::ffi::c_char,
    _payload: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: libgit2 passes a NUL-terminated path.
    let path = unsafe { std::ffi::CStr::from_ptr(path) };
    let path = Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()));
    let data = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::read_link(path)
            .map(|t| t.as_os_str().as_bytes().to_vec())
            .unwrap_or_default(),
        _ => std::fs::read(path).unwrap_or_default(),
    };
    sig_out(out, &data)
}

extern "C" fn buffer_sig(
    out: *mut *mut std::ffi::c_void,
    _file: *const libgit2_sys::git_diff_file,
    buf: *const std::ffi::c_char,
    len: usize,
    _payload: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    let data = if buf.is_null() {
        &[][..]
    } else {
        // SAFETY: libgit2 passes `len` readable bytes.
        unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), len) }
    };
    sig_out(out, data)
}

extern "C" fn free_sig(sig: *mut std::ffi::c_void, _payload: *mut std::ffi::c_void) {
    if !sig.is_null() {
        // SAFETY: made by sig_out.
        drop(unsafe { Box::from_raw(sig.cast::<Sig>()) });
    }
}

extern "C" fn similarity_cb(
    score: *mut std::ffi::c_int,
    a: *mut std::ffi::c_void,
    b: *mut std::ffi::c_void,
    _payload: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: both were made by sig_out.
    let (a, b) = unsafe { (&*a.cast::<Sig>(), &*b.cast::<Sig>()) };
    let max = a.size.max(b.size);
    let copied: u64 = a
        .hashes
        .iter()
        .map(|(h, n)| b.hashes.get(h).map_or(0, |m| (*n).min(*m)))
        .sum();
    let pct = if max == 0 || b.size == 0 {
        0
    } else {
        copied * 60000 / max * 100 / 60000
    };
    // SAFETY: libgit2 passes a valid out pointer.
    unsafe { *score = pct as std::ffi::c_int };
    0
}

struct Metric(libgit2_sys::git_diff_similarity_metric);
// SAFETY: the table only holds function pointers and a null payload.
unsafe impl Sync for Metric {}

static GIT_METRIC: Metric = Metric(libgit2_sys::git_diff_similarity_metric {
    file_signature: Some(file_sig),
    buffer_signature: Some(buffer_sig),
    free_signature: Some(free_sig),
    similarity: Some(similarity_cb),
    payload: std::ptr::null_mut(),
});

/// Score rename and copy candidates the way git does (diffcore-delta's
/// span hashing), so libgit2 pairs the files git would.
pub(crate) fn git_metric(f: &mut DiffFindOptions) {
    // SAFETY: the options struct is owned by `f`; libgit2 only reads the
    // static metric table.
    unsafe {
        let raw = f.raw().cast_mut();
        (*raw).metric = (&raw const GIT_METRIC.0).cast_mut();
    }
}

fn submodule_token(d: &Change) -> String {
    if [d.mode_head, d.mode_index, d.mode_worktree].contains(&0o160000) {
        format!(
            "S{}{}{}",
            if d.new_submodule_commits { 'C' } else { '.' },
            if d.dirty_submodule & DIRTY_MODIFIED != 0 {
                'M'
            } else {
                '.'
            },
            if d.dirty_submodule & DIRTY_UNTRACKED != 0 {
                'U'
            } else {
                '.'
            }
        )
    } else {
        "N...".to_owned()
    }
}

fn unmerged_key(mask: u8) -> &'static str {
    match mask {
        1 => "DD",
        2 => "AU",
        3 => "UD",
        4 => "UA",
        5 => "DU",
        6 => "AA",
        _ => "UU",
    }
}

fn delta_path(delta: &git2::DiffDelta) -> String {
    delta
        .new_file()
        .path()
        .or_else(|| delta.old_file().path())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn delta_char(d: Delta) -> Option<u8> {
    Some(match d {
        Delta::Added => b'A',
        Delta::Deleted => b'D',
        Delta::Modified => b'M',
        Delta::Renamed => b'R',
        Delta::Copied => b'C',
        Delta::Typechange => b'T',
        Delta::Conflicted => b'U',
        _ => return None,
    })
}

/// A submodule's dirt: tracked changes and, when `untracked`, new files.
fn submodule_dirty(sub: &Repository, untracked: bool) -> u8 {
    let mut o = git2::StatusOptions::new();
    o.include_untracked(untracked)
        .include_ignored(false)
        .exclude_submodules(false);
    let Ok(st) = sub.statuses(Some(&mut o)) else {
        return 0;
    };
    let mut dirty = 0;
    for e in st.iter() {
        let s = e.status();
        if s.is_wt_new() {
            dirty |= DIRTY_UNTRACKED;
        } else if !s.is_ignored() {
            dirty |= DIRTY_MODIFIED;
        }
    }
    dirty
}

/// git's parse_rename_score: `50`, `50%` or `0.5` as a percentage.
pub(crate) fn parse_rename_score(arg: &str) -> u16 {
    if let Some(p) = arg.strip_suffix('%') {
        return p.parse::<f64>().map_or(50, |v| v.clamp(0.0, 100.0) as u16);
    }
    if arg.contains('.') {
        return arg
            .parse::<f64>()
            .map_or(50, |v| (v.min(1.0) * 100.0) as u16);
    }
    // Digits are a fraction: "5" is 50%, "05" 5%, "75" 75%.
    let digits: String = arg.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return 50;
    }
    let v: f64 = format!("0.{digits}").parse().unwrap_or(0.5);
    (v * 100.0) as u16
}

/// git's column config: a layout named without always, never or auto
/// turns columns on.
fn column_config(colopts: &mut u32, v: &str) {
    let _ = crate::text::column_mode(colopts, v);
    let words: Vec<&str> = v.split([' ', ',']).collect();
    let layout = words.iter().any(|w| ["plain", "column", "row"].contains(w));
    let enable = words
        .iter()
        .any(|w| ["always", "never", "auto"].contains(w));
    if layout && !enable {
        let _ = crate::text::column_mode(colopts, "always");
    }
}

/// git's relative_path(): `path` as seen from the folder `prefix`.
fn relative_path(path: &str, prefix: &str) -> String {
    let base: Vec<&str> = prefix.split('/').filter(|s| !s.is_empty()).collect();
    let dir = path.ends_with('/');
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let common = base.iter().zip(&parts).take_while(|(a, b)| a == b).count();
    let mut rel = format!(
        "{}{}",
        "../".repeat(base.len() - common),
        parts[common..].join("/")
    );
    if dir && common < parts.len() {
        rel.push('/');
    }
    if rel.is_empty() { "./".to_owned() } else { rel }
}

/// git's quote_c_style for a path, with `quote_sp` adding quotes around a
/// path with spaces (short formats).
pub(crate) fn quote_c(s: &str, quote_sp: bool, quote_high: bool) -> String {
    let must =
        |b: u8| b < 0x20 || b == b'"' || b == b'\\' || b == 0x7f || (quote_high && b >= 0x80);
    let bytes = s.as_bytes();
    if !bytes.iter().any(|&b| must(b)) {
        return if quote_sp && s.contains(' ') {
            format!("\"{s}\"")
        } else {
            s.to_owned()
        };
    }
    let mut out = vec![b'"'];
    for &b in bytes {
        if !must(b) {
            out.push(b);
            continue;
        }
        out.push(b'\\');
        match b {
            0x07 => out.push(b'a'),
            0x08 => out.push(b'b'),
            b'\t' => out.push(b't'),
            b'\n' => out.push(b'n'),
            0x0b => out.push(b'v'),
            0x0c => out.push(b'f'),
            b'\r' => out.push(b'r'),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            _ => out.extend_from_slice(format!("{b:03o}").as_bytes()),
        }
    }
    out.push(b'"');
    String::from_utf8_lossy(&out).into_owned()
}

fn shorten_ref(name: &str) -> String {
    ["refs/heads/", "refs/remotes/", "refs/tags/", "refs/"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .unwrap_or(name)
        .to_owned()
}

/// The scissors line and its explanation, `#` commented.
pub fn cut_line() -> String {
    "# ------------------------ >8 ------------------------\n\
     # Do not modify or remove the line above.\n\
     # Everything below it will be ignored.\n"
        .to_owned()
}

/// The plain patch `git diff-files -p` (no `rev`) or `git diff-index [-R]
/// [--cached] <rev> -p` prints, with `--ignore-submodules=dirty`, as the
/// `-p` modes read it.
pub(crate) fn patch_diff(
    repo: &Repository,
    index: &Index,
    rev: Option<&str>,
    cached: bool,
    reverse: bool,
    context: Option<u32>,
    paths: &[String],
) -> Result<Vec<u8>, GitError> {
    let context = context.unwrap_or(3);
    crate::pathspec::Pathspec::new(paths)?;
    let options = |o: &mut DiffOptions| {
        o.reverse(reverse)
            .indent_heuristic(true)
            .context_lines(context);
        let _ = crate::pathspec::limit_diff(o, paths);
        // SAFETY: the options struct is owned by `o`.
        unsafe {
            (*o.raw().cast_mut()).ignore_submodules = libgit2_sys::GIT_SUBMODULE_IGNORE_DIRTY;
        }
    };
    let mut unmerged = BTreeSet::new();
    let mut ita = Vec::new();
    let mut kept = Index::new()?;
    for e in index.iter() {
        let path = String::from_utf8_lossy(&e.path).into_owned();
        if (e.flags >> 12) & 3 != 0 {
            unmerged.insert(path);
        } else if e.flags_extended & (1 << 13) != 0 && rev.is_none() {
            ita.push(path);
        } else {
            kept.add(&e)?;
        }
    }
    let spec = (!paths.is_empty())
        .then(|| crate::pathspec::Pathspec::new(paths.iter()).ok())
        .flatten();
    let unmerged: Vec<String> = unmerged
        .into_iter()
        .filter(|p| spec.as_ref().is_none_or(|s| s.matches_path(Path::new(p))))
        .collect();
    let index = if ita.is_empty() { index } else { &kept };
    let mut o = DiffOptions::new();
    options(&mut o);
    let tree = match rev {
        Some(r) => repo
            .rev_single(r)
            .ok()
            .map(|o| o.peel_to_tree())
            .transpose()?,
        None => None,
    };
    let mut diffs = vec![match (rev, cached) {
        (None, _) => repo.diff_index_to_workdir(Some(index), Some(&mut o))?,
        (Some(_), true) => repo.diff_tree_to_index(tree.as_ref(), Some(index), Some(&mut o))?,
        (Some(_), false) => repo.diff_tree_to_workdir_with_index(tree.as_ref(), Some(&mut o))?,
    }];
    ita.retain(|p| spec.as_ref().is_none_or(|s| s.matches_path(Path::new(p))));
    if !ita.is_empty() {
        // Intent-to-add files diff as new files.
        let mut o = DiffOptions::new();
        o.reverse(reverse)
            .indent_heuristic(true)
            .context_lines(context)
            .include_untracked(true)
            .show_untracked_content(true)
            .disable_pathspec_match(true);
        for p in &ita {
            o.pathspec(p);
        }
        diffs.push(repo.diff_index_to_workdir(Some(index), Some(&mut o))?);
    }
    let refs: Vec<&Diff> = diffs.iter().collect();
    patch_text(repo, &refs, false, &unmerged)
}

/// libgit2 takes a working-tree file it did not hash (the old side of a
/// reversed diff) for a missing one: `index 0000000..`, `--- /dev/null`,
/// even `new mode 0`. The header git prints instead, with the file hashed.
fn unhashed_header(repo: &Repository, delta: &git2::DiffDelta, text: &str) -> Option<String> {
    let (one, two) = (delta.old_file(), delta.new_file());
    let unhashed = |f: &git2::DiffFile| u32::from(f.mode()) != 0 && f.id().is_zero();
    if !unhashed(&one) && !unhashed(&two) {
        return None;
    }
    let workdir = repo.workdir()?;
    let hash = |f: &git2::DiffFile| -> (Oid, u64) {
        if !unhashed(f) {
            return (f.id(), f.size());
        }
        let path = workdir.join(f.path().unwrap_or(Path::new("")));
        let data = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => std::fs::read_link(&path)
                .map(|t| t.to_string_lossy().into_owned().into_bytes())
                .unwrap_or_default(),
            _ => std::fs::read(&path).unwrap_or_default(),
        };
        let size = data.len() as u64;
        (
            Oid::hash_object(git2::ObjectType::Blob, &data).unwrap_or(Oid::ZERO_SHA1),
            size,
        )
    };
    let first = text.lines().next()?;
    let names = first.strip_prefix("diff --git ")?;
    let (x, y) = names.split_once(' ')?;
    let (old_mode, new_mode) = (u32::from(one.mode()), u32::from(two.mode()));
    let ((old_id, old_size), (new_id, new_size)) = (hash(&one), hash(&two));
    let short = |id: Oid| id.to_string()[..7].to_owned();
    let mut out = format!("{first}\n");
    if old_mode == 0 {
        out.push_str(&format!("new file mode {new_mode:o}\n"));
    } else if new_mode == 0 {
        out.push_str(&format!("deleted file mode {old_mode:o}\n"));
    } else if old_mode != new_mode {
        out.push_str(&format!("old mode {old_mode:o}\nnew mode {new_mode:o}\n"));
    }
    if old_id != new_id {
        out.push_str(&format!("index {}..{}", short(old_id), short(new_id)));
        if old_mode != 0 && old_mode == new_mode {
            out.push_str(&format!(" {old_mode:o}"));
        }
        out.push('\n');
    }
    let content = if text.contains("\n--- ") {
        true
    } else {
        !delta.flags().is_binary() && old_size.max(new_size) > 0 && old_id != new_id
    };
    if content && !delta.flags().is_binary() {
        let side = |mode: u32, name: &str| {
            if mode == 0 {
                "/dev/null".to_owned()
            } else {
                name.to_owned()
            }
        };
        out.push_str(&format!(
            "--- {}\n+++ {}\n",
            side(old_mode, x),
            side(new_mode, y)
        ));
    }
    Some(out)
}

/// A diff as git prints it, colored like git's default diff colors, with
/// git's `* Unmerged path` line for each of `unmerged` in path order.
fn patch_text(
    repo: &Repository,
    diffs: &[&Diff],
    color: bool,
    unmerged: &[String],
) -> Result<Vec<u8>, GitError> {
    let mut chunks: Vec<(String, Vec<u8>)> = unmerged
        .iter()
        .map(|p| (p.clone(), format!("* Unmerged path {p}\n").into_bytes()))
        .collect();
    let paint = |out: &mut Vec<u8>, c: &str, text: &[u8]| {
        if color && !c.is_empty() {
            out.extend_from_slice(c.as_bytes());
            out.extend_from_slice(text);
            out.extend_from_slice(RESET.as_bytes());
        } else {
            out.extend_from_slice(text);
        }
    };
    let mut skip = false;
    let mut current: Option<usize> = None;
    for diff in diffs {
        diff.print(DiffFormat::Patch, |delta, _hunk, line| {
            let content = line.content();
            if line.origin() == 'F' {
                let path = delta_path(&delta);
                skip = unmerged.contains(&path);
                if !skip {
                    chunks.push((path, Vec::new()));
                    current = Some(chunks.len() - 1);
                }
            }
            let Some(i) = current.filter(|_| !skip) else {
                return true;
            };
            let out = &mut chunks[i].1;
            match line.origin() {
                'F' => {
                    let mut text = String::from_utf8_lossy(content).into_owned();
                    // libgit2 leaves out a copy's header lines.
                    if delta.status() == Delta::Copied
                        && !text.contains("\ncopy from ")
                        && let Some(i) = text.find('\n')
                    {
                        let from = delta.old_file().path().unwrap_or(Path::new(""));
                        let to = delta.new_file().path().unwrap_or(Path::new(""));
                        let score = crate::git_repo::similarity(repo, &delta);
                        text.insert_str(
                            i + 1,
                            &format!(
                                "similarity index {score}%\ncopy from {}\ncopy to {}\n",
                                from.display(),
                                to.display()
                            ),
                        );
                    }
                    if let Some(fixed) = unhashed_header(repo, &delta, &text) {
                        text = fixed;
                    }
                    // A dirty submodule's header has no `index` line in git.
                    let same = |l: &str| {
                        l.strip_prefix("index ")
                            .and_then(|r| r.split(' ').next())
                            .and_then(|r| r.split_once(".."))
                            .is_some_and(|(a, b)| a == b)
                    };
                    for l in text.lines().filter(|l| !same(l)) {
                        paint(out, "\x1b[1m", l.as_bytes());
                        out.push(b'\n');
                    }
                }
                'H' => {
                    let text = String::from_utf8_lossy(content);
                    let text = text.trim_end_matches('\n');
                    match text.get(2..).and_then(|t| t.find("@@")).map(|i| i + 4) {
                        Some(end) if end <= text.len() => {
                            paint(out, "\x1b[36m", &text.as_bytes()[..end]);
                            out.extend_from_slice(&text.as_bytes()[end..]);
                            if color && end < text.len() {
                                out.extend_from_slice(RESET.as_bytes());
                            }
                        }
                        _ => paint(out, "\x1b[36m", text.as_bytes()),
                    }
                    out.push(b'\n');
                }
                o @ ('+' | '-' | ' ') => {
                    let body = content.strip_suffix(b"\n").unwrap_or(content);
                    let mut text = vec![o as u8];
                    text.extend_from_slice(body);
                    match o {
                        _ if !color => out.extend_from_slice(&text),
                        '+' => {
                            // git colors the sign apart and marks trailing blanks.
                            let kept = body
                                .iter()
                                .rposition(|b| !b" \t".contains(b))
                                .map_or(0, |i| i + 1);
                            paint(out, "\x1b[32m", b"+");
                            if kept > 0 {
                                paint(out, "\x1b[32m", &body[..kept]);
                            }
                            if kept < body.len() {
                                paint(out, "\x1b[41m", &body[kept..]);
                            }
                        }
                        '-' => paint(out, "\x1b[31m", &text),
                        _ => {
                            out.extend_from_slice(&text);
                            out.extend_from_slice(RESET.as_bytes());
                        }
                    }
                    if content.ends_with(b"\n") {
                        out.push(b'\n');
                    }
                }
                _ => out.extend_from_slice(content),
            }
            true
        })?;
    }
    chunks.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(chunks.into_iter().flat_map(|(_, t)| t).collect())
}

/// The untracked-file walk, as git's dir.c classifies paths.
struct Walk<'a> {
    repo: &'a Repository,
    workdir: &'a Path,
    files: &'a HashSet<String>,
    dirs: &'a HashSet<String>,
    untracked: Untracked,
    ignored: Ignored,
    spec: Option<&'a crate::pathspec::Pathspec>,
}

enum Kind {
    File,
    Dir,
    Repo,
}

impl Walk<'_> {
    fn children(&self, dir: &str) -> Vec<(String, Kind)> {
        let Ok(read) = std::fs::read_dir(self.workdir.join(dir)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in read.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let path = format!("{dir}{name}");
            let Ok(ft) = e.file_type() else { continue };
            let kind = if ft.is_dir() {
                if e.path().join(".git").exists() {
                    Kind::Repo
                } else {
                    Kind::Dir
                }
            } else {
                Kind::File
            };
            out.push((path, kind));
        }
        out
    }

    fn is_ignored(&self, path: &str) -> bool {
        self.repo.is_path_ignored(path).unwrap_or(false)
    }

    fn matches(&self, path: &str) -> bool {
        self.spec.is_none_or(|s| s.matches_path(Path::new(path)))
    }

    /// Whether `dir` itself is matched, so it may be shown collapsed.
    fn dir_matches(&self, dir: &str) -> bool {
        self.matches(dir) || self.matches(dir.trim_end_matches('/'))
    }

    /// A folder that holds tracked files: everything in it is looked at.
    fn tracked_dir(&self, dir: &str, unt: &mut Vec<String>, ign: &mut Vec<String>) {
        for (path, kind) in self.children(dir) {
            match kind {
                Kind::File => {
                    if self.files.contains(&path) || !self.matches(&path) {
                        continue;
                    }
                    if self.is_ignored(&path) {
                        ign.push(path);
                    } else {
                        unt.push(path);
                    }
                }
                Kind::Dir | Kind::Repo if self.files.contains(&path) => {}
                Kind::Dir if self.dirs.contains(&path) => {
                    self.tracked_dir(&format!("{path}/"), unt, ign);
                }
                kind => {
                    let d = format!("{path}/");
                    self.untracked_dir(&d, matches!(kind, Kind::Repo), unt, ign);
                }
            }
        }
    }

    /// An untracked folder `dir` (`a/b/`) seen from a tracked parent.
    fn untracked_dir(&self, dir: &str, repo: bool, unt: &mut Vec<String>, ign: &mut Vec<String>) {
        let ignored = self.is_ignored(dir);
        if repo {
            if ignored {
                if self.dir_matches(dir) {
                    ign.push(dir.to_owned());
                }
            } else if self.dir_matches(dir) {
                unt.push(dir.to_owned());
            }
            return;
        }
        if self.spec.is_some() && !self.dir_matches(dir) {
            // Only paths below are asked about: look at each.
            if !ignored {
                self.tracked_dir(dir, unt, ign);
            } else if self.ignored != Ignored::No {
                self.all_files(dir, ign);
            }
            return;
        }
        if ignored {
            match (self.ignored, self.untracked) {
                (Ignored::No, _) => {}
                (Ignored::Traditional, Untracked::All) => self.all_files(dir, ign),
                _ => {
                    if self.has_any(dir) || self.ignored == Ignored::Matching {
                        ign.push(dir.to_owned());
                    }
                }
            }
            return;
        }
        let (mut u, mut i) = (Vec::new(), Vec::new());
        let any = self.scan(dir, &mut u, &mut i);
        if self.untracked == Untracked::All {
            unt.extend(u);
            ign.extend(i);
            return;
        }
        if any {
            unt.push(dir.to_owned());
        }
        if self.ignored == Ignored::Traditional && !any && !i.is_empty() {
            ign.push(dir.to_owned());
        } else {
            ign.extend(i);
        }
    }

    /// Classify what is inside an untracked, unignored folder. Returns
    /// whether it holds any untracked file; `unt` gets them one by one.
    fn scan(&self, dir: &str, unt: &mut Vec<String>, ign: &mut Vec<String>) -> bool {
        let mut any = false;
        for (path, kind) in self.children(dir) {
            match kind {
                Kind::File => {
                    if !self.matches(&path) {
                        continue;
                    }
                    if self.is_ignored(&path) {
                        ign.push(path);
                    } else {
                        any = true;
                        unt.push(path);
                    }
                }
                Kind::Repo => {
                    let d = format!("{path}/");
                    if self.is_ignored(&d) {
                        ign.push(d);
                    } else {
                        any = true;
                        unt.push(d);
                    }
                }
                Kind::Dir => {
                    let d = format!("{path}/");
                    if self.is_ignored(&d) {
                        match (self.ignored, self.untracked) {
                            (Ignored::No, _) => {}
                            (Ignored::Traditional, Untracked::All) => self.all_files(&d, ign),
                            _ => {
                                if self.has_any(&d) || self.ignored == Ignored::Matching {
                                    ign.push(d);
                                }
                            }
                        }
                        continue;
                    }
                    let (mut u, mut i) = (Vec::new(), Vec::new());
                    let sub = self.scan(&d, &mut u, &mut i);
                    any |= sub;
                    unt.extend(u);
                    if self.untracked == Untracked::Normal
                        && self.ignored == Ignored::Traditional
                        && !sub
                        && !i.is_empty()
                    {
                        ign.push(d);
                    } else {
                        ign.extend(i);
                    }
                }
            }
        }
        any
    }

    fn has_any(&self, dir: &str) -> bool {
        let mut v = Vec::new();
        self.all_files(dir, &mut v);
        !v.is_empty()
    }

    fn all_files(&self, dir: &str, out: &mut Vec<String>) {
        for (path, kind) in self.children(dir) {
            match kind {
                Kind::File => {
                    if self.matches(&path) {
                        out.push(path);
                    }
                }
                Kind::Repo => out.push(format!("{path}/")),
                Kind::Dir => self.all_files(&format!("{path}/"), out),
            }
        }
    }
}
