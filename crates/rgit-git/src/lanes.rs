//! Lanes: several independent lines of work in one worktree. Uncommitted changes
//! are assigned (file-level in M1) to named lanes, and each lane commits to its
//! own branch, without stashing or switching. State lives in a commit chain
//! under `refs/rgit/lanes` (the op-log pattern), so plain `git` is never blocked
//! and `lanes off` deletes the ref to return to a plain repo. HEAD stays on a
//! normal branch; a lane commit is synthesized in memory and never rewrites the
//! worktree.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use git2::{ApplyOptions, DiffOptions, IndexEntry, IndexTime, Oid, Patch, Repository, Signature};

use crate::change_id;
use crate::error::GitError;
use crate::model::{HunkRef, Lane, LanesState};

const LANES_REF: &str = "refs/rgit/lanes";
const DEFAULT_LANE: &str = "default";

fn other(msg: impl Into<String>) -> GitError {
    GitError::Other(msg.into())
}

/// Whether the lanes overlay is active in this repo.
pub fn active(repo: &Repository) -> bool {
    repo.find_reference(LANES_REF).is_ok()
}

/// Enter lanes mode: record the fork point (HEAD) and a default lane that owns
/// everything unassigned and commits back to the current branch.
pub fn init(repo: &Repository) -> Result<(), GitError> {
    if active(repo) {
        return Err(other("lanes already active; use `lanes off` first"));
    }
    let head = repo
        .head()
        .map_err(|_| other("cannot init lanes: no HEAD yet"))?;
    if !head.is_branch() {
        return Err(other("cannot init lanes on a detached HEAD"));
    }
    let branch = head.shorthand().unwrap_or("").to_owned();
    if branch.is_empty() {
        return Err(other("cannot read the current branch"));
    }
    let base = head.peel_to_commit()?.id().to_string();
    let state = LanesState {
        base,
        lanes: vec![Lane {
            name: DEFAULT_LANE.to_owned(),
            branch,
            paths: Vec::new(),
            hunks: Vec::new(),
            commits: Vec::new(),
            parent: None,
        }],
    };
    save(repo, &state)
}

/// Leave lanes mode: delete the state ref. Lane branches are left in place.
pub fn off(repo: &Repository) -> Result<(), GitError> {
    match repo.find_reference(LANES_REF) {
        Ok(mut r) => {
            r.delete()?;
            Ok(())
        }
        Err(_) => Err(other("lanes are not active")),
    }
}

/// The current lanes state, reconciled in memory against the working tree: owned
/// paths that are no longer changed are dropped, and changed-but-unowned paths
/// fall to the default lane. Does not persist (reads stay cheap); the mutating
/// ops below persist the reconciled state.
pub fn state(repo: &Repository) -> Result<LanesState, GitError> {
    let mut state = load(repo)?.ok_or_else(|| other("lanes are not active"))?;
    reconcile(repo, &mut state)?;
    let base = Oid::from_str(&state.base)?;
    for lane in &mut state.lanes {
        lane.commits = lane_commits(repo, base, &lane.branch)?;
    }
    Ok(state)
}

/// The commits a lane's branch carries above the fork point, newest first, as
/// `(short_id, summary)`. Empty when the branch does not exist or is at base.
fn lane_commits(
    repo: &Repository,
    base: Oid,
    branch: &str,
) -> Result<Vec<(String, String)>, GitError> {
    let Ok(b) = repo.find_branch(branch, git2::BranchType::Local) else {
        return Ok(Vec::new());
    };
    let Some(tip) = b.get().target() else {
        return Ok(Vec::new());
    };
    let mut walk = repo.revwalk()?;
    walk.push(tip)?;
    walk.hide(base)?;
    let mut out = Vec::new();
    for oid in walk {
        let commit = repo.find_commit(oid?)?;
        let short = commit
            .as_object()
            .short_id()
            .ok()
            .and_then(|b| b.as_str().ok().map(str::to_owned))
            .unwrap_or_default();
        let summary = commit.summary().ok().flatten().unwrap_or("").to_owned();
        out.push((short, summary));
        if out.len() >= 50 {
            break;
        }
    }
    Ok(out)
}

