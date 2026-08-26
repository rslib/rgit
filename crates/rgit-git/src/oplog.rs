//! An operation log: before each destructive operation the backend snapshots
//! the repository state (HEAD, the branch it points at, and the full working
//! tree including staged and unstaged changes) as a real git commit, chained
//! under `refs/rgit/undo`. `undo` restores the newest snapshot and moves the
//! current state onto `refs/rgit/redo`; `redo` reverses that. Because a snapshot
//! captures the working tree, undo can recover uncommitted work that
//! `git reflog` cannot.
//!
//! Snapshots are ordinary commits, so they are GC-safe while the refs point at
//! them and never confuse plain `git` (they live outside `refs/heads`).

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use git2::build::CheckoutBuilder;
use git2::{Commit, ErrorCode, IndexAddOption, Oid, Repository, Signature};

use crate::error::GitError;
use crate::model::OpLogEntry;

const UNDO: &str = "refs/rgit/undo";
const REDO: &str = "refs/rgit/redo";
const DETACHED: &str = "DETACHED";

/// Whether the op-log records snapshots. On by default; set `RGIT_OPLOG` to
/// `0`/`off`/`false`/`no` to disable it entirely (no extra refs or commits).
fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("RGIT_OPLOG").as_deref(),
            Ok("0" | "off" | "false" | "no")
        )
    })
}

/// Snapshot the current state under the undo stack and clear the redo stack
/// (a new operation invalidates any redo future). Best-effort: a failure to
/// snapshot never blocks the operation the caller is about to perform.
pub fn snapshot(repo: &Repository, label: &str) -> Result<(), GitError> {
    // Opt-out: with the op-log off, rgit writes no extra refs/commits and stays
    // a plain git tool. Undo/redo simply have nothing to restore.
    if !enabled() {
        return Ok(());
    }
    let (head_ref, head_oid) = head_state(repo)?;
    // A conflicted index cannot be written to a tree; a mid-conflict state is
    // not snapshotted (undo still restores the previous snapshot, wiping it).
    let Ok(tree) = capture_tree(repo) else {
        return Ok(());
    };
    push(repo, UNDO, tree, label, &head_ref, head_oid)?;
    clear(repo, REDO)?;
    Ok(())
}

/// Restore the newest undo snapshot (the state before the last operation),
/// pushing the current state onto the redo stack. Returns the label of the
/// operation that was undone.
pub fn undo(repo: &Repository) -> Result<String, GitError> {
    step(repo, UNDO, REDO, "undo")
}

/// Reverse the last undo.
pub fn redo(repo: &Repository) -> Result<String, GitError> {
    step(repo, REDO, UNDO, "redo")
}

/// The undo stack, newest first, for display.
pub fn entries(repo: &Repository) -> Result<Vec<OpLogEntry>, GitError> {
    let now = now_secs();
    let mut out = Vec::new();
    let mut cursor = tip(repo, UNDO)?;
    while let Some(oid) = cursor {
        let commit = repo.find_commit(oid)?;
        let meta = Meta::parse(commit.message().unwrap_or(""));
        let head = meta.head_display();
        out.push(OpLogEntry {
            label: meta.label,
            head,
            when: relative(commit.time().seconds(), now),
            short_id: short(oid),
        });
        cursor = commit.parent(0).ok().map(|p| p.id());
    }
    Ok(out)
}

/// Pop `from`, push the current state onto `to`, and restore the popped state.
fn step(repo: &Repository, from: &str, to: &str, what: &str) -> Result<String, GitError> {
    let Some(target) = tip(repo, from)? else {
        return Err(GitError::Git(git2::Error::from_str(&format!(
            "nothing to {what}"
        ))));
    };
    let commit = repo.find_commit(target)?;
    let meta = Meta::parse(commit.message().unwrap_or(""));
    // Best-effort: save where we are now so the reverse direction can return
    // here (labeled with the op being reversed). A conflicted index cannot be
    // captured, so the reverse direction is simply unavailable in that case.
    if let Ok(current) = capture_tree(repo) {
        let (head_ref, head_oid) = head_state(repo)?;
        push(repo, to, current, &meta.label, &head_ref, head_oid)?;
    }

    restore(repo, &commit, &meta)?;
    pop(repo, from)?;
    Ok(meta.label)
}

/// Write the working tree (staged + unstaged, honoring .gitignore) to a tree
/// object without disturbing the real index.
fn capture_tree(repo: &Repository) -> Result<Oid, GitError> {
    let mut index = repo.index()?;
    // Reload from disk first: `saved` is written back to the index below, so a
    // stale in-memory snapshot would clobber staging done by plain `git add`
    // since the backend last touched the index.
    index.read(true)?;
    let saved = index.write_tree()?;
    index.add_all(["*"], IndexAddOption::DEFAULT, None)?;
    let tree = index.write_tree()?;
    // Restore the in-memory index to what was staged before, and persist it so
    // the caller's staging area is untouched.
    let saved_tree = repo.find_tree(saved)?;
    index.read_tree(&saved_tree)?;
    index.write()?;
    Ok(tree)
}

