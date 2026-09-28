//! git's low-level diff and merge plumbing: `diff-tree`, `diff-index`,
//! `diff-files`, `merge-tree --write-tree` and `merge-file`, in git's formats.

use crate::rev::RevParse;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use git2::{Delta, Diff, DiffFindOptions, DiffOptions, ObjectType, Oid, Repository, Tree};

use crate::GitError;

/// What a diff plumbing command prints for each change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffMode {
    /// The sections [`DiffFmt`]'s `raw`, `stat`, `summary` and `patch` ask for.
    #[default]
    Raw,
    NameOnly,
    NameStatus,
    /// `-s`: only the commit header.
    Nothing,
}

#[derive(Debug, Clone, Default)]
pub struct DiffFmt {
    pub mode: DiffMode,
    pub z: bool,
    /// `-r`: list blobs inside changed subtrees (diff-tree).
    pub recursive: bool,
    /// `-t`: list the changed trees too while recursing (diff-tree).
    pub trees: bool,
    /// Repo-root pathspecs.
    pub paths: Vec<String>,
    pub raw: bool,
    pub stat: bool,
    pub summary: bool,
    pub patch: bool,
    /// 0 off, 1 renames (`-M`), 2 copies (`-C`), 3 copies from unmodified
    /// files too (`--find-copies-harder`).
    pub detect: u8,
    /// The minimum rename or copy score, as `-M<n>` gave it.
    pub score: Option<String>,
    /// `--abbrev[=<n>]`: raw ids this long (0: the default length).
    pub abbrev: Option<usize>,
    /// `--stat`'s width in columns.
    pub stat_width: usize,
}

/// One change as git's raw format lists it.
struct Change {
    old_mode: u32,
    new_mode: u32,
    old_id: Oid,
    new_id: Oid,
    status: char,
    /// A rename's or copy's source path and score.
    source: Option<(String, u16)>,
    path: String,
}

impl DiffFmt {
    fn ends(&self) -> (char, char) {
        if self.z { ('\0', '\0') } else { ('\t', '\n') }
    }

    fn id(&self, repo: &Repository, id: Oid) -> Result<String, GitError> {
        match self.abbrev {
            Some(n) => crate::plumbing::abbrev(repo, &id.to_string(), n),
            None => Ok(id.to_string()),
        }
    }

    fn render(&self, repo: &Repository, changes: &[Change]) -> Result<String, GitError> {
        let (sep, end) = self.ends();
        let mut out = String::new();
        for c in changes {
            let (status, names) = match &c.source {
                Some((from, score)) => (
                    format!("{}{score:03}", c.status),
                    format!("{from}{sep}{}", c.path),
                ),
                None => (c.status.to_string(), c.path.clone()),
            };
            let _ = match self.mode {
                DiffMode::NameOnly => write!(out, "{}{end}", c.path),
                DiffMode::NameStatus => write!(out, "{status}{sep}{names}{end}"),
                _ => write!(
                    out,
                    ":{:06o} {:06o} {} {} {status}{sep}{names}{end}",
                    c.old_mode,
                    c.new_mode,
                    self.id(repo, c.old_id)?,
                    self.id(repo, c.new_id)?
                ),
            };
        }
        Ok(out)
    }

    /// Whether the output needs a libgit2 diff: for renames, a stat, a
    /// summary or a patch.
    fn wants_diff(&self) -> bool {
        self.detect > 0 || (self.mode == DiffMode::Raw && (self.stat || self.summary || self.patch))
    }

    /// The raw listing, stat, summary and patch, in git's order; `diff` has
    /// its renames found.
    fn sections(
        &self,
        repo: &Repository,
        changes: &[Change],
        diff: Option<&Diff>,
    ) -> Result<String, GitError> {
        match self.mode {
            DiffMode::Nothing => return Ok(String::new()),
            DiffMode::NameOnly | DiffMode::NameStatus => return self.render(repo, changes),
            DiffMode::Raw => {}
        }
        let mut out = if self.raw {
            self.render(repo, changes)?
        } else {
            String::new()
        };
        let Some(diff) = diff else {
            return Ok(out);
        };
        if self.stat && !changes.is_empty() {
            out.push_str(&crate::format_patch::diffstat(diff, self.stat_width)?);
        }
        if self.summary {
            out.push_str(&crate::format_patch::summary(diff)?);
        }
        if self.patch {
            let patch = copy_headers(repo, diff, patch_of(diff)?);
            if !out.is_empty() && !patch.is_empty() {
                out.push(self.ends().1);
            }
            out.push_str(&patch);
        }
        Ok(out)
    }

    fn wants(&self, path: &str) -> bool {
        self.paths.is_empty() || crate::pathspec_matches(&self.paths, path)
    }

