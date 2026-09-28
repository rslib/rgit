//! `git stash push -- <pathspec>`: the stash commits git builds for a
//! pathspec, and the reset of only those paths.

use crate::pathspec::Pathspec;
use std::path::Path;

use git2::build::CheckoutBuilder;
use git2::{DiffOptions, IndexEntry, IndexTime, Oid, Repository, Status};

use crate::error::GitError;

pub struct Opts<'a> {
    pub message: Option<&'a str>,
    pub untracked: bool,
    pub all: bool,
    pub keep_index: bool,
}

/// Stash the changes under `paths` as git does, returning git's report line.
pub fn push_paths(repo: &Repository, paths: &[String], o: &Opts) -> Result<String, GitError> {
    let spec = Pathspec::new(paths.iter())?;
    let hit = |p: &str| spec.matches_path(Path::new(p));
    let head = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .ok_or_else(|| GitError::Other("You do not have the initial commit yet".to_owned()))?;
    let head_tree = head.tree()?;
    let mut index = repo.index()?;

    let mut status = git2::StatusOptions::new();
    status
        .include_untracked(o.untracked || o.all)
        .recurse_untracked_dirs(true)
        .include_ignored(o.all)
        .recurse_ignored_dirs(o.all)
        .include_unmodified(true)
        .exclude_submodules(true);
    if spec.is_plain() {
        for p in paths {
            status.pathspec(p);
        }
    }
    let entries: Vec<(String, Status)> = repo
        .statuses(Some(&mut status))?
        .iter()
        .filter_map(|e| Some((e.path().ok()?.to_owned(), e.status())))
        .filter(|(p, _)| hit(p))
        .collect();
    for p in paths {
        let one = Pathspec::new([p])?;
        if !entries
            .iter()
            .any(|(path, _)| one.matches_path(Path::new(path)))
        {
            return Err(GitError::Other(format!(
                "pathspec '{p}' did not match any file(s) known to git\n\
                 Did you forget to 'git add'?"
            )));
        }
    }
    // Magic pathspecs go on as the paths they match.
    let expanded: Vec<String>;
    let paths = if spec.is_plain() {
        paths
    } else {
        expanded = entries.iter().map(|(p, _)| p.clone()).collect();
        &expanded[..]
    };
    let untracked: Vec<&str> = entries
        .iter()
        .filter(|(_, s)| s.intersects(Status::WT_NEW | Status::IGNORED))
        .map(|(p, _)| p.as_str())
        .collect();
    if entries.iter().all(|(_, s)| *s == Status::CURRENT) {
        return Ok("No local changes to save".to_owned());
    }

    let branch = match repo.head()?.shorthand() {
        Ok("HEAD") | Err(_) => "(no branch)".to_owned(),
        Ok(b) => b.to_owned(),
    };
    let short = repo
        .find_object(head.id(), None)?
        .short_id()?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let on = format!(
        "{branch}: {short} {}",
        head.summary().ok().flatten().unwrap_or("")
    );
    let sig = repo.signature()?;

    let i_tree = repo.find_tree(index.write_tree()?)?;
    let i_commit = repo.find_commit(repo.commit(
        None,
        &sig,
        &sig,
        &format!("index on {on}\n"),
        &i_tree,
        &[&head],
    )?)?;
    let u_commit = if untracked.is_empty() {
        None
    } else {
        let mut u = git2::Index::new()?;
        for p in &untracked {
            u.add(&workdir_entry(repo, p)?)?;
        }
        let tree = repo.find_tree(u.write_tree_to(repo)?)?;
        let id = repo.commit(
            None,
            &sig,
            &sig,
            &format!("untracked files on {on}\n"),
            &tree,
            &[],
        )?;
        Some(repo.find_commit(id)?)
    };

    // The worktree commit: the index, with the pathspec's tracked files as
    // they are in the working tree.
    let mut w = git2::Index::new()?;
    w.read_tree(&i_tree)?;
    let mut dopts = DiffOptions::new();
    crate::pathspec::limit_diff(&mut dopts, paths)?;
    let diff = repo.diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut dopts))?;
    for d in diff.deltas() {
        let Some(path) = d.new_file().path().and_then(|p| p.to_str()) else {
            continue;
        };
        if d.new_file().mode() == git2::FileMode::Commit {
            continue;
        }
        if d.status() == git2::Delta::Deleted {
            w.remove_path(Path::new(path))?;
        } else {
            w.add(&workdir_entry(repo, path)?)?;
        }
    }
    let w_tree = repo.find_tree(w.write_tree_to(repo)?)?;
    let message = match o.message {
        Some(m) => format!("On {branch}: {m}"),
        None => format!("WIP on {on}"),
    };
    let mut parents = vec![&head, &i_commit];
    parents.extend(u_commit.as_ref());
    let w_id = repo.commit(None, &sig, &sig, &format!("{message}\n"), &w_tree, &parents)?;
    repo.reference_ensure_log("refs/stash")?;
    repo.reference("refs/stash", w_id, true, &message)?;

    // Put the pathspec back to HEAD (the index with --keep-index), as git's
    // `add -u` then `apply -R --index` of the staged diff does.
    let target = if o.keep_index { &i_tree } else { &head_tree };
    if !o.keep_index {
        let added: Vec<String> = index
            .iter()
            .filter_map(|e| String::from_utf8(e.path).ok())
            .filter(|p| hit(p) && head_tree.get_path(Path::new(p)).is_err())
            .collect();
        repo.reset_default(Some(head.as_object()), paths.iter())?;
        let workdir = repo.workdir().unwrap_or(repo.path());
        for p in added {
            let _ = std::fs::remove_file(workdir.join(p));
        }
        index.read(true)?;
    }
    let mut checkout = CheckoutBuilder::new();
    checkout.force().update_index(!o.keep_index);
    for p in paths {
        checkout.path(p);
    }
    repo.checkout_tree(target.as_object(), Some(&mut checkout))?;
    let workdir = repo.workdir().unwrap_or(repo.path());
    for p in untracked {
        let file = workdir.join(p);
        let _ = std::fs::remove_file(&file);
        let mut dir = file.parent();
        while let Some(d) = dir.filter(|d| *d != workdir) {
            if std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }
    Ok(format!("Saved working directory and index state {message}"))
}

/// An index entry for the working tree file at `path`, its blob written.
fn workdir_entry(repo: &Repository, path: &str) -> Result<IndexEntry, GitError> {
    let file = repo.workdir().unwrap_or(repo.path()).join(path);
    let meta = std::fs::symlink_metadata(&file)?;
    let (mode, id): (u32, Oid) = if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&file)?;
        (0o120000, repo.blob(target.to_string_lossy().as_bytes())?)
    } else {
        #[cfg(unix)]
        let exec = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let exec = false;
        (
            if exec { 0o100755 } else { 0o100644 },
            repo.blob_path(&file)?,
        )
    };
    Ok(IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        file_size: meta.len() as u32,
        id,
        flags: path.len().min(0xfff) as u16,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    })
}