/// Create a new empty lane that commits to a same-named branch.
pub fn new_lane(repo: &Repository, name: &str) -> Result<(), GitError> {
    if !git2::Reference::is_valid_name(&format!("refs/heads/{name}")) {
        return Err(other(format!("invalid lane/branch name: {name:?}")));
    }
    let mut state = state(repo)?;
    if state.lanes.iter().any(|l| l.name == name) {
        return Err(other(format!("lane already exists: {name}")));
    }
    state.lanes.push(Lane {
        name: name.to_owned(),
        branch: name.to_owned(),
        paths: Vec::new(),
        hunks: Vec::new(),
        commits: Vec::new(),
        parent: None,
    });
    save(repo, &state)
}

/// Create a new lane stacked on another: its commits build on the parent lane's
/// branch instead of the shared fork point. Also records the git stack config on
/// the lane branch so the existing `restack` moves it when the parent advances.
pub fn stack(repo: &Repository, name: &str, parent: &str) -> Result<(), GitError> {
    if name == parent {
        return Err(other("a lane cannot be stacked on itself"));
    }
    if !git2::Reference::is_valid_name(&format!("refs/heads/{name}")) {
        return Err(other(format!("invalid lane/branch name: {name:?}")));
    }
    let mut state = state(repo)?;
    if state.lanes.iter().any(|l| l.name == name) {
        return Err(other(format!("lane already exists: {name}")));
    }
    let parent_branch = state
        .lanes
        .iter()
        .find(|l| l.name == parent)
        .map(|l| l.branch.clone())
        .ok_or_else(|| other(format!("no such parent lane: {parent}")))?;

    // Record the stack relationship in git config, so `stack`/`restack` see it.
    let mut cfg = repo.config()?;
    cfg.set_str(&format!("branch.{name}.rgit-stack-parent"), &parent_branch)?;
    if let Some(tip) = repo
        .find_branch(&parent_branch, git2::BranchType::Local)
        .ok()
        .and_then(|b| b.get().target())
    {
        cfg.set_str(&format!("branch.{name}.rgit-stack-base"), &tip.to_string())?;
    }

    state.lanes.push(Lane {
        name: name.to_owned(),
        branch: name.to_owned(),
        paths: Vec::new(),
        hunks: Vec::new(),
        commits: Vec::new(),
        parent: Some(parent.to_owned()),
    });
    save(repo, &state)
}

/// Move each stacked lane onto its parent lane's current tip, entirely in the
/// object database (cherry-pick in memory) so the dirty worktree lanes keep is
/// never touched. A conflict on one lane is reported and skipped; the rest still
/// move. This is the worktree-safe counterpart to the checkout-based `restack`.
pub fn restack(repo: &Repository) -> Result<crate::RestackOutcome, GitError> {
    let state = state(repo)?;
    let parents: HashMap<String, String> = state
        .lanes
        .iter()
        .filter_map(|l| l.parent.clone().map(|p| (l.name.clone(), p)))
        .collect();
    if parents.is_empty() {
        return Ok(crate::RestackOutcome::default());
    }
    let branch_of = |name: &str| {
        state
            .lanes
            .iter()
            .find(|l| l.name == name)
            .map(|l| l.branch.clone())
    };
    // Parents before children, so a child restacks onto its already-moved parent.
    let mut order: Vec<String> = parents.keys().cloned().collect();
    order.sort_by_key(|n| lane_depth(n, &parents));

    let mut outcome = crate::RestackOutcome::default();
    for name in order {
        let parent_name = &parents[&name];
        let (Some(lane_branch), Some(parent_branch)) = (branch_of(&name), branch_of(parent_name))
        else {
            continue;
        };
        let Some(new_base) = tip_of(repo, &parent_branch) else {
            continue; // the parent lane has no commits yet
        };
        let base_key = format!("branch.{lane_branch}.rgit-stack-base");
        let record_base = |repo: &Repository| {
            if let Ok(mut c) = repo.config() {
                let _ = c.set_str(&base_key, &new_base.to_string());
            }
        };
        let Some(lane_tip) = tip_of(repo, &lane_branch) else {
            record_base(repo);
            continue; // no commits to replay
        };
        if lane_tip == new_base
            || repo
                .graph_descendant_of(lane_tip, new_base)
                .unwrap_or(false)
        {
            record_base(repo); // already on top of the parent
            continue;
        }
        match replay_onto(repo, lane_tip, new_base) {
            Ok(new_tip) => {
                repo.reference(
                    &format!("refs/heads/{lane_branch}"),
                    new_tip,
                    true,
                    "rgit lane restack",
                )?;
                record_base(repo);
                outcome.restacked.push(format!("{name} -> {parent_name}"));
            }
            Err(GitError::Conflict(_)) => outcome.conflicted.push(name.clone()),
            Err(e) => return Err(e),
        }
    }
    Ok(outcome)
}