    fn options(&self) -> DiffOptions {
        let mut o = DiffOptions::new();
        o.include_typechange(true)
            .indent_heuristic(true)
            .include_unmodified(self.detect >= 3);
        if let Some(n) = self.abbrev.filter(|n| *n > 0) {
            o.id_abbrev(n.clamp(4, 40) as u16);
        }
        let _ = crate::pathspec::limit_diff(&mut o, &self.paths);
        o
    }

    /// git's rename and copy detection for `-M`, `-C` and
    /// `--find-copies-harder`.
    fn find(&self) -> Option<DiffFindOptions> {
        if self.detect == 0 {
            return None;
        }
        let mut f = DiffFindOptions::new();
        f.renames(true)
            .copies(self.detect >= 2)
            .copies_from_unmodified(self.detect >= 3)
            .remove_unmodified(self.detect >= 3);
        if let Some(s) = &self.score {
            let s = crate::wt_status::parse_rename_score(s);
            f.rename_threshold(s).copy_threshold(s);
        }
        crate::wt_status::git_metric(&mut f);
        Some(f)
    }

    /// Find `diff`'s renames and copies and fold them into `changes`: a pair
    /// takes its destination's place, and a rename drops its source's
    /// deletion.
    fn fold_renames(
        &self,
        repo: &Repository,
        diff: &mut Diff,
        changes: &mut Vec<Change>,
    ) -> Result<(), GitError> {
        let Some(mut f) = self.find() else {
            return Ok(());
        };
        diff.find_similar(Some(&mut f))?;
        let name = |f: git2::DiffFile| {
            String::from_utf8_lossy(f.path_bytes().unwrap_or_default()).into_owned()
        };
        for d in diff.deltas() {
            let status = match d.status() {
                Delta::Renamed => 'R',
                Delta::Copied => 'C',
                _ => continue,
            };
            let (old, new) = (name(d.old_file()), name(d.new_file()));
            let Some(c) = changes
                .iter_mut()
                .find(|c| c.path == new && c.status == 'A')
            else {
                continue;
            };
            c.status = status;
            c.old_mode = u32::from(d.old_file().mode());
            c.old_id = d.old_file().id();
            c.source = Some((old.clone(), crate::git_repo::similarity(repo, &d)));
            if status == 'R' {
                changes.retain(|c| !(c.path == old && c.status == 'D'));
            }
        }
        Ok(())
    }
}

fn open(git_dir: &Path) -> Result<Repository, GitError> {
    Ok(Repository::open(git_dir)?)
}

fn patch_of(diff: &Diff) -> Result<String, GitError> {
    crate::format_patch::patch_text(diff)
}

/// libgit2 leaves a copy's similarity and source lines out of its patch;
/// add them where git prints them, after any mode lines.
fn copy_headers(repo: &Repository, diff: &Diff, mut patch: String) -> String {
    let name = |f: git2::DiffFile| {
        String::from_utf8_lossy(f.path_bytes().unwrap_or_default()).into_owned()
    };
    for d in diff.deltas().filter(|d| d.status() == Delta::Copied) {
        let (old, new) = (name(d.old_file()), name(d.new_file()));
        let head = format!("diff --git a/{old} b/{new}\n");
        let Some(at) = patch.find(&head) else {
            continue;
        };
        let mut end = at + head.len();
        while patch[end..].starts_with("old mode ") || patch[end..].starts_with("new mode ") {
            end += patch[end..].find('\n').map_or(0, |i| i + 1);
        }
        if !patch[end..].starts_with("similarity index ") {
            let score = crate::git_repo::similarity(repo, &d);
            patch.insert_str(
                end,
                &format!("similarity index {score}%\ncopy from {old}\ncopy to {new}\n"),
            );
        }
    }
    patch
}

const TREE: u32 = 0o040000;

/// Tree entries in git's tree order, keyed by name with a `/` after trees.
fn entries(tree: Option<&Tree>) -> BTreeMap<Vec<u8>, (u32, Oid)> {
    tree.map(|t| {
        t.iter()
            .map(|e| {
                let mode = e.filemode() as u32;
                let mut key = e.name_bytes().to_vec();
                if mode == TREE {
                    key.push(b'/');
                }
                (key, (mode, e.id()))
            })
            .collect()
    })
    .unwrap_or_default()
}

