//! `git sparse-checkout`: the patterns in `$GIT_DIR/info/sparse-checkout`,
//! cone and non-cone, the skip-worktree bits they set, and the working tree
//! files they leave out, with git's rules, files and messages.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use git2::build::CheckoutBuilder;
use git2::{IndexEntry, Oid, Repository, Status};

use crate::error::GitError;
use crate::index_ops::Report;

const SKIP_WORKTREE: u16 = 1 << 14;

fn fatal(message: impl Into<String>) -> GitError {
    GitError::Other(message.into())
}

fn skipped(e: &IndexEntry) -> bool {
    e.flags_extended & SKIP_WORKTREE != 0
}

fn stage(e: &IndexEntry) -> u16 {
    (e.flags >> 12) & 3
}

fn path_of(e: &IndexEntry) -> String {
    String::from_utf8_lossy(&e.path).into_owned()
}

/// One pattern line: `!` and a trailing `/` taken off, as git parses it.
struct Pat {
    neg: bool,
    dir: bool,
    text: String,
}

/// The pattern lines of a sparse-checkout file: no blanks or comments, and
/// trailing unescaped spaces trimmed.
fn lines(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let mut end = l.len();
            while end > 0 && l.as_bytes()[end - 1] == b' ' && !l[..end - 1].ends_with('\\') {
                end -= 1;
            }
            let l = &l[..end];
            (!l.is_empty() && !l.starts_with('#')).then(|| l.to_owned())
        })
        .collect()
}

fn parse(line: &str) -> Pat {
    let (neg, rest) = match line.strip_prefix('!') {
        Some(r) => (true, r),
        None => (false, line.strip_prefix('\\').unwrap_or(line)),
    };
    let dir = rest.len() > 1 && rest.ends_with('/');
    Pat {
        neg,
        dir,
        text: if dir { &rest[..rest.len() - 1] } else { rest }.to_owned(),
    }
}

fn glob_special(c: u8) -> bool {
    matches!(c, b'*' | b'?' | b'[' | b'\\')
}

/// Cone-mode patterns: the folders taken whole and their parent folders,
/// each with a leading `/`, as git's hashmaps hold them.
#[derive(Default)]
struct Cone {
    full: bool,
    recursive: BTreeSet<String>,
    parents: BTreeSet<String>,
}

/// git's dup_and_filter_pattern: escapes dropped, a trailing `/*` cut.
fn filtered(p: &str) -> String {
    let mut out = String::new();
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' {
            match chars.next() {
                Some(n) => n,
                None => break,
            }
        } else {
            c
        });
    }
    if out.len() > 2 && out.ends_with("/*") {
        out.truncate(out.len() - 2);
    }
    out
}

/// Whether some folder above `path` is in `set`.
fn contains_parent(set: &BTreeSet<String>, path: &str) -> bool {
    let mut p = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    while let Some(i) = p.rfind('/').filter(|i| *i > 0) {
        p.truncate(i);
        if set.contains(&p) {
            return true;
        }
    }
    false
}

impl Cone {
    /// Read `lines` as cone patterns, as git's add_pattern_to_hashsets does,
    /// or `None` (with git's warnings) when they are not.
    fn parse(lines: &[String], warn: &mut String) -> Option<Cone> {
        let mut cone = Cone::default();
        for line in lines {
            let p = parse(line);
            let unrecognized = |warn: &mut String, what: &str| {
                warn.push_str(&format!(
                    "warning: unrecognized {what}pattern: '{}'\nwarning: disabling cone pattern matching\n",
                    p.text
                ));
                None
            };
            if p.neg && p.dir && p.text == "/*" {
                cone.full = false;
                continue;
            }
            if !p.neg && p.text == "/*" {
                cone.full = true;
                continue;
            }
            if p.text.len() < 2 || !p.text.starts_with('/') || p.text.contains("**") {
                return unrecognized(warn, "");
            }
            if !p.dir {
                return unrecognized(warn, "");
            }
            let b = p.text.as_bytes();
            for i in 1..b.len() {
                let (prev, cur, next) = (b[i - 1], b[i], b.get(i + 1).copied());
                if !glob_special(cur)
                    || prev == b'\\'
                    || (cur == b'\\' && next.is_some_and(glob_special))
                    || (prev == b'/' && cur == b'*' && next.is_none())
                {
                    continue;
                }
                return unrecognized(warn, "");
            }
            if p.text.len() > 2 && p.text.ends_with("/*") {
                if !p.neg {
                    return unrecognized(warn, "");
                }
                let t = filtered(&p.text);
                cone.recursive.remove(&t);
                cone.parents.insert(t);
                continue;
            }
            if p.neg {
                return unrecognized(warn, "negative ");
            }
            let t = filtered(&p.text);
            if cone.parents.contains(&t) {
                warn.push_str(&format!(
                    "warning: your sparse-checkout file may have issues: pattern '{}' is repeated\n\
                     warning: disabling cone pattern matching\n",
                    p.text
                ));
                return None;
            }
            cone.recursive.insert(t);
        }
        Some(cone)
    }

