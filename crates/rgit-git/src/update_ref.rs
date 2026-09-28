//! `git update-ref`'s all-or-nothing transactions, with its `symref-*`
//! commands and `--batch-updates` rejections.

use std::collections::HashSet;

use git2::{Oid, Repository};

use crate::{GitError, RefUpdate};

const ZERO: &str = "0000000000000000000000000000000000000000";

enum Change {
    Oid(Oid),
    Symbolic(String),
    Delete,
}

/// See [`crate::GitBackend::update_refs`].
pub(crate) fn update_refs(
    repo: &Repository,
    updates: &[RefUpdate],
    message: Option<&str>,
    no_deref: bool,
    create_reflog: bool,
    check_only: bool,
    batch: bool,
) -> Result<Vec<String>, GitError> {
    let zero = |v: &str| v.chars().all(|c| c == '0');
    // A full object id stands for itself, existing or not; anything else
    // is a revision.
    let resolve = |v: &str| -> Result<Oid, GitError> {
        if v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(Oid::from_str(v)?);
        }
        let obj = repo.revparse_single(v)?;
        Ok(obj.peel_to_commit().map_or(obj.id(), |c| c.id()))
    };
    let head = repo
        .find_reference("HEAD")
        .ok()
        .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned));
    let mut seen = HashSet::new();
    let mut plan = Vec::new();
    let mut rejected = Vec::new();
    for u in updates {
        let deref = !(no_deref || u.no_deref || u.symref);
        let mut name = u.name.clone();
        if !seen.insert(name.clone()) {
            return Err(GitError::Other(format!(
                "multiple updates for ref '{name}' not allowed"
            )));
        }
        while deref
            && let Some(target) = repo
                .find_reference(&name)
                .ok()
                .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
        {
            name = target;
        }
        if name != u.name && !seen.insert(name.clone()) {
            return Err(GitError::Other(format!(
                "multiple updates for '{name}' (including one via symref '{}') are not allowed",
                u.name
            )));
        }
        let old = u.old.as_deref().map(resolve).transpose()?;
        let new = match (&u.new_target, u.new.as_deref()) {
            _ if u.verify => None,
            (Some(t), _) => Some(Change::Symbolic(t.clone())),
            (None, Some(v)) if !zero(v) => Some(Change::Oid(resolve(v)?)),
            _ => Some(Change::Delete),
        };
        let reference = repo.find_reference(&name).ok();
        let exists = reference.is_some();
        let target = reference
            .as_ref()
            .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned));
        let current = repo.refname_to_id(&name).ok();
        let lock = format!("cannot lock ref '{name}'");
        // The reason git's --batch-updates gives, and the message it dies with.
        let failure: Option<(&str, String)> = if let Some(want) = &u.old_target {
            match &target {
                _ if !exists => Some((
                    "reference does not exist",
                    format!("{lock}: unable to resolve reference '{name}'"),
                )),
                None => Some((
                    "expected symref but found regular ref",
                    format!("{lock}: expected symref with target '{want}': but is a regular ref"),
                )),
                Some(t) if t != want => Some((
                    "incorrect old value provided",
                    format!("{lock}: is at {t} but expected {want}"),
                )),
                _ => None,
            }
        } else if let Some(want) = old.filter(|o| !o.is_zero()) {
            match current {
                None => Some((
                    "reference does not exist",
                    format!("{lock}: unable to resolve reference '{name}'"),
                )),
                Some(c) if c != want => Some((
                    "incorrect old value provided",
                    format!("{lock}: is at {c} but expected {want}"),
                )),
                _ => None,
            }
        } else if (old.is_some() || (u.symref && u.verify)) && exists {
            Some((
                "reference already exists",
                format!("{lock}: reference already exists"),
            ))
        } else if !exists && matches!(new, Some(Change::Oid(_) | Change::Symbolic(_))) {
            conflict(repo, &name).map(|other| {
                (
                    "refname conflict",
                    format!("{lock}: '{other}' exists; cannot create '{name}'"),
                )
            })
        } else {
            None
        };
        if let Some((reason, message)) = failure {
            if !batch {
                return Err(GitError::Other(message));
            }
            let new_col = match &new {
                None => "(null)".to_owned(),
                Some(Change::Oid(id)) => id.to_string(),
                Some(_) => ZERO.to_owned(),
            };
            let old_col = match old {
                Some(o) => o.to_string(),
                None if u.symref => ZERO.to_owned(),
                None => "(null)".to_owned(),
            };
            rejected.push(format!("rejected {name} {new_col} {old_col} {reason}"));
            // A change to the branch HEAD is on is also one to HEAD's log.
            if deref && name != "HEAD" && head.as_deref() == Some(name.as_str()) {
                rejected.push(format!("rejected HEAD {new_col} {old_col} {reason}"));
            }
            continue;
        }
        match new {
            // Deleting a ref that does not exist is a no-op.
            Some(Change::Delete) if !exists => {}
            Some(change) => plan.push((name, change)),
            None => {}
        }
    }
    if check_only {
        return Ok(rejected);
    }
    let msg = message.unwrap_or("update-ref");
    let sig = repo.signature()?;
    let mut tx = repo.transaction()?;
    for (name, _) in &plan {
        tx.lock_ref(name)?;
    }
    for (name, change) in &plan {
        match change {
            Change::Oid(id) => tx.set_target(name, *id, Some(&sig), msg)?,
            Change::Symbolic(t) => tx.set_symbolic_target(name, t, Some(&sig), msg)?,
            Change::Delete => tx.remove(name)?,
        }
    }
    tx.commit()?;
    for (name, change) in &plan {
        let logs = repo.path().join("logs").join(name);
        if let (true, Change::Oid(id), false) = (create_reflog, change, logs.exists()) {
            let mut log = repo.reflog(name)?;
            log.append(*id, &sig, Some(msg))?;
            log.write()?;
        }
    }
    Ok(rejected)
}

/// A ref that keeps `name` from being created: one of its leading folders
/// is a ref, or it is a folder of refs.
fn conflict(repo: &Repository, name: &str) -> Option<String> {
    let mut at = 0;
    while let Some(i) = name[at..].find('/') {
        at += i;
        let prefix = &name[..at];
        if repo.find_reference(prefix).is_ok() {
            return Some(prefix.to_owned());
        }
        at += 1;
    }
    repo.references_glob(&format!("{name}/*"))
        .ok()?
        .flatten()
        .find_map(|r| r.name().ok().map(str::to_owned))
}