/// git's diff-tree walk: changed entries of `a` and `b` under `base`, in
/// tree order, recursing as -r, -t and the pathspecs ask.
fn walk_trees(
    repo: &Repository,
    a: Option<&Tree>,
    b: Option<&Tree>,
    base: &str,
    fmt: &DiffFmt,
    out: &mut Vec<Change>,
) -> Result<(), GitError> {
    let (ea, eb) = (entries(a), entries(b));
    let mut keys: Vec<&Vec<u8>> = ea.keys().chain(eb.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let (old, new) = (ea.get(key).copied(), eb.get(key).copied());
        if old == new {
            continue;
        }
        let name = String::from_utf8_lossy(key.strip_suffix(b"/").unwrap_or(key));
        let path = format!("{base}{name}");
        let is_tree = key.ends_with(b"/");
        let zero = (0, Oid::ZERO_SHA1);
        let (om, oi) = old.unwrap_or(zero);
        let (nm, ni) = new.unwrap_or(zero);
        let status = match (old, new) {
            (None, _) => 'A',
            (_, None) => 'D',
            _ if (om & 0o170000) != (nm & 0o170000) => 'T',
            _ => 'M',
        };
        let change = Change {
            old_mode: om,
            new_mode: nm,
            old_id: oi,
            new_id: ni,
            status,
            source: None,
            path: path.clone(),
        };
        if !is_tree {
            if fmt.wants(&path) {
                out.push(change);
            }
            continue;
        }
        let covered = fmt.paths.is_empty()
            || fmt
                .paths
                .iter()
                .any(|s| s == "." || *s == path || path.starts_with(&format!("{s}/")))
            || fmt.wants(&path);
        let inside = fmt.paths.iter().any(|s| s.starts_with(&format!("{path}/")));
        if !covered && !inside {
            continue;
        }
        if !fmt.recursive {
            out.push(change);
            continue;
        }
        if fmt.trees {
            out.push(change);
        }
        let sub = |id: Option<(u32, Oid)>| id.map(|(_, id)| repo.find_tree(id)).transpose();
        walk_trees(
            repo,
            sub(old)?.as_ref(),
            sub(new)?.as_ref(),
            &format!("{path}/"),
            fmt,
            out,
        )?;
    }
    Ok(())
}

/// Options of `git diff-tree`.
#[derive(Debug, Clone)]
pub struct DiffTreeOpts {
    pub fmt: DiffFmt,
    /// One commit, or two tree-ishes.
    pub revs: Vec<String>,
    /// Show a root commit's files as added.
    pub root: bool,
    pub no_commit_id: bool,
    /// A merge's combined diff: `-c`, or `--cc` when true.
    pub combined: Option<bool>,
}

/// What diff-tree prints for two trees, and whether they differ.
fn tree_pair(
    repo: &Repository,
    a: Option<&Tree>,
    b: &Tree,
    fmt: &DiffFmt,
) -> Result<(String, bool), GitError> {
    let mut changes = Vec::new();
    walk_trees(repo, a, Some(b), "", fmt, &mut changes)?;
    let mut diff = None;
    if fmt.wants_diff() {
        let mut d = repo.diff_tree_to_tree(a, Some(b), Some(&mut fmt.options()))?;
        fmt.fold_renames(repo, &mut d, &mut changes)?;
        diff = Some(d);
    }
    Ok((
        fmt.sections(repo, &changes, diff.as_ref())?,
        !changes.is_empty(),
    ))
}

/// A merge against all its parents (`-c`, or `--cc` when `dense`); a stat
/// or summary is against the first parent, as in git.
fn combined(
    repo: &Repository,
    commit: &git2::Commit,
    dense: bool,
    fmt: &DiffFmt,
) -> Result<(String, bool), GitError> {
    let files = crate::combine::combined(repo, &commit.id().to_string(), &fmt.paths, dense)?;
    let (sep, end) = fmt.ends();
    let mut out = String::new();
    match fmt.mode {
        DiffMode::Nothing => {}
        DiffMode::NameOnly | DiffMode::NameStatus => {
            for f in &files {
                if fmt.mode == DiffMode::NameStatus {
                    out.extend(&f.status);
                    out.push(sep);
                }
                let _ = write!(out, "{}{end}", f.path);
            }
        }
        DiffMode::Raw => {
            if fmt.raw {
                for f in &files {
                    out.push_str("::");
                    for m in &f.modes {
                        let _ = write!(out, "{m:06o} ");
                    }
                    for id in &f.ids {
                        let _ = write!(out, "{} ", fmt.id(repo, *id)?);
                    }
                    out.extend(&f.status);
                    let _ = write!(out, "{sep}{}{end}", f.path);
                }
            }
            if fmt.stat || fmt.summary {
                let first = DiffFmt {
                    raw: false,
                    patch: false,
                    ..fmt.clone()
                };
                let parent = commit.parent(0)?.tree()?;
                out.push_str(&tree_pair(repo, Some(&parent), &commit.tree()?, &first)?.0);
            }
            if fmt.patch {
                if !out.is_empty() {
                    out.push(end);
                }
                for f in &files {
                    out.push_str(&f.patch);
                }
            }
        }
    }
    Ok((out, !files.is_empty()))
}