fn tip_of(repo: &Repository, branch: &str) -> Option<Oid> {
    repo.find_branch(branch, git2::BranchType::Local)
        .ok()
        .and_then(|b| b.get().target())
}

/// How many stacked lane ancestors a lane has, for ordering parents first.
fn lane_depth(name: &str, parents: &HashMap<String, String>) -> usize {
    let mut depth = 0;
    let mut cursor = name;
    while let Some(parent) = parents.get(cursor) {
        if !parents.contains_key(parent) {
            break;
        }
        depth += 1;
        cursor = parent;
        if depth > 1000 {
            break;
        }
    }
    depth
}

/// Replay `merge_base(lane_tip, new_base)..lane_tip` onto `new_base` in the odb,
/// via cherry-pick, and return the new tip oid. Never touches index or worktree.
fn replay_onto(repo: &Repository, lane_tip: Oid, new_base: Oid) -> Result<Oid, GitError> {
    let fork = repo.merge_base(lane_tip, new_base)?;
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
    walk.push(lane_tip)?;
    walk.hide(fork)?;

    let mut base = repo.find_commit(new_base)?;
    for oid in walk {
        let commit = repo.find_commit(oid?)?;
        let mut index = repo.cherrypick_commit(&commit, &base, 0, None)?;
        if index.has_conflicts() {
            return Err(GitError::Conflict(format!(
                "conflict replaying {}",
                commit.id()
            )));
        }
        let tree = repo.find_tree(index.write_tree_to(repo)?)?;
        let new = repo.commit(
            None,
            &commit.author(),
            &commit.committer(),
            commit.message().unwrap_or(""),
            &tree,
            &[&base],
        )?;
        base = repo.find_commit(new)?;
    }
    Ok(base.id())
}

/// The branch a lane commits to.
pub fn lane_branch(repo: &Repository, name: &str) -> Result<String, GitError> {
    state(repo)?
        .lanes
        .into_iter()
        .find(|l| l.name == name)
        .map(|l| l.branch)
        .ok_or_else(|| other(format!("no such lane: {name}")))
}

/// Rename a lane and its branch. The default lane cannot be renamed.
pub fn rename(repo: &Repository, old: &str, new: &str) -> Result<(), GitError> {
    if old == DEFAULT_LANE {
        return Err(other("the default lane cannot be renamed"));
    }
    if !git2::Reference::is_valid_name(&format!("refs/heads/{new}")) {
        return Err(other(format!("invalid lane/branch name: {new:?}")));
    }
    let mut state = state(repo)?;
    if state.lanes.iter().any(|l| l.name == new) {
        return Err(other(format!("lane already exists: {new}")));
    }
    let lane = state
        .lanes
        .iter_mut()
        .find(|l| l.name == old)
        .ok_or_else(|| other(format!("no such lane: {old}")))?;
    // Rename the real branch too, when it exists, so the mapping stays honest.
    if let Ok(mut branch) = repo.find_branch(&lane.branch, git2::BranchType::Local) {
        branch.rename(new, false)?;
    }
    lane.name = new.to_owned();
    lane.branch = new.to_owned();
    save(repo, &state)
}

