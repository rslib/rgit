//! `git replay`: replay commits onto a new base in memory, printing the ref
//! updates for `git update-ref --stdin` instead of making them.

use crate::rev::RevParse;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use git2::{ObjectType, Oid, Repository};

use crate::GitError;
use crate::fast_export::{dwim_ref, parse_revs, topo_walk};

/// `git replay`'s options.
pub struct ReplayOpts {
    pub onto: Option<String>,
    pub advance: Option<String>,
    pub contained: bool,
    pub revs: Vec<String>,
}

fn fatal(msg: &str) -> GitError {
    GitError::Other(msg.to_owned())
}

/// Replay the range; the `update <ref> <new> <old>` lines and whether every
/// commit applied cleanly.
pub fn replay(git_dir: &Path, o: &ReplayOpts) -> Result<(String, bool), GitError> {
    let repo = Repository::open(git_dir)?;
    let revs = parse_revs(&repo, &o.revs)?;
    if revs.tips.is_empty() {
        return Err(fatal("need some commits to replay"));
    }
    let commit_of = |s: &str| {
        repo.rev_single(s)
            .and_then(|o| o.peel_to_commit())
            .map(|c| c.id())
    };
    let (onto, advance) = match (&o.onto, &o.advance) {
        (Some(onto), None) => (
            commit_of(onto)
                .map_err(|_| fatal(&format!("Failed to resolve '{onto}' as a valid revision.")))?,
            None,
        ),
        (None, Some(b)) => {
            let full = dwim_ref(&repo, b)
                .filter(|f| f.starts_with("refs/"))
                .ok_or_else(|| fatal("argument to --advance must be a reference"))?;
            if revs.tips.len() > 1 {
                return Err(fatal(
                    "cannot advance target with multiple sources because ordering would be ill-defined",
                ));
            }
            (repo.refname_to_id(&full)?, Some(full))
        }
        _ => unreachable!("the caller checks --onto and --advance"),
    };
    let update: HashSet<String> = revs.tips.iter().filter_map(|t| t.1.clone()).collect();
    let mut branches: HashMap<Oid, Vec<String>> = HashMap::new();
    for r in repo.references_glob("refs/heads/*")? {
        let r = r?;
        if let (Some(name), Some(id)) = (r.name().ok(), r.target()) {
            branches.entry(id).or_default().push(name.to_owned());
        }
    }
    let tips: Vec<(String, Oid)> = revs.tips.iter().map(|t| (t.0.clone(), t.2)).collect();
    let (walked, _) = topo_walk(&repo, &tips, &revs.hide, &mut HashMap::new(), None)?;
    let committer = crate::plumbing::ident(&repo, true)?;
    let mut replayed: HashMap<Oid, Oid> = HashMap::new();
    let mut out = String::new();
    let mut last = onto;
    let mut clean = true;
    for w in &walked {
        let [base] = w.parents[..] else {
            return Err(fatal(if w.parents.is_empty() {
                "Replaying down to root commit is not supported yet!"
            } else {
                "Replaying merge commits is not supported yet!"
            }));
        };
        let new_base = replayed.get(&base).copied().unwrap_or(onto);
        let pick = repo.find_commit(w.id)?;
        let mut index = repo.merge_trees(
            &repo.find_commit(base)?.tree()?,
            &repo.find_commit(new_base)?.tree()?,
            &pick.tree()?,
            None,
        )?;
        if index.has_conflicts() {
            clean = false;
            break;
        }
        let tree = index.write_tree_to(&repo)?;
        let id = rewrite(&repo, w.id, tree, new_base, &committer)?;
        replayed.insert(w.id, id);
        last = id;
        if advance.is_some() {
            continue;
        }
        let mut names = branches.get(&w.id).cloned().unwrap_or_default();
        names.sort();
        for name in names.iter().rev() {
            if o.contained || update.contains(name) {
                out.push_str(&format!("update {name} {id} {}\n", w.id));
            }
        }
    }
    if clean && let Some(name) = advance {
        out.push_str(&format!("update {name} {last} {onto}\n"));
    }
    Ok((out, clean))
}

/// `orig` rewritten with `tree` on `parent`: the same author, message and
/// extra headers (bar signatures), committed now.
fn rewrite(
    repo: &Repository,
    orig: Oid,
    tree: Oid,
    parent: Oid,
    committer: &str,
) -> Result<Oid, GitError> {
    let odb = repo.odb()?;
    let raw = odb.read(orig)?;
    let buf = raw.data();
    let at = buf
        .windows(2)
        .position(|p| p == b"\n\n")
        .map_or(buf.len(), |i| i + 1);
    let (head, message) = (&buf[..at], buf.get(at + 1..).unwrap_or_default());
    let mut author = Vec::new();
    let mut extra = Vec::new();
    let mut skip = false;
    for line in head.split_inclusive(|&b| b == b'\n') {
        if line.starts_with(b" ") {
            if !skip {
                extra.extend_from_slice(line);
            }
            continue;
        }
        let key = line.split(|&b| b == b' ').next().unwrap_or_default();
        skip = true;
        match key {
            b"author" => author = line.to_vec(),
            b"tree" | b"parent" | b"committer" | b"gpgsig" | b"gpgsig-sha256" => {}
            _ => {
                skip = false;
                extra.extend_from_slice(line);
            }
        }
    }
    let mut out = format!("tree {tree}\nparent {parent}\n").into_bytes();
    out.extend_from_slice(&author);
    out.extend_from_slice(format!("committer {committer}\n").as_bytes());
    out.extend_from_slice(&extra);
    out.push(b'\n');
    out.extend_from_slice(message);
    Ok(repo.odb()?.write(ObjectType::Commit, &out)?)
}