    /// git's insert_recursive_pattern: `path` whole, and each folder above it
    /// as a parent.
    fn insert(&mut self, path: &str) {
        self.recursive.insert(path.to_owned());
        let mut p = path;
        while let Some(i) = p.rfind('/').filter(|i| *i > 0) {
            p = &p[..i];
            self.parents.insert(p.to_owned());
        }
    }

    fn includes(&self, path: &str) -> bool {
        if self.full {
            return true;
        }
        let p = format!("/{path}");
        if self.recursive.contains(&p) {
            return true;
        }
        match p.rfind('/') {
            Some(0) | None => true,
            Some(i) => self.parents.contains(&p[..i]) || contains_parent(&self.recursive, &p),
        }
    }

    /// The file git's write_cone_to_file writes.
    fn write(&self) -> String {
        let escape = |p: &str| {
            let mut out = String::new();
            for c in p.chars() {
                if c.is_ascii() && glob_special(c as u8) {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        };
        let mut out = String::from("/*\n!/*/\n");
        for p in &self.parents {
            if !self.recursive.contains(p) && !contains_parent(&self.recursive, p) {
                let e = escape(p);
                out.push_str(&format!("{e}/\n!{e}/*/\n"));
            }
        }
        for p in &self.recursive {
            if !contains_parent(&self.recursive, p) {
                out.push_str(&format!("{}/\n", escape(p)));
            }
        }
        out
    }

    /// The folders `list` shows: the whole ones, without the leading `/`.
    fn folders(&self) -> Vec<String> {
        self.recursive.iter().map(|p| p[1..].to_owned()).collect()
    }
}

/// A sparse-checkout definition: cone patterns, or gitignore-style ones.
enum Rules {
    Cone(Cone),
    Patterns(ignore::gitignore::Gitignore),
}

impl Rules {
    fn patterns(lines: &[String]) -> Rules {
        let mut b = ignore::gitignore::GitignoreBuilder::new("");
        for l in lines {
            let _ = b.add_line(None, l);
        }
        Rules::Patterns(
            b.build()
                .unwrap_or_else(|_| ignore::gitignore::Gitignore::empty()),
        )
    }

    fn of(lines: &[String], cone: bool, warn: &mut String) -> Rules {
        match cone.then(|| Cone::parse(lines, warn)).flatten() {
            Some(c) => Rules::Cone(c),
            None => Rules::patterns(lines),
        }
    }

    /// Whether the file at `path` belongs in the working tree.
    fn includes(&self, path: &str) -> bool {
        match self {
            Rules::Cone(c) => c.includes(path),
            Rules::Patterns(g) => {
                // A folder's match carries to what is inside it unless a
                // pattern decides otherwise, as git's clear_ce_flags walks.
                let mut state = false;
                let parts: Vec<&str> = path.split('/').collect();
                for i in 0..parts.len() {
                    let sub = parts[..=i].join("/");
                    match g.matched(&sub, i + 1 < parts.len()) {
                        ignore::Match::Ignore(_) => state = true,
                        ignore::Match::Whitelist(_) => state = false,
                        ignore::Match::None => {}
                    }
                }
                state
            }
        }
    }
}

fn file(repo: &Repository) -> PathBuf {
    repo.path().join("info/sparse-checkout")
}

/// The config values that govern sparsity: core.sparseCheckout and
/// core.sparseCheckoutCone (git's default true).
fn modes(repo: &Repository) -> (bool, bool) {
    let cfg = repo.config().and_then(|mut c| c.snapshot());
    let get = |k: &str, d: bool| {
        cfg.as_ref()
            .ok()
            .and_then(|c| c.get_bool(k).ok())
            .unwrap_or(d)
    };
    (
        get("core.sparseCheckout", false),
        get("core.sparseCheckoutCone", true),
    )
}

/// The rules of the repository's sparse checkout, when it has one.
fn rules(repo: &Repository) -> Option<Rules> {
    let (on, cone) = modes(repo);
    if !on {
        return None;
    }
    let text = std::fs::read_to_string(file(repo)).ok()?;
    Some(Rules::of(&lines(&text), cone, &mut String::new()))
}

/// git's set_config: sparsity in the worktree's own config, turning on
/// extensions.worktreeConfig first. `cone` is `None` to turn sparsity off.
fn set_config(repo: &Repository, cone: Option<bool>) -> Result<(), GitError> {
    if !repo
        .config()?
        .get_bool("extensions.worktreeConfig")
        .unwrap_or(false)
    {
        git2::Config::open(&repo.commondir().join("config"))?
            .set_bool("extensions.worktreeConfig", true)?;
    }
    let mut c = git2::Config::open(&repo.path().join("config.worktree"))?;
    c.set_bool("core.sparseCheckout", cone.is_some())?;
    c.set_bool("core.sparseCheckoutCone", cone == Some(true))?;
    if cone.is_none() {
        c.set_bool("index.sparse", false)?;
    }
    Ok(())
}

/// Whether the file at `full` holds blob `id`, unfiltered.
fn same_blob(full: &Path, id: Oid) -> bool {
    let data = match std::fs::read_link(full) {
        Ok(target) => Some(target.to_string_lossy().into_owned().into_bytes()),
        Err(_) => std::fs::read(full).ok(),
    };
    data.is_some_and(|d| Oid::hash_object(git2::ObjectType::Blob, &d).is_ok_and(|h| h == id))
}

/// Remove `rel` from the working tree, and the folders it leaves empty.
fn remove(top: &Path, rel: &str) {
    let full = top.join(rel);
    let _ = std::fs::remove_file(&full);
    let mut dir = full.parent();
    while let Some(d) = dir.filter(|d| *d != top) {
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn warn_list(out: &mut String, head: &str, paths: &[String]) -> bool {
    if paths.is_empty() {
        return false;
    }
    out.push_str(&format!("warning: {head}\n"));
    for p in paths {
        out.push_str(&format!("\t{p}\n"));
    }
    true
}

/// Set skip-worktree bits as `rules` ask and add or remove the files, as
/// git's update_sparsity does; the warnings go to `warn`. `None` means no
/// sparsity: every file present.
fn update(repo: &Repository, rules: Option<&Rules>, warn: &mut String) -> Result<(), GitError> {
    let top = repo
        .workdir()
        .ok_or_else(|| fatal("this operation must be run in a work tree"))?
        .to_path_buf();
    let mut index = repo.index()?;
    index.read(false)?;
    let entries: Vec<IndexEntry> = index.iter().collect();
    if entries.iter().any(|e| stage(e) != 0) {
        return Err(fatal("you need to resolve your current index first"));
    }
    let (mut dirty, mut present, mut add) = (Vec::new(), Vec::new(), Vec::new());
    let mut changed = Vec::new();
    for mut e in entries {
        let path = path_of(&e);
        let want = rules.is_none_or(|r| r.includes(&path));
        let on_disk = top.join(&path).symlink_metadata().is_ok();
        if want && skipped(&e) {
            e.flags_extended &= !SKIP_WORKTREE;
            if !on_disk {
                add.push(path.clone());
            } else if !same_blob(&top.join(&path), e.id) {
                present.push(path.clone());
            }
            changed.push(e);
        } else if !want && !skipped(&e) {
            let clean = !on_disk
                || !repo.status_file(Path::new(&path)).is_ok_and(|s| {
                    s.intersects(Status::WT_MODIFIED | Status::WT_TYPECHANGE | Status::WT_DELETED)
                });
            if !clean {
                dirty.push(path);
                continue;
            }
            if on_disk {
                remove(&top, &path);
            }
            e.flags_extended |= SKIP_WORKTREE;
            changed.push(e);
        }
    }
    for e in &changed {
        index.add(e)?;
    }
    if !changed.is_empty() {
        index.write()?;
    }
    if !add.is_empty() {
        let mut co = CheckoutBuilder::new();
        co.force()
            .recreate_missing(true)
            .disable_pathspec_match(true);
        for p in &add {
            co.path(p);
        }
        repo.checkout_index(Some(&mut index), Some(&mut co))?;
    }
    let mut any = warn_list(
        warn,
        "The following paths are not up to date and were left despite sparse patterns:",
        &dirty,
    );
    any |= warn_list(
        warn,
        "The following paths were already present and thus not updated despite sparse patterns:",
        &present,
    );
    if any {
        warn.push_str(
            "\nAfter fixing the above paths, you may want to run `git sparse-checkout reapply`.\n",
        );
    }
    if let Some(Rules::Cone(cone)) = rules {
        clean_folders(repo, &top, cone, warn)?;
    }
    Ok(())
}

/// git's clean_tracked_sparse_directories: a folder wholly outside the cone
/// goes, ignored files and all, unless it holds untracked files.
fn clean_folders(
    repo: &Repository,
    top: &Path,
    cone: &Cone,
    warn: &mut String,
) -> Result<(), GitError> {
    let index = repo.index()?;
    let mut tracked: BTreeSet<String> = BTreeSet::new();
    let mut folders: BTreeSet<String> = BTreeSet::new();
    let mut kept: BTreeSet<String> = BTreeSet::new();
    for e in index.iter() {
        let path = path_of(&e);
        tracked.insert(path.clone());
        let Some(dir) = sparse_folder(cone, &path) else {
            continue;
        };
        if !skipped(&e) {
            kept.insert(dir.clone());
        }
        folders.insert(dir);
    }
    for dir in folders.difference(&kept) {
        let full = top.join(dir);
        if !full.is_dir() {
            continue;
        }
        let untracked = walkdir::WalkDir::new(&full)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|f| !f.file_type().is_dir())
            .any(|f| {
                let rel = f.path().strip_prefix(top).unwrap_or(f.path());
                let rel = rel.to_string_lossy();
                !tracked.contains(rel.as_ref())
                    && !repo
                        .is_path_ignored(Path::new(rel.as_ref()))
                        .unwrap_or(false)
            });
        if untracked {
            warn.push_str(&format!(
                "warning: directory '{dir}/' contains untracked files, but is not in the sparse-checkout cone\n"
            ));
        } else {
            std::fs::remove_dir_all(&full)?;
        }
    }
    Ok(())
}

/// The top folder of `path` wholly outside the cone, if there is one.
fn sparse_folder(cone: &Cone, path: &str) -> Option<String> {
    if cone.full || cone.includes(path) {
        return None;
    }
    let mut at = 0;
    while let Some(i) = path[at..].find('/') {
        let dir = &path[..at + i];
        if !cone.parents.contains(&format!("/{dir}")) {
            return Some(dir.to_owned());
        }
        at += i + 1;
    }
    None
}

/// The skip-worktree paths of `index`.
pub(crate) fn skipped_paths(index: &git2::Index) -> std::collections::HashSet<String> {
    index.iter().filter(skipped).map(|e| path_of(&e)).collect()
}

/// A callback for libgit2's add_all and update_all that keeps skip-worktree
/// entries whose files are absent, which libgit2 would drop as deleted.
pub(crate) fn keep_skipped(
    repo: &Repository,
    index: &git2::Index,
) -> impl FnMut(&Path, &[u8]) -> i32 + use<> {
    let top = repo.workdir().map(Path::to_path_buf).unwrap_or_default();
    let skips = skipped_paths(index);
    move |p: &Path, _: &[u8]| {
        i32::from(
            !skips.is_empty()
                && skips.contains(p.to_string_lossy().as_ref())
                && top.join(p).symlink_metadata().is_err(),
        )
    }
}

/// Paths materialized by [`widen`], with the blob each got, for [`narrow`].
pub struct Widened(Vec<(String, Oid)>);

/// Write out every skip-worktree file and clear its bit, so a command that
/// moves HEAD, the index or the files can run on a full working tree; then
/// [`narrow`] puts sparsity back. `None` when the repository is not sparse.
// ponytail: materializes every skipped file for the length of the command;
// teach the checkouts themselves about skip-worktree if sparse trees get huge.
pub fn widen(git_dir: &Path) -> Result<Option<Widened>, GitError> {
    let repo = Repository::open(git_dir)?;
    if rules(&repo).is_none() || repo.is_bare() {
        return Ok(None);
    }
    let top = repo.workdir().map(Path::to_path_buf).unwrap_or_default();
    let mut index = repo.index()?;
    let mut out = Vec::new();
    for mut e in index.iter().collect::<Vec<_>>() {
        if stage(&e) != 0 || !skipped(&e) {
            continue;
        }
        let path = path_of(&e);
        if top.join(&path).symlink_metadata().is_ok() {
            continue;
        }
        e.flags_extended &= !SKIP_WORKTREE;
        index.add(&e)?;
        out.push((path, e.id));
    }
    if out.is_empty() {
        return Ok(Some(Widened(out)));
    }
    index.write()?;
    let mut co = CheckoutBuilder::new();
    co.force()
        .recreate_missing(true)
        .disable_pathspec_match(true);
    for (p, _) in &out {
        co.path(p);
    }
    repo.checkout_index(Some(&mut index), Some(&mut co))?;
    Ok(Some(Widened(out)))
}

/// Put sparsity back after [`widen`]: files outside the patterns that match
/// the index, or are still as widen wrote them, go, and their entries get
/// the skip-worktree bit, as git's checkouts leave them.
pub fn narrow(git_dir: &Path, widened: Widened) -> Result<(), GitError> {
    let repo = Repository::open(git_dir)?;
    let Some(rules) = rules(&repo) else {
        return Ok(());
    };
    let top = repo.workdir().map(Path::to_path_buf).unwrap_or_default();
    let written: std::collections::HashMap<String, Oid> = widened.0.into_iter().collect();
    let as_written = |path: &str| {
        written
            .get(path)
            .is_some_and(|id| same_blob(&top.join(path), *id))
    };
    let mut index = repo.index()?;
    index.read(false)?;
    let mut tracked = BTreeSet::new();
    let mut changed = Vec::new();
    for mut e in index.iter().collect::<Vec<_>>() {
        let path = path_of(&e);
        tracked.insert(path.clone());
        if stage(&e) != 0 || skipped(&e) || rules.includes(&path) {
            continue;
        }
        let on_disk = top.join(&path).symlink_metadata().is_ok();
        let clean = !on_disk
            || as_written(&path)
            || !repo.status_file(Path::new(&path)).is_ok_and(|s| {
                s.intersects(Status::WT_MODIFIED | Status::WT_TYPECHANGE | Status::WT_DELETED)
            });
        if clean {
            if on_disk {
                remove(&top, &path);
            }
            e.flags_extended |= SKIP_WORKTREE;
            changed.push(e);
        }
    }
    for e in &changed {
        index.add(e)?;
    }
    if !changed.is_empty() {
        index.write()?;
    }
    for path in written.keys() {
        if !tracked.contains(path) && as_written(path) {
            remove(&top, path);
        }
    }
    Ok(())
}

/// What lies outside the sparse checkout: skip-worktree entries and paths
/// the patterns leave out.
struct Outside {
    rules: Option<Rules>,
    skips: BTreeSet<String>,
}

impl Outside {
    fn load(repo: &Repository) -> Result<Outside, GitError> {
        let skips = repo
            .index()?
            .iter()
            .filter(|e| stage(e) == 0 && skipped(e))
            .map(|e| path_of(&e))
            .collect();
        Ok(Outside {
            rules: rules(repo),
            skips,
        })
    }

    fn sparse(&self) -> bool {
        self.rules.is_some() || !self.skips.is_empty()
    }

    fn has(&self, path: &str) -> bool {
        self.skips.contains(path) || self.rules.as_ref().is_some_and(|r| !r.includes(path))
    }
}

/// What git's add, rm and mv refuse without `--sparse`: the pathspecs
/// (from the top) that match only tracked paths outside the sparse checkout,
/// then, with `untracked`, the untracked files outside it they match.
pub fn outside_only(
    git_dir: &Path,
    specs: &[String],
    untracked: bool,
) -> Result<Vec<String>, GitError> {
    let repo = Repository::open(git_dir)?;
    let outside = Outside::load(&repo)?;
    if !outside.sparse() {
        return Ok(Vec::new());
    }
    let tracked: Vec<String> = repo
        .index()?
        .iter()
        .filter(|e| stage(e) == 0)
        .map(|e| path_of(&e))
        .collect();
    let mut out: Vec<String> = specs
        .iter()
        .filter(|spec| {
            let one = [spec.to_string()];
            let mut hits = tracked
                .iter()
                .filter(|p| spec.as_str() == "." || crate::pathspec_matches(&one, p));
            let first = hits.next();
            first.is_some_and(|p| outside.has(p)) && hits.all(|p| outside.has(p))
        })
        .cloned()
        .collect();
    if untracked {
        let mut o = git2::StatusOptions::new();
        o.include_untracked(true).recurse_untracked_dirs(true);
        for s in specs.iter().filter(|s| s.as_str() != ".") {
            o.pathspec(s);
        }
        for e in repo.statuses(Some(&mut o))?.iter() {
            if e.status().contains(Status::WT_NEW)
                && let Some(p) = e.path().ok().filter(|p| outside.has(p))
            {
                out.push(p.to_owned());
            }
        }
    }
    Ok(out)
}

static EVERYWHERE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--sparse`: let add and commit reach paths outside the sparse checkout.
pub fn sparse_everywhere() {
    EVERYWHERE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// [`keep_skipped`], and paths outside the sparse checkout too unless
/// `--sparse` asked for them, as git's add and commit -a leave them.
pub(crate) fn keep_sparse(
    repo: &Repository,
    index: &git2::Index,
) -> impl FnMut(&Path, &[u8]) -> i32 + use<> {
    let outside = (!EVERYWHERE.load(std::sync::atomic::Ordering::Relaxed))
        .then(|| Outside::load(repo).ok())
        .flatten()
        .filter(Outside::sparse);
    let mut skip = keep_skipped(repo, index);
    move |p: &Path, m: &[u8]| {
        if skip(p, m) != 0 {
            return 1;
        }
        i32::from(
            outside
                .as_ref()
                .is_some_and(|o| o.has(p.to_string_lossy().as_ref())),
        )
    }
}

/// Whether `path` (from the top) lies outside the sparse checkout.
pub fn outside_sparse(git_dir: &Path, path: &str) -> Result<bool, GitError> {
    Ok(Outside::load(&Repository::open(git_dir)?)?.has(path))
}

/// `git mv --sparse` of the file `from` to `to` when either lies outside the
/// sparse checkout: the entry moves in the index, and the file appears at
/// `to` only inside the checkout (with the skip-worktree bit outside).
pub fn sparse_mv(git_dir: &Path, from: &str, to: &str) -> Result<(), GitError> {
    let repo = Repository::open(git_dir)?;
    let outside = Outside::load(&repo)?;
    let top = repo.workdir().map(Path::to_path_buf).unwrap_or_default();
    let mut index = repo.index()?;
    let Some(mut e) = index.get_path(Path::new(from), 0) else {
        return Err(fatal(format!(
            "not under version control, source={from}, destination={to}"
        )));
    };
    let (src, dst) = (top.join(from), top.join(to));
    let had = src.symlink_metadata().is_ok();
    if had {
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::rename(&src, &dst)?;
        remove(&top, from);
    }
    index.remove_path(Path::new(from))?;
    e.path = to.as_bytes().to_vec();
    e.flags_extended &= !SKIP_WORKTREE;
    let away = outside.rules.as_ref().is_some_and(|r| !r.includes(to));
    if away && (!had || same_blob(&dst, e.id)) {
        if had {
            remove(&top, to);
        }
        e.flags_extended |= SKIP_WORKTREE;
    }
    index.add(&e)?;
    index.write()?;
    if !had && !away {
        let mut co = CheckoutBuilder::new();
        co.force()
            .recreate_missing(true)
            .disable_pathspec_match(true)
            .path(to);
        repo.checkout_index(Some(&mut index), Some(&mut co))?;
    }
    Ok(())
}

/// Stdin lines, C-unquoted when quoted, as git reads patterns.
fn input_lines(data: &[u8], end: u8) -> Vec<String> {
    data.split(|b| *b == end)
        .map(|l| String::from_utf8_lossy(l.strip_suffix(b"\r").unwrap_or(l)).into_owned())
        .filter(|l| !l.is_empty())
        .map(|l| {
            if l.starts_with('"') {
                crate::apply::unquote(&l)
            } else {
                l
            }
        })
        .collect()
}

/// git's strbuf_normalize_path on a folder name: `.` and `..` resolved,
/// slashes collapsed, no trailing slash; `None` when it climbs out.
fn normalize(p: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for c in p.trim().split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// git's usage error: the error, then the usage and options as git prints
/// them.
fn usage(block: &str, error: &str) -> GitError {
    fatal(format!("error: {error}\n{block}"))
}

const USAGE: &str = "usage: git sparse-checkout (init | list | set | add | reapply | disable | check-rules) [<options>]\n";

/// A subcommand's usage and options, as its `-h` prints them.
fn usage_of(sub: &str) -> Option<String> {
    let cone = "    --[no-]cone           initialize the sparse-checkout in cone mode\n";
    let index = "    --[no-]sparse-index   toggle the use of a sparse index\n";
    let checks = "    --skip-checks         skip some sanity checks on the given paths that might give false positives\n";
    let (line, opts) = match sub {
        "list" | "disable" => (String::new(), String::new()),
        "init" => (" [--cone] [--[no-]sparse-index]".to_owned(), format!("{cone}{index}")),
        "set" => (
            " [--[no-]cone] [--[no-]sparse-index] [--skip-checks] (--stdin | <patterns>)".to_owned(),
            format!("{cone}{index}{checks}    --stdin               read patterns from standard in\n"),
        ),
        "add" => (
            " [--skip-checks] (--stdin | <patterns>)".to_owned(),
            format!("{checks}    --[no-]stdin          read patterns from standard in\n"),
        ),
        "reapply" => (" [--[no-]cone] [--[no-]sparse-index]".to_owned(), format!("{cone}{index}")),
        "check-rules" => (
            " [-z] [--skip-checks][--[no-]cone] [--rules-file <file>]".to_owned(),
            "    -z                    terminate input and output files by a NUL character\n    \
             --[no-]cone           when used with --rules-file interpret patterns as cone mode patterns\n    \
             --[no-]rules-file <file>\n                          \
             use patterns in <file> instead of the current ones.\n"
                .to_owned(),
        ),
        _ => return None,
    };
    let opts = if opts.is_empty() {
        opts
    } else {
        format!("\n{opts}")
    };
    Some(format!("usage: git sparse-checkout {sub}{line}\n{opts}"))
}

/// `git sparse-checkout <sub> [<options>]`: `prefix` is the current folder
/// under the top, `stdin` reads standard input for `--stdin` and check-rules.
pub fn sparse_checkout(
    git_dir: &Path,
    prefix: &str,
    args: &[String],
    stdin: &mut dyn FnMut() -> std::io::Result<Vec<u8>>,
) -> Result<Report, GitError> {
    let repo = Repository::open(git_dir)?;
    if repo.is_bare() {
        return Err(fatal("this operation must be run in a work tree"));
    }
    let Some((sub, rest)) = args.split_first() else {
        return Err(usage(USAGE, "need a subcommand"));
    };
    let Some(line) = usage_of(sub) else {
        return Err(usage(USAGE, &format!("unknown subcommand: `{sub}'")));
    };
    let line = line.as_str();
    let allowed: &[&str] = match sub.as_str() {
        "init" => &["--cone", "--no-cone", "--sparse-index", "--no-sparse-index"],
        "set" => &[
            "--cone",
            "--no-cone",
            "--sparse-index",
            "--no-sparse-index",
            "--skip-checks",
            "--no-skip-checks",
            "--stdin",
            "--no-stdin",
        ],
        "add" => &["--skip-checks", "--no-skip-checks", "--stdin", "--no-stdin"],
        "reapply" => &["--cone", "--no-cone", "--sparse-index", "--no-sparse-index"],
        "check-rules" => &["-z", "--null", "--cone", "--no-cone", "--rules-file"],
        _ => &[],
    };
    let (mut cone, mut sparse_index): (Option<bool>, Option<bool>) = (None, None);
    let (mut skip_checks, mut use_stdin, mut z) = (false, false, false);
    let mut rules_file: Option<String> = None;
    let mut words: Vec<String> = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if a == "--" || a == "--end-of-options" {
            words.extend(it.by_ref().cloned());
            break;
        }
        if !a.starts_with('-') || a == "-" {
            words.push(a.clone());
            continue;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        if !allowed.contains(&name) {
            let what = match name.strip_prefix("--") {
                Some(l) => format!("unknown option `{l}'"),
                None => format!("unknown switch `{}'", &name[1..]),
            };
            return Err(usage(line, &what));
        }
        match name {
            "--cone" => cone = Some(true),
            "--no-cone" => cone = Some(false),
            "--sparse-index" => sparse_index = Some(true),
            "--no-sparse-index" => sparse_index = Some(false),
            "--skip-checks" => skip_checks = true,
            "--no-skip-checks" => skip_checks = false,
            "--stdin" => use_stdin = true,
            "--no-stdin" => use_stdin = false,
            "-z" | "--null" => z = true,
            "--rules-file" => {
                rules_file = Some(
                    inline
                        .or_else(|| it.next().cloned())
                        .ok_or_else(|| usage(line, "option `rules-file' requires a value"))?,
                )
            }
            _ => {}
        }
    }
    let (on, was_cone) = modes(&repo);
    let mut report = Report::default();
    let read = || std::fs::read_to_string(file(&repo)).ok();
    match sub.as_str() {
        "list" => {
            if !on {
                return Err(fatal("this worktree is not sparse"));
            }
            let Some(text) = read() else {
                report.err.push_str(
                    "warning: this worktree is not sparse (sparse-checkout file may not exist)\n",
                );
                return Ok(report);
            };
            let lines = lines(&text);
            match Rules::of(&lines, was_cone, &mut report.err) {
                Rules::Cone(c) => {
                    for f in c.folders() {
                        report.out.push_str(&crate::text::quote_path(&f));
                        report.out.push('\n');
                    }
                }
                Rules::Patterns(_) => {
                    for l in lines {
                        report.out.push_str(&l);
                        report.out.push('\n');
                    }
                }
            }
        }
        "disable" => {
            let mut all = ignore::gitignore::GitignoreBuilder::new("");
            let _ = all.add_line(None, "/*");
            let rules = Rules::Patterns(all.build().map_err(|e| fatal(e.to_string()))?);
            update(&repo, Some(&rules), &mut report.err)?;
            set_config(&repo, None)?;
        }
        "reapply" => {
            if !on {
                return Err(fatal(
                    "must be in a sparse-checkout to reapply sparsity patterns",
                ));
            }
            let cone = update_modes(&repo, cone, sparse_index, on, was_cone)?;
            let text = read().unwrap_or_default();
            let rules = Rules::of(&lines(&text), cone, &mut report.err);
            update(&repo, Some(&rules), &mut report.err)?;
        }
        "init" => {
            let cone = update_modes(&repo, cone, sparse_index, on, was_cone)?;
            if let Some(text) = read() {
                let rules = Rules::of(&lines(&text), cone, &mut report.err);
                update(&repo, Some(&rules), &mut report.err)?;
                return Ok(report);
            }
            write_file(&repo, "/*\n!/*/\n")?;
            if repo.head().is_ok() {
                let rules = Rules::of(&["/*".to_owned(), "!/*/".to_owned()], cone, &mut report.err);
                update(&repo, Some(&rules), &mut report.err)?;
            }
        }
        "set" | "add" => {
            if sub == "add" && !on {
                return Err(fatal("no sparse-checkout to add to"));
            }
            let cone = if sub == "set" {
                update_modes(&repo, cone, sparse_index, on, was_cone)?
            } else {
                was_cone
            };
            let defaults = sub == "set" && !cone && !use_stdin && words.is_empty();
            if defaults {
                words = vec!["/*".to_owned(), "!/*/".to_owned()];
            } else {
                sanitize(
                    &repo,
                    &mut words,
                    prefix,
                    cone,
                    skip_checks,
                    &mut report.err,
                )?;
            }
            if use_stdin {
                words.extend(input_lines(&stdin()?, b'\n'));
            }
            let text = if cone {
                let mut new = Cone::default();
                for w in &words {
                    let Some(n) = normalize(w) else {
                        return Err(fatal(format!("could not normalize path {w}")));
                    };
                    if !n.is_empty() {
                        new.insert(&format!("/{n}"));
                    }
                }
                if sub == "add" {
                    let old = lines(&read().unwrap_or_default());
                    let mut warn = String::new();
                    let Some(old) = Cone::parse(&old, &mut warn) else {
                        return Err(fatal(format!(
                            "{warn}fatal: existing sparse-checkout patterns do not use cone mode"
                        )));
                    };

                    for p in &old.recursive {
                        if !contains_parent(&new.recursive, p) || !contains_parent(&new.parents, p)
                        {
                            new.insert(p);
                        }
                    }
                }
                new.write()
            } else {
                let mut all = if sub == "add" {
                    lines(&read().unwrap_or_default())
                } else {
                    Vec::new()
                };
                all.extend(words);
                all.iter().map(|l| format!("{l}\n")).collect()
            };
            let rules = Rules::of(&lines(&text), cone, &mut String::new());
            update(&repo, Some(&rules), &mut report.err)?;
            write_file(&repo, &text)?;
        }
        _ => {
            let cone = match cone {
                Some(c) => c,
                None if rules_file.is_some() => true,
                None if on => was_cone,
                None => true,
            };
            let rules = match &rules_file {
                Some(f) => {
                    let data = std::fs::read(f)
                        .map_err(|e| fatal(format!("could not open '{f}' for reading: {e}")))?;
                    let input = input_lines(&data, b'\n');
                    if cone {
                        let mut c = Cone::default();
                        for w in &input {
                            if let Some(n) = normalize(w).filter(|n| !n.is_empty()) {
                                c.insert(&format!("/{n}"));
                            }
                        }
                        Some(Rules::Cone(c))
                    } else {
                        Some(Rules::patterns(&input))
                    }
                }
                None if on => read().map(|t| Rules::of(&lines(&t), cone, &mut report.err)),
                None => None,
            };
            let end = if z { 0 } else { b'\n' };
            for p in input_lines(&stdin()?, end) {
                if rules.as_ref().is_none_or(|r| r.includes(&p)) {
                    if z {
                        report.out.push_str(&p);
                        report.out.push('\0');
                    } else {
                        report.out.push_str(&crate::text::quote_path(&p));
                        report.out.push('\n');
                    }
                }
            }
        }
    }
    Ok(report)
}

/// git's update_modes: record cone mode (and turn sparsity on) when asked
/// or when not yet sparse; set index.sparse when asked. Returns cone mode.
fn update_modes(
    repo: &Repository,
    cone: Option<bool>,
    sparse_index: Option<bool>,
    on: bool,
    was_cone: bool,
) -> Result<bool, GitError> {
    let record = cone.is_some() || !on;
    let cone = cone.unwrap_or(if on { was_cone } else { true });
    if record {
        set_config(repo, Some(cone))?;
    }
    // libgit2 cannot read a sparse index, so --sparse-index keeps the index
    // full rather than have git write one.
    if sparse_index == Some(false) {
        git2::Config::open(&repo.path().join("config.worktree"))?
            .set_bool("index.sparse", false)?;
    }
    Ok(cone)
}

fn write_file(repo: &Repository, text: &str) -> Result<(), GitError> {
    let path = file(repo);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

/// git's sanitize_paths: folders from the current folder in cone mode, and
/// its checks unless `skip_checks`.
fn sanitize(
    repo: &Repository,
    words: &mut [String],
    prefix: &str,
    cone: bool,
    skip_checks: bool,
    warn: &mut String,
) -> Result<(), GitError> {
    if words.is_empty() {
        return Ok(());
    }
    if !prefix.is_empty() && cone {
        for w in words.iter_mut() {
            *w = normalize(&format!("{prefix}{w}"))
                .ok_or_else(|| fatal(format!("'{w}' is outside repository")))?;
        }
    }
    if skip_checks {
        return Ok(());
    }
    if !prefix.is_empty() && !cone {
        return Err(fatal(
            "please run from the toplevel directory in non-cone mode",
        ));
    }
    if cone {
        for w in words.iter() {
            if w.starts_with('/') {
                return Err(fatal(
                    "specify directories rather than patterns (no leading slash)",
                ));
            }
            if w.starts_with('!') {
                return Err(fatal(
                    "specify directories rather than patterns.  If your directory starts with a '!', pass --skip-checks",
                ));
            }
            if w.contains(['*', '?', '[', ']']) {
                return Err(fatal(
                    "specify directories rather than patterns.  If your directory really has any of '*?[]\\' in it, pass --skip-checks",
                ));
            }
        }
    }
    let index = repo.index()?;
    for w in words.iter() {
        if w.starts_with('/') || index.get_path(Path::new(w), 0).is_none() {
            continue;
        }
        if cone {
            return Err(fatal(format!(
                "'{w}' is not a directory; to treat it as a directory anyway, rerun with --skip-checks"
            )));
        }
        warn.push_str(&format!(
            "warning: pass a leading slash before paths such as '{w}' if you want a single file (see NON-CONE PROBLEMS in the git-sparse-checkout manual).\n"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cone_file_round_trips_as_git_writes_it() {
        let mut c = Cone::default();
        c.insert("/a/b");
        c.insert("/d");
        c.insert("/d/e");
        c.insert("/x*y");
        let text = c.write();
        assert_eq!(text, "/*\n!/*/\n/a/\n!/a/*/\n/a/b/\n/d/\n/x\\*y/\n");
        let back = Cone::parse(&lines(&text), &mut String::new()).unwrap();
        assert!(back.includes("top") && back.includes("a/x") && back.includes("a/b/c/d"));
        assert!(!back.includes("a/c/x") && !back.includes("c/z") && back.includes("x*y/f"));
        assert_eq!(sparse_folder(&back, "a/c/x").as_deref(), Some("a/c"));
    }

    #[test]
    fn non_cone_folders_carry_their_match() {
        let r = Rules::patterns(&lines("/*\n!/a/\na/b/\n"));
        assert!(r.includes("top") && r.includes("c/z") && r.includes("a/b/y"));
        assert!(!r.includes("a/x"));
    }
}