/// Restore HEAD, its branch, and the working tree to a snapshot.
fn restore(repo: &Repository, commit: &Commit, meta: &Meta) -> Result<(), GitError> {
    let tree = commit.tree()?;
    // Recreate recorded branches first, so HEAD's branch exists below.
    for (name, oid) in &meta.branches {
        let _ = repo.reference(name, *oid, true, "rgit oplog restore");
    }

    let mut checkout = CheckoutBuilder::new();
    checkout.force().remove_untracked(true);
    repo.checkout_tree(tree.as_object(), Some(&mut checkout))?;
    let mut index = repo.index()?;
    index.read_tree(&tree)?;
    index.write()?;

    if meta.head_ref == DETACHED {
        repo.set_head_detached(meta.head_oid)?;
    } else if meta.head_oid.is_zero() {
        // An unborn branch: drop it if it was created since, and point HEAD back.
        if let Ok(mut r) = repo.find_reference(&meta.head_ref) {
            r.delete()?;
        }
        repo.set_head(&meta.head_ref)?;
    } else {
        repo.reference(&meta.head_ref, meta.head_oid, true, "rgit oplog restore")?;
        repo.set_head(&meta.head_ref)?;
    }

    // Delete branches created since the snapshot (HEAD now points at a recorded
    // branch, so the current branch is never among them). Only when we captured
    // branch state, so older snapshots do not wipe branches.
    if !meta.branches.is_empty() {
        let recorded: std::collections::HashSet<&str> =
            meta.branches.iter().map(|(n, _)| n.as_str()).collect();
        for (name, _) in branch_refs(repo) {
            if !recorded.contains(name.as_str()) {
                if let Ok(mut r) = repo.find_reference(&name) {
                    let _ = r.delete();
                }
            }
        }
    }
    Ok(())
}

/// The branch HEAD points at (or DETACHED / an unborn branch) and its commit.
fn head_state(repo: &Repository) -> Result<(String, Oid), GitError> {
    match repo.head() {
        Ok(head) => {
            let oid = head
                .target()
                .or_else(|| head.peel_to_commit().ok().map(|c| c.id()))
                .unwrap_or(Oid::ZERO_SHA1);
            if head.is_branch() {
                Ok((head.name().unwrap_or("HEAD").to_owned(), oid))
            } else {
                Ok((DETACHED.to_owned(), oid))
            }
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            // Fresh repo: HEAD is symbolic to a branch that has no commit yet.
            let name = repo
                .find_reference("HEAD")
                .ok()
                .and_then(|r| r.symbolic_target().ok().flatten().map(|s| s.to_owned()))
                .unwrap_or_else(|| "refs/heads/main".to_owned());
            Ok((name, Oid::ZERO_SHA1))
        }
        Err(e) => Err(e.into()),
    }
}

fn push(
    repo: &Repository,
    stack: &str,
    tree_oid: Oid,
    label: &str,
    head_ref: &str,
    head_oid: Oid,
) -> Result<(), GitError> {
    let sig = signature(repo)?;
    let tree = repo.find_tree(tree_oid)?;
    let parent = tip(repo, stack)?
        .map(|oid| repo.find_commit(oid))
        .transpose()?;
    let parents: Vec<&Commit> = parent.iter().collect();
    let mut message = format!(
        "{}\n\nrgit-oplog: v1\nhead-ref: {head_ref}\nhead-oid: {head_oid}\ntime: {}\n",
        label.lines().next().unwrap_or("op"),
        now_secs(),
    );
    for (name, oid) in branch_refs(repo) {
        message.push_str(&format!("branch: {name} {oid}\n"));
    }
    repo.commit(Some(stack), &sig, &sig, &message, &tree, &parents)?;
    prune(repo, stack)?;
    Ok(())
}

/// The most snapshots either stack keeps.
const CAP: usize = 100;