/// A commit against its first parent (or `parent`), with its id as a header;
/// merges and (without --root) root commits print nothing, as in git.
fn commit_diff(
    repo: &Repository,
    commit: &git2::Commit,
    parent: Option<Oid>,
    o: &DiffTreeOpts,
) -> Result<(String, bool), GitError> {
    let parent = match parent {
        Some(p) => Some(repo.find_commit(p)?),
        None if commit.parent_count() > 1 => None,
        None if commit.parent_count() == 1 => Some(commit.parent(0)?),
        None if !o.root => return Ok((String::new(), false)),
        None => None,
    };
    let (text, changed) = match (parent, o.combined) {
        (None, Some(dense)) if commit.parent_count() > 1 => combined(repo, commit, dense, &o.fmt)?,
        (None, None) if commit.parent_count() > 1 => return Ok((String::new(), false)),
        (parent, _) => {
            let from = parent.map(|p| p.tree()).transpose()?;
            tree_pair(repo, from.as_ref(), &commit.tree()?, &o.fmt)?
        }
    };
    if o.no_commit_id || !changed {
        return Ok((text, changed));
    }
    let end = if o.fmt.z { '\0' } else { '\n' };
    Ok((format!("{}{end}{text}", commit.id()), changed))
}

/// `git diff-tree`: the text and whether anything changed. `stdin` holds the
/// lines of `--stdin`.
pub fn diff_tree(
    git_dir: &Path,
    o: &DiffTreeOpts,
    stdin: Option<&str>,
) -> Result<(String, bool), GitError> {
    let repo = open(git_dir)?;
    let obj = |rev: &str| {
        repo.rev_single(rev)
            .map_err(|_| GitError::Other(format!("ambiguous argument '{rev}'")))
    };
    if let Some(input) = stdin {
        let mut out = String::new();
        let mut any = false;
        for line in input.split_inclusive('\n') {
            let ids: Option<Vec<Oid>> = line
                .split_whitespace()
                .map(|w| (w.len() == 40).then(|| Oid::from_str(w).ok()).flatten())
                .collect();
            let Some(ids) = ids.filter(|i| !i.is_empty()) else {
                out.push_str(line);
                continue;
            };
            let first = repo.find_object(ids[0], None)?;
            let (text, changed) = match first.kind() {
                Some(ObjectType::Commit) => {
                    commit_diff(&repo, &first.peel_to_commit()?, ids.get(1).copied(), o)?
                }
                Some(ObjectType::Tree) if ids.len() == 2 => {
                    let b = repo.find_tree(ids[1])?;
                    let (text, changed) =
                        tree_pair(&repo, Some(&first.peel_to_tree()?), &b, &o.fmt)?;
                    (format!("{} {}\n{text}", ids[0], ids[1]), changed)
                }
                _ => continue,
            };
            out.push_str(&text);
            any |= changed;
        }
        return Ok((out, any));
    }
    match o.revs.as_slice() {
        [one] => {
            let c = obj(one)?.peel_to_commit()?;
            commit_diff(&repo, &c, None, o)
        }
        [a, b] => tree_pair(
            &repo,
            Some(&obj(a)?.peel_to_tree()?),
            &obj(b)?.peel_to_tree()?,
            &o.fmt,
        ),
        _ => Err(GitError::Other(
            "usage: git diff-tree [<options>] <tree-ish> [<tree-ish>] [<path>...]".into(),
        )),
    }
}

/// Raw changes of a libgit2 diff; `worktree` zeroes the new id of a file
/// whose working copy differs from its index entry, as git leaves those
/// unhashed.
fn raw_changes(repo: &Repository, diff: &Diff, worktree: bool) -> Result<Vec<Change>, GitError> {
    let index = repo.index()?;
    let workdir = repo.workdir().map(Path::to_path_buf);
    let mut out = Vec::new();
    for d in diff.deltas() {
        let (old, new) = (d.old_file(), d.new_file());
        let path =
            String::from_utf8_lossy(new.path_bytes().or(old.path_bytes()).unwrap_or_default())
                .into_owned();
        let status = match d.status() {
            Delta::Added => 'A',
            Delta::Deleted => 'D',
            Delta::Typechange => 'T',
            Delta::Conflicted => 'U',
            Delta::Unmodified => continue,
            _ => 'M',
        };
        if status == 'U' {
            out.push(Change {
                old_mode: 0,
                new_mode: 0,
                old_id: Oid::ZERO_SHA1,
                new_id: Oid::ZERO_SHA1,
                status,
                source: None,
                path,
            });
            continue;
        }
        let mut new_id = new.id();
        if worktree && status != 'D' {
            let on_disk = workdir
                .as_ref()
                .and_then(|w| Oid::hash_file(ObjectType::Blob, w.join(&path)).ok());
            new_id = match index.get_path(Path::new(&path), 0) {
                Some(e) if Some(e.id) == on_disk => e.id,
                _ => Oid::ZERO_SHA1,
            };
        }
        out.push(Change {
            old_mode: u32::from(old.mode()),
            new_mode: if status == 'D' {
                0
            } else {
                u32::from(new.mode())
            },
            old_id: if status == 'A' {
                Oid::ZERO_SHA1
            } else {
                old.id()
            },
            new_id: if status == 'D' {
                Oid::ZERO_SHA1
            } else {
                new_id
            },
            status,
            source: None,
            path,
        });
    }
    Ok(out)
}