/// Delete a lane, returning its owned changes to the default lane. Its branch is
/// kept so committed work is not lost. The default lane cannot be deleted.
pub fn delete(repo: &Repository, name: &str) -> Result<(), GitError> {
    if name == DEFAULT_LANE {
        return Err(other("the default lane cannot be deleted"));
    }
    let mut state = state(repo)?;
    let before = state.lanes.len();
    state.lanes.retain(|l| l.name != name);
    if state.lanes.len() == before {
        return Err(other(format!("no such lane: {name}")));
    }
    save(repo, &state)
}

/// Assign one hunk of a tracked file to a lane, identified by the hunk's current
/// `new_start` in the working diff. The whole path becomes hunk-managed (its
/// other hunks fall to the default lane), and the hunk is anchored by content so
/// the ownership survives edits elsewhere in the file.
pub fn assign_hunk(
    repo: &Repository,
    lane: &str,
    path: &str,
    new_start: u32,
) -> Result<(), GitError> {
    let mut state = state(repo)?;
    if !state.lanes.iter().any(|l| l.name == lane) {
        return Err(other(format!("no such lane: {lane}")));
    }
    let base = Oid::from_str(&state.base)?;
    let anchor = tracked_hunks(repo, base)?
        .into_iter()
        .find(|(p, s, _)| p == path && *s == new_start)
        .map(|(_, _, a)| a)
        .ok_or_else(|| other(format!("no hunk at {path}:{new_start}")))?;

    // The path becomes hunk-managed: drop any whole-file ownership of it, and
    // move this specific hunk to the target lane.
    for l in &mut state.lanes {
        l.paths.retain(|p| p != path);
        l.hunks.retain(|h| !(h.path == path && h.anchor == anchor));
    }
    let target = state.lanes.iter_mut().find(|l| l.name == lane).unwrap();
    target.hunks.push(HunkRef {
        path: path.to_owned(),
        anchor,
    });
    save(repo, &state)
}

/// Assign a worktree path to a lane, taking it away from whatever lane held it.
pub fn assign(repo: &Repository, lane: &str, path: &str) -> Result<(), GitError> {
    let mut state = state(repo)?;
    if !state.lanes.iter().any(|l| l.name == lane) {
        return Err(other(format!("no such lane: {lane}")));
    }
    for l in &mut state.lanes {
        l.paths.retain(|p| p != path);
    }
    let target = state.lanes.iter_mut().find(|l| l.name == lane).unwrap();
    target.paths.push(path.to_owned());
    save(repo, &state)
}

/// Return a path (and any of its hunks) to the default lane by removing it from
/// every other lane; the default owns whatever is unassigned.
pub fn unassign(repo: &Repository, path: &str) -> Result<(), GitError> {
    let mut state = state(repo)?;
    for l in &mut state.lanes {
        if l.name != DEFAULT_LANE {
            l.paths.retain(|p| p != path);
            l.hunks.retain(|h| h.path != path);
        }
    }
    save(repo, &state)
}

