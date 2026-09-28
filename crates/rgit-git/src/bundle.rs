//! `git bundle`: a ref list plus a packfile in one file, made and read with
//! libgit2's pack builder and pack writer.

use crate::rev::RevParse;
use std::path::Path;

use git2::{Oid, Repository};

use crate::GitError;

/// A bundle's header: the commits it needs and the refs it carries.
#[derive(Debug, Clone, Default)]
pub struct BundleHeader {
    /// `(id, comment)` of each commit the receiver must already have.
    pub prerequisites: Vec<(String, String)>,
    /// `(id, ref name)` of each ref.
    pub refs: Vec<(String, String)>,
}

/// Read a bundle file: its header and where the pack starts.
fn read(path: &Path) -> Result<(BundleHeader, Vec<u8>, usize), GitError> {
    let data = std::fs::read(path)
        .map_err(|e| GitError::Other(format!("could not open '{}': {e}", path.display())))?;
    let bad = || {
        GitError::Other(format!(
            "'{}' does not look like a v2 or v3 bundle file",
            path.display()
        ))
    };
    let mut header = BundleHeader::default();
    let mut pos = 0;
    let mut first = true;
    loop {
        let end = data[pos..]
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(bad)?
            + pos;
        let line = String::from_utf8_lossy(&data[pos..end]).into_owned();
        pos = end + 1;
        if first {
            if line != "# v2 git bundle" && line != "# v3 git bundle" {
                return Err(bad());
            }
            first = false;
            continue;
        }
        if line.is_empty() {
            break;
        }
        if line.starts_with('@') {
            if line != "@object-format=sha1" && !line.starts_with("@filter=") {
                return Err(GitError::Other(format!(
                    "unsupported bundle capability {line}"
                )));
            }
            continue;
        }
        let (id, rest) = line.split_once(' ').unwrap_or((&line, ""));
        match id.strip_prefix('-') {
            Some(id) => header.prerequisites.push((id.to_owned(), rest.to_owned())),
            None => header.refs.push((id.to_owned(), rest.to_owned())),
        }
    }
    Ok((header, data, pos))
}

/// The header of the bundle at `path` (needs no repository).
pub fn bundle_header(path: &Path) -> Result<BundleHeader, GitError> {
    Ok(read(path)?.0)
}

