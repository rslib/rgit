//! git's low-level diff and merge plumbing: `diff-tree`, `diff-index`,
//! `diff-files`, `merge-tree --write-tree` and `merge-file`, in git's formats.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use git2::{Delta, Diff, DiffOptions, ObjectType, Oid, Repository, Tree};

use crate::GitError;

/// What a diff plumbing command prints for each change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    Raw,
    NameOnly,
    NameStatus,
    Patch,
    /// `-s`: only the commit header.
    Nothing,
}

#[derive(Debug, Clone)]
pub struct DiffFmt {
    pub mode: DiffMode,
    pub z: bool,
    /// `-r`: list blobs inside changed subtrees (diff-tree).
    pub recursive: bool,
    /// `-t`: list the changed trees too while recursing (diff-tree).
    pub trees: bool,
    /// Repo-root pathspecs.
    pub paths: Vec<String>,
}

/// One change as git's raw format lists it.
struct Change {
    old_mode: u32,
    new_mode: u32,
    old_id: Oid,
    new_id: Oid,
    status: char,
    path: String,
}

impl DiffFmt {
    fn render(&self, changes: &[Change]) -> String {
        let (sep, end) = if self.z { ('\0', '\0') } else { ('\t', '\n') };
        let mut out = String::new();
        for c in changes {
            let _ = match self.mode {
                DiffMode::NameOnly => write!(out, "{}{end}", c.path),
                DiffMode::NameStatus => write!(out, "{}{sep}{}{end}", c.status, c.path),
                _ => write!(
                    out,
                    ":{:06o} {:06o} {} {} {}{sep}{}{end}",
                    c.old_mode, c.new_mode, c.old_id, c.new_id, c.status, c.path
                ),
            };
        }
        out
    }

    fn wants(&self, path: &str) -> bool {
        self.paths.is_empty() || crate::pathspec_matches(&self.paths, path)
    }

    fn options(&self) -> DiffOptions {
        let mut o = DiffOptions::new();
        o.include_typechange(true).indent_heuristic(true);
        for p in &self.paths {
            o.pathspec(p);
        }
        o
    }
}

fn open(git_dir: &Path) -> Result<Repository, GitError> {
    Ok(Repository::open(git_dir)?)
}

fn patch_of(diff: &Diff) -> Result<String, GitError> {
    crate::format_patch::patch_text(diff)
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
}

/// What diff-tree prints for two trees, and whether they differ.
fn tree_pair(
    repo: &Repository,
    a: Option<&Tree>,
    b: &Tree,
    fmt: &DiffFmt,
) -> Result<(String, bool), GitError> {
    if fmt.mode == DiffMode::Patch {
        let diff = repo.diff_tree_to_tree(a, Some(b), Some(&mut fmt.options()))?;
        return Ok((patch_of(&diff)?, diff.deltas().len() > 0));
    }
    let mut changes = Vec::new();
    walk_trees(repo, a, Some(b), "", fmt, &mut changes)?;
    let text = if fmt.mode == DiffMode::Nothing {
        String::new()
    } else {
        fmt.render(&changes)
    };
    Ok((text, !changes.is_empty()))
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
        None if commit.parent_count() > 1 => return Ok((String::new(), false)),
        None if commit.parent_count() == 1 => Some(commit.parent(0)?),
        None if !o.root => return Ok((String::new(), false)),
        None => None,
    };
    let from = parent.map(|p| p.tree()).transpose()?;
    let (text, changed) = tree_pair(repo, from.as_ref(), &commit.tree()?, &o.fmt)?;
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
        repo.revparse_single(rev)
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
            _ => 'M',
        };
        if status == 'U' {
            out.push(Change {
                old_mode: 0,
                new_mode: 0,
                old_id: Oid::ZERO_SHA1,
                new_id: Oid::ZERO_SHA1,
                status,
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
            path,
        });
    }
    Ok(out)
}

