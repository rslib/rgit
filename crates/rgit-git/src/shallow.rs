//! Shallow history built by hand, for what libgit2's transports cannot do:
//! the commits a `--depth`, `--deepen` or `--shallow-since` fetch keeps,
//! copied from a local source repository, and the `shallow` file that marks
//! where the history stops.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;

use git2::{ObjectType, Oid, Repository};

use crate::error::GitError;

/// How far back from the fetched tips history reaches.
pub enum Cut {
    /// The commits within this many of a tip.
    Depth(usize),
    /// Everything new, and this many commits past the current boundary.
    Deepen(usize),
    /// The commits made at or after this unix time.
    Since(i64),
    /// All of it.
    Full,
}

/// The commits `repo` marks shallow.
pub fn roots(repo: &Repository) -> Vec<Oid> {
    std::fs::read_to_string(repo.commondir().join("shallow"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| Oid::from_str(l.trim()).ok())
        .collect()
}

/// Mark `ids` shallow, removing the file when there are none, as git does.
pub fn write_roots(repo: &Repository, ids: &HashSet<Oid>) -> Result<(), GitError> {
    let path = repo.commondir().join("shallow");
    if ids.is_empty() {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    let mut lines: Vec<String> = ids.iter().map(|id| format!("{id}\n")).collect();
    lines.sort();
    std::fs::write(path, lines.concat())?;
    Ok(())
}

/// The commits of `repo` reachable from `tips` that `cut` keeps, and the
/// kept ones that end the history: those at the depth limit (which git marks
/// shallow, parents or not) and those a parent of which is too old.
/// `boundary` is the history's current shallow boundary, which `Cut::Deepen`
/// counts from; a walk without a limit stops at the commits `have` has.
pub fn select(
    repo: &Repository,
    tips: &[Oid],
    cut: &Cut,
    boundary: &HashSet<Oid>,
    have: Option<&git2::Odb<'_>>,
) -> Result<(HashSet<Oid>, HashSet<Oid>), GitError> {
    // Each commit's budget: how many commits from it down may still be kept.
    let mut best: HashMap<Oid, usize> = HashMap::new();
    let start = match cut {
        Cut::Depth(d) => *d,
        _ => usize::MAX,
    };
    let mut queue: VecDeque<(Oid, usize)> = tips.iter().map(|&t| (t, start)).collect();
    if !matches!(cut, Cut::Depth(_)) {
        queue.extend(boundary.iter().map(|&b| (b, usize::MAX)));
    }
    let mut too_old = HashSet::new();
    while let Some((id, budget)) = queue.pop_front() {
        if best.get(&id).is_some_and(|&b| b >= budget) {
            continue;
        }
        let Ok(commit) = repo.find_commit(id) else {
            continue;
        };
        best.insert(id, budget);
        let next = match cut {
            Cut::Deepen(n) if boundary.contains(&id) => *n,
            _ if budget == usize::MAX => budget,
            _ => budget - 1,
        };
        // What is here already is here with its history.
        let known =
            next == usize::MAX && !boundary.contains(&id) && have.is_some_and(|db| db.exists(id));
        if next == 0 || known {
            continue;
        }
        for parent in commit.parent_ids() {
            if let Cut::Since(t) = cut
                && !repo
                    .find_commit(parent)
                    .is_ok_and(|p| p.committer().when().seconds() >= *t)
            {
                too_old.insert(id);
                continue;
            }
            queue.push_back((parent, next));
        }
    }
    let cutoff = best
        .iter()
        .filter(|&(id, &b)| b == 1 || too_old.contains(id))
        .map(|(id, _)| *id)
        .collect();
    Ok((best.into_keys().collect(), cutoff))
}

/// Copy `commits` (with their trees) and `tags` from `src` into `dst`,
/// leaving out what `dst` already has.
pub fn copy(
    src: &Repository,
    dst: &Repository,
    commits: &HashSet<Oid>,
    tags: &[Oid],
) -> Result<(), GitError> {
    let odb = dst.odb()?;
    let mut pack = src.packbuilder()?;
    for &id in commits {
        if !odb.exists(id) {
            pack.insert_commit(id)?;
        }
    }
    for &id in tags {
        if !odb.exists(id) {
            pack.insert_recursive(id, None)?;
        }
    }
    write_pack(&mut pack, &odb)
}

/// Write what `pack` holds into `odb` as one pack.
pub fn write_pack(pack: &mut git2::PackBuilder<'_>, odb: &git2::Odb<'_>) -> Result<(), GitError> {
    if pack.object_count() == 0 {
        return Ok(());
    }
    let mut writer = odb.packwriter()?;
    let mut failed = None;
    pack.foreach(|buf| match writer.write_all(buf) {
        Ok(()) => true,
        Err(e) => {
            failed = Some(e);
            false
        }
    })?;
    if let Some(e) = failed {
        return Err(e.into());
    }
    writer.commit()?;
    Ok(())
}

/// The parents `id` names in its object, whatever the shallow file says.
fn raw_parents(repo: &Repository, id: Oid) -> Vec<Oid> {
    let Ok(odb) = repo.odb() else {
        return Vec::new();
    };
    let Ok(obj) = odb.read(id) else {
        return Vec::new();
    };
    if obj.kind() != ObjectType::Commit {
        return Vec::new();
    }
    String::from_utf8_lossy(obj.data())
        .lines()
        .take_while(|l| !l.is_empty())
        .filter_map(|l| Oid::from_str(l.strip_prefix("parent ")?).ok())
        .collect()
}

/// Of `candidates`, the commits `repo` has without all of their parents.
pub fn incomplete(repo: &Repository, candidates: &HashSet<Oid>) -> HashSet<Oid> {
    let Ok(odb) = repo.odb() else {
        return HashSet::new();
    };
    candidates
        .iter()
        .copied()
        .filter(|&id| odb.exists(id) && raw_parents(repo, id).iter().any(|p| !odb.exists(*p)))
        .collect()
}