fn show(
    repo: &Repository,
    mut diff: Diff,
    fmt: &DiffFmt,
    worktree: bool,
) -> Result<(String, bool), GitError> {
    let mut changes = raw_changes(repo, &diff, worktree)?;
    fmt.fold_renames(repo, &mut diff, &mut changes)?;
    Ok((
        fmt.sections(repo, &changes, Some(&diff))?,
        !changes.is_empty(),
    ))
}

/// `git diff-index <tree-ish>`: the tree against the index (`cached`) or the
/// working tree.
pub fn diff_index(
    git_dir: &Path,
    rev: &str,
    cached: bool,
    fmt: &DiffFmt,
) -> Result<(String, bool), GitError> {
    let repo = open(git_dir)?;
    let tree = repo
        .rev_single(rev)
        .map_err(|_| GitError::Other(format!("ambiguous argument '{rev}'")))?
        .peel_to_tree()?;
    let mut o = fmt.options();
    let diff = if cached {
        repo.diff_tree_to_index(Some(&tree), None, Some(&mut o))?
    } else {
        repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut o))?
    };
    show(&repo, diff, fmt, !cached)
}

/// `git diff-files`: the index against the working tree.
pub fn diff_files(git_dir: &Path, fmt: &DiffFmt) -> Result<(String, bool), GitError> {
    let repo = open(git_dir)?;
    let diff = repo.diff_index_to_workdir(None, Some(&mut fmt.options()))?;
    show(&repo, diff, fmt, true)
}

/// How `merge-file` resolves conflicts and marks them.
#[derive(Debug, Clone, Default)]
pub struct MergeFileOpts {
    /// Labels of ours, the base and theirs.
    pub labels: [String; 3],
    /// `ours`, `theirs` or `union`.
    pub favor: Option<String>,
    /// `diff3` or `zdiff3`.
    pub style: Option<String>,
    pub marker_size: Option<u16>,
    /// git merge-file's XDL_MERGE_ZEALOUS_ALNUM rather than ort's ZEALOUS.
    pub alnum: bool,
    /// -X ignore-all-space, ignore-space-change and ignore-space-at-eol.
    pub ignore_space: [bool; 3],
    /// -X patience and diff-algorithm=minimal.
    pub patience: bool,
    pub minimal: bool,
}

/// A three-way merge of file contents, and how many conflicts it left.
pub fn merge_file(
    ours: &[u8],
    base: &[u8],
    theirs: &[u8],
    o: &MergeFileOpts,
) -> Result<(Vec<u8>, usize), GitError> {
    // Also keeps git2 from reading an empty result through a null pointer.
    if ours == theirs || base == theirs {
        return Ok((ours.to_vec(), 0));
    }
    if base == ours {
        return Ok((theirs.to_vec(), 0));
    }
    let mut opts = git2::MergeFileOptions::new();
    opts.our_label(o.labels[0].as_str())
        .ancestor_label(o.labels[1].as_str())
        .their_label(o.labels[2].as_str())
        .simplify_alnum(o.alnum)
        .ignore_whitespace(o.ignore_space[0])
        .ignore_whitespace_change(o.ignore_space[1])
        .ignore_whitespace_eol(o.ignore_space[2])
        .patience(o.patience)
        .minimal(o.minimal);
    match o.favor.as_deref() {
        Some("ours") => opts.favor(git2::FileFavor::Ours),
        Some("theirs") => opts.favor(git2::FileFavor::Theirs),
        Some("union") => opts.favor(git2::FileFavor::Union),
        _ => &mut opts,
    };
    match o.style.as_deref() {
        Some("diff3") => opts.style_diff3(true),
        Some("zdiff3") => opts.style_zdiff3(true),
        _ => &mut opts,
    };
    let size = o.marker_size.unwrap_or(7);
    opts.marker_size(size);
    let input = |data| {
        let mut i = git2::MergeFileInput::new();
        i.content(data);
        i
    };
    let result = git2::merge_file(&input(base), &input(ours), &input(theirs), Some(&mut opts))?;
    let content = result.content().to_vec();
    let marker = vec![b'<'; size as usize];
    let conflicts = content
        .split(|&b| b == b'\n')
        .filter(|l| l.starts_with(&marker) && matches!(l.get(size as usize), None | Some(b' ')))
        .count();
    Ok((content, conflicts))
}