fn show(
    repo: &Repository,
    diff: &Diff,
    fmt: &DiffFmt,
    worktree: bool,
) -> Result<(String, bool), GitError> {
    let changed = diff.deltas().len() > 0;
    Ok(match fmt.mode {
        DiffMode::Patch => (patch_of(diff)?, changed),
        DiffMode::Nothing => (String::new(), changed),
        _ => (fmt.render(&raw_changes(repo, diff, worktree)?), changed),
    })
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
        .revparse_single(rev)
        .map_err(|_| GitError::Other(format!("ambiguous argument '{rev}'")))?
        .peel_to_tree()?;
    let mut o = fmt.options();
    let diff = if cached {
        repo.diff_tree_to_index(Some(&tree), None, Some(&mut o))?
    } else {
        repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut o))?
    };
    show(&repo, &diff, fmt, !cached)
}

/// `git diff-files`: the index against the working tree.
pub fn diff_files(git_dir: &Path, fmt: &DiffFmt) -> Result<(String, bool), GitError> {
    let repo = open(git_dir)?;
    let diff = repo.diff_index_to_workdir(None, Some(&mut fmt.options()))?;
    show(&repo, &diff, fmt, true)
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
}

/// A three-way merge of file contents, and how many conflicts it left.
pub fn merge_file(
    ours: &[u8],
    base: &[u8],
    theirs: &[u8],
    o: &MergeFileOpts,
) -> Result<(Vec<u8>, usize), GitError> {
    let mut opts = git2::MergeFileOptions::new();
    opts.our_label(o.labels[0].as_str())
        .ancestor_label(o.labels[1].as_str())
        .their_label(o.labels[2].as_str())
        .simplify_alnum(o.alnum);
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
}

/// Every blob under a tree, by path.
fn flatten(tree: Option<&Tree>) -> Result<BTreeMap<String, (u32, Oid)>, GitError> {
    let mut out = BTreeMap::new();
    if let Some(tree) = tree {
        tree.walk(git2::TreeWalkMode::PreOrder, |dir, e| {
            if e.kind() != Some(ObjectType::Tree) {
                let name = String::from_utf8_lossy(e.name_bytes());
                out.insert(format!("{dir}{name}"), (e.filemode() as u32, e.id()));
            }
            git2::TreeWalkResult::Ok
        })?;
    }
    Ok(out)
}

