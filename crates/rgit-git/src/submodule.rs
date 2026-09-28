//! Submodule operations libgit2 has no call for: `summary`, `absorbgitdirs`
//! and `add --name`, done as git does them.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use git2::build::CheckoutBuilder;
use git2::{Oid, Repository};

use crate::error::GitError;
use crate::model::OpProgress;

const GITLINK: u32 = 0o160000;

/// `git submodule summary [--cached | --files] [-n N] [<commit>] [--] [<path>...]`.
pub fn summary(
    repo: &Repository,
    args: &[String],
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let (mut cached, mut files, mut limit) = (false, false, 0usize);
    let (mut rev, mut paths) = (None, Vec::new());
    let mut it = args.iter();
    let number = |v: Option<&String>| -> Result<usize, GitError> {
        v.and_then(|n| n.parse().ok())
            .ok_or_else(|| GitError::Other("-n needs a number".to_owned()))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cached" => cached = true,
            "--files" => files = true,
            "-q" | "--quiet" | "--for-status" => {}
            "-n" | "--summary-limit" => limit = number(it.next())?,
            "--" => paths.extend(it.by_ref().cloned()),
            n if n.starts_with("--summary-limit=") => {
                limit = number(Some(&n["--summary-limit=".len()..].to_owned()))?
            }
            n if n.starts_with("-n") => limit = number(Some(&n[2..].to_owned()))?,
            r if rev.is_none()
                && paths.is_empty()
                && repo.revparse_single(&format!("{r}^0")).is_ok() =>
            {
                rev = Some(r.to_owned())
            }
            p => paths.push(p.to_owned()),
        }
    }
    if cached && files {
        return Err(GitError::Other(
            "--cached and --files are mutually exclusive".to_owned(),
        ));
    }
    let top = repo.workdir().unwrap_or(repo.path()).to_path_buf();
    let mut index = BTreeMap::new();
    for e in repo.index()?.iter() {
        if e.mode == GITLINK {
            index.insert(String::from_utf8_lossy(&e.path).into_owned(), e.id);
        }
    }
    let src = if files {
        index.clone()
    } else {
        let commit = repo.revparse_single(&format!("{}^0", rev.as_deref().unwrap_or("HEAD")));
        match commit.and_then(|c| c.peel_to_tree()) {
            Ok(tree) => tree_gitlinks(&tree)?,
            Err(_) => BTreeMap::new(),
        }
    };
    let checked_out = |path: &str| {
        Repository::open(top.join(path))
            .ok()
            .and_then(|r| r.head().ok()?.target())
    };
    let dst: BTreeMap<String, Oid> = if cached {
        index
    } else {
        index
            .into_iter()
            .map(|(p, id)| {
                let now = checked_out(&p).unwrap_or(id);
                (p, now)
            })
            .collect()
    };
    let mut all: Vec<&String> = src.keys().chain(dst.keys()).collect();
    all.sort();
    all.dedup();
    for path in all {
        if !paths.is_empty() && !crate::pathspec_matches(&paths, path) {
            continue;
        }
        let (from, to) = (src.get(path).copied(), dst.get(path).copied());
        if from == to {
            continue;
        }
        for line in summarize(repo, &top, path, from, to, limit) {
            report(OpProgress::Line(line));
        }
    }
    Ok(())
}