/// Options of `git merge-tree --write-tree`.
#[derive(Debug, Clone, Default)]
pub struct MergeTreeOpts {
    pub branch1: String,
    pub branch2: String,
    pub merge_base: Option<String>,
    pub allow_unrelated: bool,
    pub name_only: bool,
    /// Informational messages: forced on or off, or (None) only on conflicts.
    pub messages: Option<bool>,
    pub z: bool,
    /// One merge of `--stdin`: the clean status first and a NUL last.
    pub batch: bool,
    /// -X strategy options.
    pub xopts: Vec<String>,
    /// The folder merge-tree runs in, which names are shown relative to.
    pub prefix: String,
}

/// git's parse_rename_score: `50`, `50%` or `0.5`, out of 60000.
fn rename_score(s: &str) -> Option<u64> {
    let (mut num, mut scale, mut dot) = (0u64, 1u64, false);
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if !dot && c == '.' {
            scale = 1;
            dot = true;
        } else if c == '%' {
            scale = if dot { scale * 100 } else { 100 };
            rest = &rest[1..];
            break;
        } else if let Some(d) = c.to_digit(10) {
            if scale < 100000 {
                scale *= 10;
                num = num * 10 + u64::from(d);
            }
        } else {
            break;
        }
        rest = &rest[1..];
    }
    rest.is_empty().then(|| {
        if num >= scale {
            60000
        } else {
            60000 * num / scale
        }
    })
}

/// merge-ort's options from the config and -X, as git reads them.
fn ort_opts(repo: &Repository, o: &MergeTreeOpts) -> Result<crate::ort::OrtOpts, GitError> {
    use crate::ort::DirRenames;
    let mut ort = crate::ort::OrtOpts {
        branch1: o.branch1.clone(),
        branch2: o.branch2.clone(),
        ..Default::default()
    };
    let cfg = repo.config()?;
    for key in ["diff.renames", "merge.renames"] {
        if let Ok(v) = cfg.get_string(key) {
            ort.no_renames = matches!(
                v.to_ascii_lowercase().as_str(),
                "false" | "no" | "off" | "0" | ""
            );
        }
    }
    if let Ok(v) = cfg.get_string("merge.directoryRenames") {
        ort.dir_renames = match v.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => DirRenames::True,
            "false" | "no" | "off" | "0" => DirRenames::None,
            _ => DirRenames::Conflict,
        };
    }
    if let Ok(v) = cfg.get_string("merge.conflictStyle")
        && matches!(v.as_str(), "diff3" | "zdiff3")
    {
        ort.file.style = Some(v);
    }
    for x in &o.xopts {
        let bad = || GitError::Other(format!("unknown strategy option: -X{x}"));
        match x.as_str() {
            "ours" | "theirs" => ort.favor = Some(x.clone()),
            "patience" => ort.file.patience = true,
            "histogram" | "no-renormalize" => {}
            "ignore-all-space" => ort.file.ignore_space[0] = true,
            "ignore-space-change" => ort.file.ignore_space[1] = true,
            "ignore-space-at-eol" => ort.file.ignore_space[2] = true,
            "no-renames" => ort.no_renames = true,
            "find-renames" => {
                ort.no_renames = false;
                ort.rename_score = 0;
            }
            _ => {
                if let Some(alg) = x.strip_prefix("diff-algorithm=") {
                    if !matches!(
                        alg,
                        "myers" | "default" | "minimal" | "patience" | "histogram"
                    ) {
                        return Err(bad());
                    }
                    ort.file.patience = alg == "patience";
                    ort.file.minimal = alg == "minimal";
                } else if let Some(n) = x
                    .strip_prefix("find-renames=")
                    .or_else(|| x.strip_prefix("rename-threshold="))
                {
                    ort.rename_score = rename_score(n).ok_or_else(bad)?;
                    ort.no_renames = false;
                } else {
                    return Err(bad());
                }
            }
        }
    }
    Ok(ort)
}