/// Commit a lane's owned changes to its branch, synthesized in memory: HEAD and
/// the worktree are not disturbed. Returns the git-style summary line.
pub fn commit(repo: &Repository, lane_name: &str, message: &str) -> Result<String, GitError> {
    let state = state(repo)?;
    let base = Oid::from_str(&state.base)?;
    let lane = state
        .lanes
        .iter()
        .find(|l| l.name == lane_name)
        .ok_or_else(|| other(format!("no such lane: {lane_name}")))?
        .clone();
    if lane.paths.is_empty() && lane.hunks.is_empty() {
        return Err(other(format!("lane {lane_name} has no owned changes")));
    }

    // The new commit's parent: the lane's own tip if it has commits; else the
    // parent lane's tip for a stacked lane; else the shared fork point.
    let own_tip = repo
        .find_branch(&lane.branch, git2::BranchType::Local)
        .ok()
        .and_then(|b| b.get().target());
    let parent_oid = match own_tip {
        Some(tip) => tip,
        None => match &lane.parent {
            Some(parent_lane) => {
                let parent_branch = state
                    .lanes
                    .iter()
                    .find(|l| &l.name == parent_lane)
                    .map(|l| l.branch.clone())
                    .ok_or_else(|| other(format!("parent lane {parent_lane} not found")))?;
                repo.find_branch(&parent_branch, git2::BranchType::Local)
                    .ok()
                    .and_then(|b| b.get().target())
                    .ok_or_else(|| other(format!("commit the parent lane {parent_lane} first")))?
            }
            None => base,
        },
    };
    let parent = repo.find_commit(parent_oid)?;
    let start_tree = parent.tree()?;

    // Build the new tree in a throwaway in-memory index so the repo index and
    // worktree are untouched.
    let mut index = git2::Index::new()?;
    index.read_tree(&start_tree)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| other("bare repositories have no lanes"))?;

    // Whole-file ownership: swap the worktree blob in (or drop a deleted file).
    for path in &lane.paths {
        let full = workdir.join(path);
        if full.symlink_metadata().is_err() {
            let _ = index.remove_path(Path::new(path));
            continue;
        }
        index.add(&worktree_entry(repo, workdir, path)?)?;
    }

    // Hunk ownership: each hunk-managed path on the lane branch is `base + this
    // lane's owned hunks`. Apply only the owned hunks of the base-vs-worktree
    // diff onto the base tree, then take the resulting blobs.
    apply_owned_hunks(repo, base, &lane, &mut index)?;

    let tree_oid = index.write_tree_to(repo)?;
    if tree_oid == start_tree.id() {
        return Err(other(format!("nothing to commit in lane {lane_name}")));
    }
    let tree = repo.find_tree(tree_oid)?;

    let sig = signature(repo)?;
    let msg = change_id::ensure(repo, message);
    let branch_ref = format!("refs/heads/{}", lane.branch);
    let new = repo.commit(Some(&branch_ref), &sig, &sig, &msg, &tree, &[&parent])?;

    // Ownership is intentionally NOT cleared: the paths still differ from the
    // fork point, so they stay this lane's files. A subsequent commit starts
    // from the lane's new tip, so already-committed content yields nothing to
    // commit, and further edits to the same files keep flowing to this lane.

    let short = repo
        .find_object(new, None)
        .ok()
        .and_then(|o| o.short_id().ok())
        .and_then(|b| b.as_str().ok().map(str::to_owned))
        .unwrap_or_default();
    let subject = msg.lines().next().unwrap_or("");
    Ok(format!("[{} {short}] {subject}", lane.branch))
}

/// Build an index entry for a worktree path: its blob plus a mode (regular,
/// executable, or symlink). Stat fields are zeroed - only mode/id/path matter
/// for writing a tree.
fn worktree_entry(repo: &Repository, workdir: &Path, path: &str) -> Result<IndexEntry, GitError> {
    let full = workdir.join(path);
    let meta = full.symlink_metadata()?;
    let (mode, id) = if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&full)?;
        let bytes = target.to_string_lossy();
        (0o120000, repo.blob(bytes.as_bytes())?)
    } else {
        let mode = executable_mode(&meta);
        (mode, repo.blob_path(&full)?)
    };
    Ok(IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        file_size: 0,
        id,
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    })
}

#[cfg(unix)]
pub(crate) fn executable_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    if meta.mode() & 0o111 != 0 {
        0o100755
    } else {
        0o100644
    }
}

#[cfg(not(unix))]
pub(crate) fn executable_mode(_meta: &std::fs::Metadata) -> u32 {
    0o100644
}