/// Write a bundle of what `args` (rev-list style: refs, `^rev`, `a..b`,
/// `--all`, `--branches`, `--tags`, `--remotes`) select. Returns how many
/// refs it records.
pub(crate) fn create(repo: &Repository, path: &Path, args: &[String]) -> Result<usize, GitError> {
    let mut walk = repo.revwalk()?;
    let mut refs: Vec<(Oid, String)> = Vec::new();
    let mut positive = false;
    let add_refs = |prefix: &str, refs: &mut Vec<(Oid, String)>| -> Result<(), GitError> {
        let mut found = Vec::new();
        for r in repo.references()? {
            let r = r?;
            let Some(name) = r.name().ok().filter(|n| n.starts_with(prefix)) else {
                continue;
            };
            if let Some(id) = r.target() {
                found.push((id, name.to_owned()));
            }
        }
        found.sort_by(|a, b| a.1.cmp(&b.1));
        refs.extend(found);
        Ok(())
    };
    for arg in args {
        match arg.as_str() {
            "--all" => {
                add_refs("refs/", &mut refs)?;
                if let Some(id) = repo.head().ok().and_then(|h| h.target()) {
                    refs.push((id, "HEAD".to_owned()));
                }
            }
            "--branches" => add_refs("refs/heads/", &mut refs)?,
            "--tags" => add_refs("refs/tags/", &mut refs)?,
            "--remotes" => add_refs("refs/remotes/", &mut refs)?,
            a if a.starts_with('^') => {
                walk.hide(repo.rev_single(&a[1..])?.peel_to_commit()?.id())?
            }
            a if a.contains("..") => {
                let (from, to) = a.split_once("..").unwrap_or((a, ""));
                let to = if to.is_empty() { "HEAD" } else { to };
                if !from.is_empty() {
                    walk.hide(repo.rev_single(from)?.peel_to_commit()?.id())?;
                }
                refs.extend(ref_of(repo, to)?);
            }
            a => refs.extend(ref_of(repo, a)?),
        }
    }
    for (id, _) in &refs {
        let commit = repo.find_object(*id, None)?.peel_to_commit()?;
        walk.push(commit.id())?;
        positive = true;
    }
    let included: Vec<Oid> = walk.collect::<Result<_, _>>()?;
    if !positive || included.is_empty() {
        return Err(GitError::Other(
            "Refusing to create empty bundle.".to_owned(),
        ));
    }
    let set: std::collections::HashSet<Oid> = included.iter().copied().collect();
    let mut prereqs = Vec::new();
    for id in &included {
        for parent in repo.find_commit(*id)?.parents() {
            if !set.contains(&parent.id()) && !prereqs.iter().any(|(p, _)| *p == parent.id()) {
                prereqs.push((
                    parent.id(),
                    parent.summary().ok().flatten().unwrap_or("").to_owned(),
                ));
            }
        }
    }
    let mut pack = repo.packbuilder()?;
    let mut walk = repo.revwalk()?;
    for id in &included {
        walk.push(*id)?;
    }
    for (p, _) in &prereqs {
        walk.hide(*p)?;
    }
    pack.insert_walk(&mut walk)?;
    for (id, _) in &refs {
        if repo.find_tag(*id).is_ok() {
            pack.insert_object(*id, None)?;
        }
    }
    let mut buf = git2::Buf::new();
    pack.write_buf(&mut buf)?;
    let mut out = b"# v2 git bundle\n".to_vec();
    for (id, subject) in &prereqs {
        out.extend(format!("-{id} {subject}\n").into_bytes());
    }
    for (id, name) in &refs {
        out.extend(format!("{id} {name}\n").into_bytes());
    }
    out.push(b'\n');
    out.extend_from_slice(&buf);
    std::fs::write(path, out)?;
    Ok(refs.len())
}

/// The ref `rev` names in full (`HEAD` stays `HEAD`), with what it points
/// at; nothing for a bare commit id.
fn ref_of(repo: &Repository, rev: &str) -> Result<Option<(Oid, String)>, GitError> {
    let (obj, reference) = repo.revparse_ext(rev)?;
    Ok(match reference {
        _ if rev == "HEAD" => Some((obj.id(), "HEAD".to_owned())),
        Some(r) => r.name().ok().map(|n| (obj.id(), n.to_owned())),
        None => None,
    })
}

/// The prerequisite commits `repo` lacks.
pub(crate) fn missing(repo: &Repository, header: &BundleHeader) -> Vec<(String, String)> {
    header
        .prerequisites
        .iter()
        .filter(|(id, _)| Oid::from_str(id).map_or(true, |id| repo.find_commit(id).is_err()))
        .cloned()
        .collect()
}

/// Check the bundle at `path` against `repo`: its header, and the commits it
/// needs that are missing.
pub(crate) fn verify(
    repo: &Repository,
    path: &Path,
) -> Result<(BundleHeader, Vec<(String, String)>), GitError> {
    let (header, ..) = read(path)?;
    let missing = missing(repo, &header);
    Ok((header, missing))
}

/// Store the bundle's objects in `repo` (refs are not touched) and return
/// its refs.
pub(crate) fn unbundle(repo: &Repository, path: &Path) -> Result<Vec<(String, String)>, GitError> {
    let (header, data, start) = read(path)?;
    let missing = missing(repo, &header);
    if !missing.is_empty() {
        return Err(lacks(&missing));
    }
    let odb = repo.odb()?;
    let mut writer = odb.packwriter()?;
    std::io::Write::write_all(&mut writer, &data[start..])?;
    writer.commit()?;
    Ok(header.refs)
}

/// git's error for missing prerequisites.
pub(crate) fn lacks(missing: &[(String, String)]) -> GitError {
    let mut msg = String::from("Repository lacks these prerequisite commits:");
    for (id, comment) in missing {
        msg.push_str(&format!("\n{id} {comment}"));
    }
    GitError::Other(msg)
}