/// `git merge-tree --write-tree`: the merged tree (conflicts written with
/// markers), the conflicted files and git's messages, and whether the merge
/// was clean.
pub fn merge_tree(git_dir: &Path, o: &MergeTreeOpts) -> Result<(String, bool), GitError> {
    let repo = open(git_dir)?;
    let mut ort = ort_opts(&repo, o)?;
    let merged = match &o.merge_base {
        Some(base) => {
            let tree = |rev: &str| {
                repo.revparse_single(rev)
                    .and_then(|x| x.peel_to_tree())
                    .map_err(|_| GitError::Other(format!("could not parse as tree '{rev}'")))
            };
            ort.ancestor = Some(base.clone());
            crate::ort::merge_trees(
                &repo,
                &tree(base)?,
                &tree(&o.branch1)?,
                &tree(&o.branch2)?,
                &ort,
            )?
        }
        None => {
            let commit = |rev: &str| {
                repo.revparse_single(rev)
                    .and_then(|c| c.peel_to_commit())
                    .map_err(|_| {
                        GitError::Other(format!("merge-tree: {rev} - not something we can merge"))
                    })
            };
            let (c1, c2) = (commit(&o.branch1)?, commit(&o.branch2)?);
            let bases = crate::ort::merge_bases(&repo, c1.id(), c2.id())?;
            if bases.is_empty() && !o.allow_unrelated {
                return Err(GitError::Other(
                    "refusing to merge unrelated histories".into(),
                ));
            }
            crate::ort::merge_commits(&repo, bases, &c1, &c2, &ort)?
        }
    };
    let end = if o.z { '\0' } else { '\n' };
    let mut out = String::new();
    if o.batch {
        let _ = write!(out, "{}{end}", u8::from(merged.clean));
    }
    let _ = write!(out, "{}{end}", merged.tree);
    if !merged.clean {
        let mut last: Option<&str> = None;
        for s in &merged.conflicted {
            if o.name_only && last == Some(s.path.as_str()) {
                continue;
            }
            if !o.name_only {
                let _ = write!(out, "{:06o} {} {}\t", s.mode, s.id, s.stage);
            }
            let name = crate::clean::relative(&s.path, &o.prefix);
            let name = if o.z {
                name
            } else {
                crate::text::quote_path(&name)
            };
            let _ = write!(out, "{name}{end}");
            last = Some(&s.path);
        }
    }
    if o.messages.unwrap_or(!merged.clean) {
        out.push(end);
        for notes in merged.messages.values() {
            for m in notes {
                if o.z {
                    let _ = write!(out, "{}\0", m.paths.len());
                    for p in &m.paths {
                        let _ = write!(out, "{p}\0");
                    }
                    let _ = write!(out, "{}\0{}\n\0", m.kind, m.text);
                } else {
                    let _ = writeln!(out, "{}", m.text);
                }
            }
        }
    }
    if o.batch {
        out.push(end);
    }
    Ok((out, merged.clean))
}

/// `git merge-tree <base> <branch1> <branch2>`: the old trivial merge, with
/// git's report of each path and the merged result as a diff from ours.
/// Warnings (binary files) come back separately.
pub fn merge_tree_trivial(git_dir: &Path, revs: [&str; 3]) -> Result<(String, String), GitError> {
    let repo = open(git_dir)?;
    let mut trees: [Option<Tree>; 3] = [None, None, None];
    for (t, rev) in trees.iter_mut().zip(revs) {
        let obj = repo
            .revparse_single(rev)
            .map_err(|_| GitError::Other(format!("unknown rev {rev}")))?;
        *t = Some(
            obj.peel_to_tree()
                .map_err(|_| GitError::Other(format!("{rev} is not a tree")))?,
        );
    }
    let mut chains = Vec::new();
    trivial_walk(&repo, trees, "", &mut chains)?;
    let odb = repo.odb()?;
    // A missing object (a submodule commit) reads as empty, as in git.
    let read = |id: Oid| -> Result<Vec<u8>, GitError> {
        Ok(odb.read(id).map(|o| o.data().to_vec()).unwrap_or_default())
    };
    let (mut out, mut warn) = (String::new(), String::new());
    for chain in &chains {
        let head = chain[0].0;
        let explanation = match head {
            0 => "merged",
            3 => "added in remote",
            2 if chain.len() > 1 => "added in both",
            2 => "added in local",
            _ => match &chain[1..] {
                [] => "removed in both",
                [_, _] => "changed in both",
                [(3, ..)] => "removed in local",
                _ => "removed in remote",
            },
        };
        let _ = writeln!(out, "{explanation}");
        for (stage, mode, id, path) in chain {
            let desc = ["result", "base", "our", "their"][*stage as usize];
            let _ = writeln!(out, "  {desc:<6} {mode:o} {id} {path}");
        }
        let path = &chain[0].3;
        let blob = |stage: u8| chain.iter().find(|e| e.0 == stage).map(|e| e.2);
        let src = match blob(2) {
            Some(id) => read(id)?,
            None => Vec::new(),
        };
        let dst = if head == 0 {
            read(chain[0].2)?
        } else {
            let base = (head == 1).then(|| chain[0].2);
            match (blob(2), blob(3)) {
                (Some(a), Some(b)) => {
                    let base = match base {
                        Some(id) => read(id)?,
                        None => Vec::new(),
                    };
                    let (ours, theirs) = (read(a)?, read(b)?);
                    let binary = |d: &[u8]| d[..d.len().min(8000)].contains(&0);
                    if binary(&base) || binary(&ours) || binary(&theirs) {
                        let _ = writeln!(
                            warn,
                            "warning: Cannot merge binary files: {path} (.our vs. .their)"
                        );
                        ours
                    } else {
                        let opts = MergeFileOpts {
                            labels: [".our".into(), String::new(), ".their".into()],
                            ..Default::default()
                        };
                        merge_file(&ours, &base, &theirs, &opts)?.0
                    }
                }
                (a, b) if base.is_none() => match a.or(b) {
                    Some(id) => read(id)?,
                    None => Vec::new(),
                },
                _ => Vec::new(),
            }
        };
        out.push_str(&String::from_utf8_lossy(&hunks(&src, &dst)?));
    }
    Ok((out, warn))
}