/// The paths changed between the fork point and the working tree (tracked edits
/// plus untracked files), which is the universe lanes can own.
fn changed_paths(repo: &Repository, base: Oid) -> Result<Vec<String>, GitError> {
    let base_tree = repo.find_commit(base)?.tree()?;
    let mut opts = DiffOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let diff = repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut opts))?;
    let mut paths = Vec::new();
    for delta in diff.deltas() {
        for file in [delta.new_file().path(), delta.old_file().path()] {
            if let Some(p) = file.and_then(|p| p.to_str()) {
                let p = p.to_owned();
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
    }
    Ok(paths)
}

/// Reconcile the stored assignment against the working tree. Non-default lanes
/// keep only ownership that still exists (paths still changed, hunks whose
/// content anchor still appears). The default lane is recomputed from scratch so
/// it always owns exactly what nothing else claims. Keeps lanes honest when
/// plain `git` edits, reverts, or commits alongside.
fn reconcile(repo: &Repository, state: &mut LanesState) -> Result<(), GitError> {
    let base = Oid::from_str(&state.base)?;
    let changed = changed_paths(repo, base)?;
    let hunks = tracked_hunks(repo, base)?;
    let current: HashSet<(String, String)> = hunks
        .iter()
        .map(|(p, _, a)| (p.clone(), a.clone()))
        .collect();

    // Prune stale ownership from real lanes; clear the default for recompute.
    for lane in &mut state.lanes {
        if lane.name == DEFAULT_LANE {
            lane.paths.clear();
            lane.hunks.clear();
            continue;
        }
        lane.paths.retain(|p| changed.contains(p));
        lane.hunks
            .retain(|h| current.contains(&(h.path.clone(), h.anchor.clone())));
    }

    // What the real lanes now claim.
    let hunk_managed: HashSet<String> = state
        .lanes
        .iter()
        .filter(|l| l.name != DEFAULT_LANE)
        .flat_map(|l| l.hunks.iter().map(|h| h.path.clone()))
        .collect();
    let owned_paths: HashSet<String> = state
        .lanes
        .iter()
        .filter(|l| l.name != DEFAULT_LANE)
        .flat_map(|l| l.paths.clone())
        .collect();
    let owned_hunks: HashSet<(String, String)> = state
        .lanes
        .iter()
        .filter(|l| l.name != DEFAULT_LANE)
        .flat_map(|l| l.hunks.iter().map(|h| (h.path.clone(), h.anchor.clone())))
        .collect();

    let default = state
        .lanes
        .iter_mut()
        .find(|l| l.name == DEFAULT_LANE)
        .ok_or_else(|| other("lanes state has no default lane"))?;
    // Default owns changed paths that are neither whole-owned nor hunk-managed.
    for path in &changed {
        if !owned_paths.contains(path) && !hunk_managed.contains(path) {
            default.paths.push(path.clone());
        }
    }
    // Default owns the leftover hunks of any hunk-managed path.
    for (path, _start, anchor) in &hunks {
        if hunk_managed.contains(path) && !owned_hunks.contains(&(path.clone(), anchor.clone())) {
            default.hunks.push(HunkRef {
                path: path.clone(),
                anchor: anchor.clone(),
            });
        }
    }
    Ok(())
}

/// Every tracked-file hunk in the base-vs-worktree diff as `(path, new_start,
/// anchor)`. The anchor is a content hash so it is stable across shifts.
fn tracked_hunks(repo: &Repository, base: Oid) -> Result<Vec<(String, u32, String)>, GitError> {
    let base_tree = repo.find_commit(base)?.tree()?;
    let mut opts = DiffOptions::new();
    let diff = repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut opts))?;
    let mut out = Vec::new();
    for idx in 0..diff.deltas().len() {
        let Some(path) = diff.get_delta(idx).and_then(|d| {
            d.new_file()
                .path()
                .map(|p| p.to_string_lossy().into_owned())
        }) else {
            continue;
        };
        let Some(patch) = Patch::from_diff(&diff, idx)? else {
            continue;
        };
        for h in 0..patch.num_hunks() {
            let (hunk, _) = patch.hunk(h)?;
            out.push((path.clone(), hunk.new_start(), hunk_anchor(&patch, h)?));
        }
    }
    Ok(out)
}