/// `git merge-tree --write-tree`: the merged tree (conflicts written with
/// markers), git's text, and whether the merge was clean.
pub fn merge_tree(git_dir: &Path, o: &MergeTreeOpts) -> Result<(String, bool), GitError> {
    let repo = open(git_dir)?;
    let commit = |rev: &str| -> Result<git2::Commit, GitError> {
        repo.revparse_single(rev)
            .and_then(|c| c.peel_to_commit())
            .map_err(|_| GitError::Other(format!("merge-tree: {rev} - not something we can merge")))
    };
    let (c1, c2) = (commit(&o.branch1)?, commit(&o.branch2)?);
    let base_tree = match &o.merge_base {
        Some(b) => Some(repo.revparse_single(b)?.peel_to_tree()?),
        None => match repo.merge_base(c1.id(), c2.id()) {
            Ok(b) => Some(repo.find_commit(b)?.tree()?),
            Err(_) if o.allow_unrelated => None,
            Err(_) => {
                return Err(GitError::Other(
                    "refusing to merge unrelated histories".into(),
                ));
            }
        },
    };
    let (t1, t2) = (c1.tree()?, c2.tree()?);
    let mut index = if o.merge_base.is_some() || base_tree.is_none() {
        let empty = repo.find_tree(repo.treebuilder(None)?.write()?)?;
        repo.merge_trees(base_tree.as_ref().unwrap_or(&empty), &t1, &t2, None)?
    } else {
        repo.merge_commits(&c1, &c2, None)?
    };
    let (base, ours, theirs) = (
        flatten(base_tree.as_ref())?,
        flatten(Some(&t1))?,
        flatten(Some(&t2))?,
    );
    let mut messages: BTreeMap<String, Vec<(&str, String)>> = BTreeMap::new();
    for (path, o1) in &ours {
        let (Some(o2), b) = (theirs.get(path), base.get(path)) else {
            continue;
        };
        if o1 != o2 && b != Some(o1) && b != Some(o2) {
            messages
                .entry(path.clone())
                .or_default()
                .push(("Auto-merging", format!("Auto-merging {path}")));
        }
    }
    let mut stages = Vec::new();
    let mut fixes = Vec::new();
    for c in index.conflicts()? {
        let c = c?;
        let entry = c.our.as_ref().or(c.their.as_ref()).or(c.ancestor.as_ref());
        let Some(entry) = entry else { continue };
        let path = String::from_utf8_lossy(&entry.path).into_owned();
        for (stage, e) in [(1, &c.ancestor), (2, &c.our), (3, &c.their)] {
            if let Some(e) = e {
                stages.push((path.clone(), stage, e.mode, e.id));
            }
        }
        let note = messages.entry(path.clone()).or_default();
        let mut keep = (entry.mode, entry.id);
        match (&c.ancestor, &c.our, &c.their) {
            (b, Some(ours), Some(theirs)) => {
                let blob = |e: Option<&git2::IndexEntry>| -> Result<Vec<u8>, GitError> {
                    Ok(match e {
                        Some(e) => repo.find_blob(e.id)?.content().to_vec(),
                        None => Vec::new(),
                    })
                };
                let opts = MergeFileOpts {
                    labels: [o.branch1.clone(), String::new(), o.branch2.clone()],
                    ..Default::default()
                };
                let (merged, _) = merge_file(
                    &blob(Some(ours))?,
                    &blob(b.as_ref())?,
                    &blob(Some(theirs))?,
                    &opts,
                )?;
                keep = (ours.mode, repo.blob(&merged)?);
                let kind = if b.is_some() { "content" } else { "add/add" };
                note.push((
                    "CONFLICT (contents)",
                    format!("CONFLICT ({kind}): Merge conflict in {path}"),
                ));
            }
            (_, ours, _) => {
                let (gone, kept) = if ours.is_some() {
                    (&o.branch2, &o.branch1)
                } else {
                    (&o.branch1, &o.branch2)
                };
                note.push((
                    "CONFLICT (modify/delete)",
                    format!(
                        "CONFLICT (modify/delete): {path} deleted in {gone} and modified in {kept}.  Version {kept} of {path} left in tree."
                    ),
                ));
            }
        }
        fixes.push((path, keep));
    }
    let clean = fixes.is_empty();
    for (path, (mode, id)) in fixes {
        index.conflict_remove(Path::new(&path))?;
        let time = git2::IndexTime::new(0, 0);
        index.add(&git2::IndexEntry {
            ctime: time,
            mtime: time,
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            file_size: 0,
            id,
            flags: path.len().min(0xfff) as u16,
            flags_extended: 0,
            path: path.into_bytes(),
        })?;
    }
    let tree = index.write_tree_to(&repo)?;
    let end = if o.z { '\0' } else { '\n' };
    let mut out = format!("{tree}{end}");
    stages.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    if o.name_only {
        stages.dedup_by(|a, b| a.0 == b.0);
    }
    for (path, stage, mode, id) in &stages {
        if o.name_only {
            let _ = write!(out, "{path}{end}");
        } else {
            let _ = write!(out, "{mode:06o} {id} {stage}\t{path}{end}");
        }
    }
    if o.messages.unwrap_or(!clean) {
        out.push(end);
        for (path, notes) in &messages {
            for (kind, text) in notes {
                if o.z {
                    let _ = write!(out, "1\0{path}\0{kind}\0{text}\n\0");
                } else {
                    let _ = writeln!(out, "{text}");
                }
            }
        }
    }
    Ok((out, clean))
}