/// Keep the op-log bounded: once a stack grows past `2*CAP`, rebuild its newest
/// `CAP` snapshots as a fresh chain and re-point the ref, so it never grows
/// without limit. Amortized cheap (a rebuild happens once per `CAP` pushes).
fn prune(repo: &Repository, stack: &str) -> Result<(), GitError> {
    let Some(tip) = tip(repo, stack)? else {
        return Ok(());
    };
    // Newest-first, stopping once we know we are over the high-water mark.
    let mut chain: Vec<(Oid, String)> = Vec::new();
    let mut cursor = Some(tip);
    while let Some(oid) = cursor {
        if chain.len() > 2 * CAP {
            break;
        }
        let commit = repo.find_commit(oid)?;
        chain.push((commit.tree_id(), commit.message().unwrap_or("").to_owned()));
        cursor = commit.parent(0).ok().map(|p| p.id());
    }
    if chain.len() <= 2 * CAP {
        return Ok(());
    }

    // Rebuild the newest CAP snapshots, oldest first, as a new chain.
    chain.truncate(CAP);
    chain.reverse();
    let sig = signature(repo)?;
    let mut parent: Option<Oid> = None;
    for (tree_oid, message) in chain {
        let tree = repo.find_tree(tree_oid)?;
        let parent_commit = parent.map(|o| repo.find_commit(o)).transpose()?;
        let parents: Vec<&Commit> = parent_commit.iter().collect();
        parent = Some(repo.commit(None, &sig, &sig, &message, &tree, &parents)?);
    }
    if let Some(new_tip) = parent {
        repo.reference(stack, new_tip, true, "rgit oplog prune")?;
    }
    Ok(())
}

/// All local branch refs as `(full refname, oid)`.
fn branch_refs(repo: &Repository) -> Vec<(String, Oid)> {
    let mut out = Vec::new();
    if let Ok(branches) = repo.branches(Some(git2::BranchType::Local)) {
        for (branch, _) in branches.flatten() {
            if let (Ok(Some(name)), Some(oid)) = (branch.name(), branch.get().target()) {
                out.push((format!("refs/heads/{name}"), oid));
            }
        }
    }
    out
}

fn pop(repo: &Repository, stack: &str) -> Result<(), GitError> {
    let Some(oid) = tip(repo, stack)? else {
        return Ok(());
    };
    let commit = repo.find_commit(oid)?;
    match commit.parent(0) {
        Ok(parent) => {
            repo.reference(stack, parent.id(), true, "rgit oplog pop")?;
        }
        Err(_) => clear(repo, stack)?,
    }
    Ok(())
}

fn clear(repo: &Repository, stack: &str) -> Result<(), GitError> {
    if let Ok(mut r) = repo.find_reference(stack) {
        r.delete()?;
    }
    Ok(())
}

fn tip(repo: &Repository, stack: &str) -> Result<Option<Oid>, GitError> {
    match repo.find_reference(stack) {
        Ok(r) => Ok(r.target()),
        Err(e) if e.code() == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn signature(repo: &Repository) -> Result<Signature<'static>, GitError> {
    match repo.signature() {
        Ok(sig) => Ok(sig.to_owned()),
        Err(_) => Ok(Signature::now("rgit", "rgit@localhost")?),
    }
}

/// Parsed snapshot metadata from a snapshot commit message.
struct Meta {
    label: String,
    head_ref: String,
    head_oid: Oid,
    /// All local branch refs (full name, oid) at snapshot time, so undo can
    /// restore branches a delete/rename/create changed.
    branches: Vec<(String, Oid)>,
}

impl Meta {
    fn parse(message: &str) -> Self {
        let label = message.lines().next().unwrap_or("op").to_owned();
        let mut head_ref = DETACHED.to_owned();
        let mut head_oid = Oid::ZERO_SHA1;
        let mut branches = Vec::new();
        for line in message.lines() {
            if let Some(v) = line.strip_prefix("head-ref: ") {
                head_ref = v.trim().to_owned();
            } else if let Some(v) = line.strip_prefix("head-oid: ") {
                head_oid = Oid::from_str(v.trim()).unwrap_or(Oid::ZERO_SHA1);
            } else if let Some(v) = line.strip_prefix("branch: ") {
                let mut parts = v.trim().splitn(2, ' ');
                if let (Some(name), Some(oid)) = (parts.next(), parts.next()) {
                    if let Ok(oid) = Oid::from_str(oid) {
                        branches.push((name.to_owned(), oid));
                    }
                }
            }
        }
        Meta {
            label,
            head_ref,
            head_oid,
            branches,
        }
    }

    /// A short, human display of where HEAD was: the branch name or a short oid.
    fn head_display(&self) -> String {
        if self.head_ref == DETACHED {
            format!("detached {}", short(self.head_oid))
        } else {
            self.head_ref
                .strip_prefix("refs/heads/")
                .unwrap_or(&self.head_ref)
                .to_owned()
        }
    }
}

fn short(oid: Oid) -> String {
    oid.to_string().chars().take(7).collect()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compact relative age like git's `2h`, `3d`.
fn relative(then: i64, now: i64) -> String {
    let secs = (now - then).max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}