type Chain = Vec<(u8, u32, Oid, String)>;

/// merge-tree's threeway_callback over the entries of three trees.
fn trivial_walk(
    repo: &Repository,
    t: [Option<Tree>; 3],
    base: &str,
    out: &mut Vec<Chain>,
) -> Result<(), GitError> {
    let mut names: BTreeMap<Vec<u8>, [Option<(u32, Oid)>; 3]> = BTreeMap::new();
    for (i, tree) in t.iter().enumerate() {
        for (mut name, e) in entries(tree.as_ref()) {
            if e.0 == TREE {
                name.pop();
            }
            names.entry(name).or_default()[i] = Some(e);
        }
    }
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort_by_cached_key(|(n, e)| {
        let mut k = n.clone();
        if e.iter().flatten().all(|(m, _)| *m == TREE) {
            k.push(b'/');
        }
        k
    });
    let is_dir = |e: Option<(u32, Oid)>| e.is_some_and(|(m, _)| m == TREE);
    for (name, n) in names {
        let path = format!("{base}{}", String::from_utf8_lossy(&name));
        let same = |a: usize, b: usize| n[a].is_some() && n[a] == n[b];
        let empty = |a: usize, b: usize| n[a].is_none() && n[b].is_none();
        if same(1, 2) || empty(1, 2) {
            continue;
        }
        if same(0, 1)
            && let Some((mode, id)) = n[2]
            && mode != TREE
        {
            let (om, oid) = n[1].unwrap();
            out.push(vec![(0, mode, id, path.clone()), (2, om, oid, path)]);
            continue;
        }
        if same(0, 2) || empty(0, 2) {
            continue;
        }
        if n.iter().any(|e| is_dir(*e)) {
            let mut sub: [Option<Tree>; 3] = [None, None, None];
            for (s, e) in sub.iter_mut().zip(n) {
                if let Some((TREE, id)) = e {
                    *s = Some(repo.find_tree(id)?);
                }
            }
            trivial_walk(repo, sub, &format!("{path}/"), out)?;
        }
        let chain: Chain = n
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                Some((mode, id)) if *mode != TREE => Some((i as u8 + 1, *mode, *id, path.clone())),
                _ => None,
            })
            .collect();
        if !chain.is_empty() {
            out.push(chain);
        }
    }
    Ok(())
}

/// xdiff's bare hunks from `a` to `b` with three lines of context, as
/// merge-tree's show_diff prints them.
fn hunks(a: &[u8], b: &[u8]) -> Result<Vec<u8>, GitError> {
    let mut out = Vec::new();
    if a == b {
        return Ok(out);
    }
    let mut opts = DiffOptions::new();
    opts.context_lines(3).force_text(true);
    let patch = git2::Patch::from_buffers(a, None, b, None, Some(&mut opts))?;
    let count = |start: u32, n: u32| {
        if n == 1 {
            format!("{start}")
        } else {
            format!("{start},{n}")
        }
    };
    for h in 0..patch.num_hunks() {
        let (hunk, lines) = patch.hunk(h)?;
        out.extend_from_slice(
            format!(
                "@@ -{} +{} @@\n",
                count(hunk.old_start(), hunk.old_lines()),
                count(hunk.new_start(), hunk.new_lines())
            )
            .as_bytes(),
        );
        for l in 0..lines {
            let line = patch.line_in_hunk(h, l)?;
            let origin = line.origin();
            if !matches!(origin, ' ' | '+' | '-') {
                continue;
            }
            out.push(origin as u8);
            // git prints each line with printf("%.*s"), which stops at a NUL.
            let text = line.content();
            let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
            out.extend_from_slice(&text[..end]);
            if !text.ends_with(b"\n") {
                out.extend_from_slice(b"\n\\ No newline at end of file\n");
            }
        }
    }
    Ok(out)
}