/// The gitlinks in `tree`, by path.
fn tree_gitlinks(tree: &git2::Tree) -> Result<BTreeMap<String, Oid>, GitError> {
    let mut out = BTreeMap::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
        if e.filemode() as u32 == GITLINK {
            out.insert(format!("{root}{}", e.name().unwrap_or_default()), e.id());
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(out)
}

/// One submodule's block of `submodule summary`.
fn summarize(
    repo: &Repository,
    top: &Path,
    path: &str,
    from: Option<Oid>,
    to: Option<Oid>,
    limit: usize,
) -> Vec<String> {
    let sub = Repository::open(top.join(path)).ok().or_else(|| {
        let name = repo.find_submodule(path).ok()?.name().ok()?.to_owned();
        Repository::open(repo.path().join("modules").join(name)).ok()
    });
    let has = |id: Option<Oid>| {
        id.is_none_or(|id| sub.as_ref().is_some_and(|s| s.find_commit(id).is_ok()))
    };
    let abbrev = |id: Option<Oid>| match (id, &sub) {
        (None, _) => "0000000".to_owned(),
        (Some(id), Some(s)) => s
            .find_object(id, None)
            .ok()
            .and_then(|o| o.short_id().ok())
            .and_then(|b| b.as_str().ok().map(str::to_owned))
            .unwrap_or_else(|| id.to_string()[..7].to_owned()),
        (Some(id), None) => id.to_string()[..7].to_owned(),
    };
    let head = format!("* {path} {}...{}", abbrev(from), abbrev(to));
    let missing: Vec<String> = [from, to]
        .into_iter()
        .flatten()
        .filter(|id| !has(Some(*id)))
        .map(|id| id.to_string())
        .collect();
    let mut out = Vec::new();
    match (missing.as_slice(), &sub) {
        ([], Some(sub)) => {
            let commits = log(sub, from, to).unwrap_or_default();
            out.push(format!("{head} ({}):", commits.len()));
            let shown: Vec<&(char, String)> = match (from, to) {
                (Some(_), Some(_)) if limit > 0 => commits.iter().take(limit).collect(),
                (Some(_), Some(_)) => commits.iter().collect(),
                _ => commits.iter().take(1).collect(),
            };
            out.extend(shown.iter().map(|(m, s)| format!("  {m} {s}")));
        }
        ([one], _) => {
            out.push(format!("{head}:"));
            out.push(format!("  Warn: {path} doesn't contain commit {one}"));
        }
        (both, _) => {
            out.push(format!("{head}:"));
            out.push(format!(
                "  Warn: {path} doesn't contain commits {}",
                both.join(" and ")
            ));
        }
    }
    out.push(String::new());
    out
}

/// `git log --first-parent --pretty='%m %s' from...to`: `>` for commits only
/// `to` has, `<` for those only `from` has. One side alone lists its history.
/// Walked in git's order: newest first, a tie going to the commit queued first.
fn log(
    sub: &Repository,
    from: Option<Oid>,
    to: Option<Oid>,
) -> Result<Vec<(char, String)>, GitError> {
    let bases: Vec<Oid> = match (from, to) {
        (Some(a), Some(b)) => sub
            .merge_bases(a, b)
            .map(|b| b.iter().copied().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let hidden = |id: Oid| {
        bases
            .iter()
            .any(|&b| b == id || sub.graph_descendant_of(b, id).unwrap_or(false))
    };
    let mut queue: Vec<(char, git2::Commit)> = Vec::new();
    fn push<'r>(queue: &mut Vec<(char, git2::Commit<'r>)>, mark: char, c: git2::Commit<'r>) {
        let at = queue
            .iter()
            .position(|(_, q)| q.time().seconds() < c.time().seconds())
            .unwrap_or(queue.len());
        queue.insert(at, (mark, c));
    }
    for (mark, id) in [('<', from), ('>', to)] {
        if let Some(id) = id {
            push(&mut queue, mark, sub.find_commit(id)?);
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    while !queue.is_empty() {
        let (mark, c) = queue.remove(0);
        if !seen.insert(c.id()) || hidden(c.id()) {
            continue;
        }
        out.push((mark, c.summary().ok().flatten().unwrap_or("").to_owned()));
        if let Ok(p) = c.parent(0) {
            push(&mut queue, mark, p);
        }
    }
    Ok(out)
}

/// `git submodule absorbgitdirs`: move each submodule's `.git` folder into the
/// superproject's `.git/modules/<name>` and link it from there, recursively.
pub fn absorb(
    repo: &Repository,
    prefix: &str,
    paths: &[String],
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let top = repo.workdir().unwrap_or(repo.path()).to_path_buf();
    for sm in repo.submodules()? {
        let path = sm.path().to_string_lossy().into_owned();
        if !paths.is_empty() && !crate::pathspec_matches(paths, &path) {
            continue;
        }
        let wt = top.join(&path);
        let dotgit = wt.join(".git");
        if dotgit.is_dir() {
            let dest = repo
                .path()
                .join("modules")
                .join(sm.name().unwrap_or(path.as_str()));
            if dest.exists() {
                return Err(GitError::Other(format!(
                    "refusing to move '{}' into an existing git dir",
                    dotgit.display()
                )));
            }
            std::fs::create_dir_all(dest.parent().unwrap_or(&dest))?;
            let old = std::fs::canonicalize(&dotgit)?;
            std::fs::rename(&dotgit, &dest)?;
            let dest = std::fs::canonicalize(&dest)?;
            link(&std::fs::canonicalize(&wt)?, &dest)?;
            report(OpProgress::Line(format!(
                "Migrating git directory of '{prefix}{path}' from\n'{}' to\n'{}'",
                old.display(),
                dest.display()
            )));
        }
        if let Ok(sub) = Repository::open(&wt) {
            absorb(&sub, &format!("{prefix}{path}/"), &[], report)?;
        }
    }
    Ok(())
}

/// Point the work tree `wt` at the git dir `gitdir` and back, with relative
/// paths, as git's `connect_work_tree_and_git_dir` does.
fn link(wt: &Path, gitdir: &Path) -> Result<(), GitError> {
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", relative(wt, gitdir).display()),
    )?;
    git2::Config::open(&gitdir.join("config"))?
        .set_str("core.worktree", &relative(gitdir, wt).to_string_lossy())?;
    Ok(())
}

/// `to` relative to the folder `from`; both absolute.
fn relative(from: &Path, to: &Path) -> PathBuf {
    let (a, b): (Vec<Component>, Vec<Component>) =
        (from.components().collect(), to.components().collect());
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out: PathBuf = a[common..].iter().map(|_| "..").collect();
    out.extend(&b[common..]);
    out
}

/// `git submodule add --name <name>`: clone into `.git/modules/<name>` with
/// the work tree at `path`, then record it under that name.
pub fn add_named(
    repo: &Repository,
    url: &str,
    path: &str,
    name: &str,
    branch: Option<&str>,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let top = repo
        .workdir()
        .ok_or_else(|| GitError::Bare(repo.path().to_path_buf()))?
        .to_path_buf();
    let wt = top.join(path);
    let gitdir = repo.path().join("modules").join(name);
    if gitdir.exists() {
        return Err(GitError::Other(format!(
            "A git directory for '{name}' is found locally"
        )));
    }
    report(OpProgress::Line(format!(
        "Cloning into '{}'...",
        wt.display()
    )));
    std::fs::create_dir_all(&wt)?;
    let mut opts = git2::RepositoryInitOptions::new();
    opts.workdir_path(&wt).no_reinit(true).no_dotgit_dir(true);
    let sub = Repository::init_opts(&gitdir, &opts)?;
    link(
        &std::fs::canonicalize(&wt)?,
        &std::fs::canonicalize(&gitdir)?,
    )?;
    let mut remote = sub.remote("origin", url)?;
    let default = {
        let conn = remote.connect_auth(git2::Direction::Fetch, None, None)?;
        conn.default_branch().ok().and_then(|b| {
            b.as_str()
                .ok()
                .map(|s| s.trim_start_matches("refs/heads/").to_owned())
        })
    };
    remote.fetch(&[] as &[&str], None, None)?;
    let chosen = branch
        .map(str::to_owned)
        .or(default)
        .ok_or_else(|| GitError::Other(format!("{url} has no branch to check out")))?;
    sub.reference_symbolic(
        "refs/remotes/origin/HEAD",
        &format!("refs/remotes/origin/{chosen}"),
        true,
        "clone",
    )?;
    let commit = sub.find_commit(sub.refname_to_id(&format!("refs/remotes/origin/{chosen}"))?)?;
    sub.branch(&chosen, &commit, true)?
        .set_upstream(Some(&format!("origin/{chosen}")))?;
    sub.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().force()))?;
    sub.set_head(&format!("refs/heads/{chosen}"))?;

    let mut modules = git2::Config::open(&top.join(".gitmodules"))?;
    modules.set_str(&format!("submodule.{name}.path"), path)?;
    modules.set_str(&format!("submodule.{name}.url"), url)?;
    if let Some(b) = branch {
        modules.set_str(&format!("submodule.{name}.branch"), b)?;
    }
    let mut config = repo.config()?.open_level(git2::ConfigLevel::Local)?;
    config.set_str(&format!("submodule.{name}.url"), url)?;
    config.set_bool(&format!("submodule.{name}.active"), true)?;
    let mut index = repo.index()?;
    index.add_path(Path::new(".gitmodules"))?;
    index.add(&git2::IndexEntry {
        ctime: git2::IndexTime::new(0, 0),
        mtime: git2::IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: GITLINK,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: commit.id(),
        flags: path.len().min(0xfff) as u16,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    })?;
    index.write()?;
    Ok(())
}