/// Apply only `lane`'s owned hunks of the base-vs-worktree diff onto the base
/// tree, and copy the resulting blob for each hunk-managed path into `index`.
/// The invariant is that a hunk-managed path on a lane branch equals `base` plus
/// that lane's owned hunks.
fn apply_owned_hunks(
    repo: &Repository,
    base: Oid,
    lane: &Lane,
    index: &mut git2::Index,
) -> Result<(), GitError> {
    if lane.hunks.is_empty() {
        return Ok(());
    }
    let owned: HashSet<(String, String)> = lane
        .hunks
        .iter()
        .map(|h| (h.path.clone(), h.anchor.clone()))
        .collect();
    // Resolve the owned hunks to their current line positions for the callback.
    let mut owned_starts: HashMap<String, HashSet<u32>> = HashMap::new();
    for (path, start, anchor) in tracked_hunks(repo, base)? {
        if owned.contains(&(path.clone(), anchor)) {
            owned_starts.entry(path).or_default().insert(start);
        }
    }
    if owned_starts.is_empty() {
        return Ok(());
    }

    let base_tree = repo.find_commit(base)?.tree()?;
    let mut dopts = DiffOptions::new();
    let diff = repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut dopts))?;

    // apply_to_tree calls the delta callback once per file (which records the
    // current path) then the hunk callback per hunk; accept only owned paths and
    // owned hunks. Scoped so the callbacks release their borrows before the read.
    let applied = {
        let current = RefCell::new(String::new());
        let mut aopts = ApplyOptions::new();
        aopts.delta_callback(|delta| {
            let path = delta.and_then(|d| {
                d.new_file()
                    .path()
                    .map(|p| p.to_string_lossy().into_owned())
            });
            *current.borrow_mut() = path.clone().unwrap_or_default();
            path.map(|p| owned_starts.contains_key(&p)).unwrap_or(false)
        });
        aopts.hunk_callback(|hunk| {
            let path = current.borrow().clone();
            hunk.map(|h| {
                owned_starts
                    .get(&path)
                    .map(|s| s.contains(&h.new_start()))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
        });
        repo.apply_to_tree(&base_tree, &diff, Some(&mut aopts))?
    };

    for path in owned_starts.keys() {
        match applied.get_path(Path::new(path), 0) {
            Some(entry) => index.add(&entry)?,
            None => {
                let _ = index.remove_path(Path::new(path));
            }
        }
    }
    Ok(())
}

/// A content anchor for hunk `h`: a hash of its line origins and content, with
/// no line numbers, so editing elsewhere in the file does not change it.
fn hunk_anchor(patch: &Patch, h: usize) -> Result<String, GitError> {
    let mut bytes = Vec::new();
    for j in 0..patch.num_lines_in_hunk(h)? {
        let line = patch.line_in_hunk(h, j)?;
        bytes.push(line.origin() as u8);
        bytes.extend_from_slice(line.content());
    }
    Ok(git2::Oid::hash_object(git2::ObjectType::Blob, &bytes)?.to_string())
}

/// Read the state from the tip of `refs/rgit/lanes`, or `None` if not active.
fn load(repo: &Repository) -> Result<Option<LanesState>, GitError> {
    let Ok(reference) = repo.find_reference(LANES_REF) else {
        return Ok(None);
    };
    let commit = reference.peel_to_commit()?;
    Ok(Some(parse(commit.message().unwrap_or(""))))
}

/// Persist the state as a new commit under `refs/rgit/lanes`, chained on the
/// previous one for an audit trail and op-log-style history.
fn save(repo: &Repository, state: &LanesState) -> Result<(), GitError> {
    let sig = signature(repo)?;
    let empty = repo.treebuilder(None)?.write()?;
    let tree = repo.find_tree(empty)?;
    let parent = repo
        .find_reference(LANES_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(
        Some(LANES_REF),
        &sig,
        &sig,
        &serialize(state),
        &tree,
        &parents,
    )?;
    Ok(())
}

/// A signature for lanes commits: the repo's configured identity, else a stable
/// fallback so lanes work in a bare-config test repo.
fn signature(repo: &Repository) -> Result<Signature<'static>, GitError> {
    match crate::git_repo::ident_signature(repo, true) {
        Ok(sig) => Ok(sig),
        Err(_) => Ok(Signature::now("rgit", "rgit@localhost")?),
    }
}

/// The line format stored in the lanes commit message. Lane names are valid ref
/// names (no spaces), so `path: <lane> <path>` round-trips paths with spaces.
fn serialize(state: &LanesState) -> String {
    let mut s = String::from("rgit-lanes: v1\n");
    s.push_str(&format!("base: {}\n", state.base));
    for lane in &state.lanes {
        s.push_str(&format!("lane: {} {}\n", lane.name, lane.branch));
        if let Some(parent) = &lane.parent {
            s.push_str(&format!("parent: {} {}\n", lane.name, parent));
        }
        for path in &lane.paths {
            s.push_str(&format!("path: {} {}\n", lane.name, path));
        }
        for h in &lane.hunks {
            s.push_str(&format!("hunk: {} {} {}\n", lane.name, h.anchor, h.path));
        }
    }
    s
}

fn parse(message: &str) -> LanesState {
    let mut base = String::new();
    let mut lanes: Vec<Lane> = Vec::new();
    for line in message.lines() {
        if let Some(v) = line.strip_prefix("base: ") {
            base = v.trim().to_owned();
        } else if let Some(v) = line.strip_prefix("lane: ") {
            if let Some((name, branch)) = v.trim().split_once(' ') {
                lanes.push(Lane {
                    name: name.to_owned(),
                    branch: branch.to_owned(),
                    paths: Vec::new(),
                    hunks: Vec::new(),
                    commits: Vec::new(),
                    parent: None,
                });
            }
        } else if let Some(v) = line.strip_prefix("parent: ") {
            if let Some((lane, parent)) = v.trim().split_once(' ')
                && let Some(l) = lanes.iter_mut().find(|l| l.name == lane)
            {
                l.parent = Some(parent.to_owned());
            }
        } else if let Some(v) = line.strip_prefix("path: ") {
            if let Some((lane, path)) = v.split_once(' ')
                && let Some(l) = lanes.iter_mut().find(|l| l.name == lane)
            {
                l.paths.push(path.to_owned());
            }
        } else if let Some(v) = line.strip_prefix("hunk: ") {
            let mut it = v.splitn(3, ' ');
            if let (Some(lane), Some(anchor), Some(path)) = (it.next(), it.next(), it.next())
                && let Some(l) = lanes.iter_mut().find(|l| l.name == lane)
            {
                l.hunks.push(HunkRef {
                    path: path.to_owned(),
                    anchor: anchor.to_owned(),
                });
            }
        }
    }
    LanesState { base, lanes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_round_trips_including_paths_with_spaces() {
        let state = LanesState {
            base: "abc123".into(),
            lanes: vec![
                Lane {
                    name: "default".into(),
                    branch: "main".into(),
                    paths: vec!["src/a.rs".into()],
                    hunks: vec![],
                    commits: vec![],
                    parent: None,
                },
                Lane {
                    name: "parser".into(),
                    branch: "parser".into(),
                    paths: vec!["src/the parser.rs".into(), "b.rs".into()],
                    hunks: vec![HunkRef {
                        path: "src/split.rs".into(),
                        anchor: "deadbeef".into(),
                    }],
                    commits: vec![],
                    parent: None,
                },
            ],
        };
        let parsed = parse(&serialize(&state));
        assert_eq!(parsed, state);
    }
}
