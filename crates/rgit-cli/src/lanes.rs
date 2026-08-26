//! Lanes CLI surface: several lines of work in one worktree. Assign uncommitted
//! files to named lanes and commit each to its own branch. The mechanics live in
//! the backend (`lanes_*` / `lane_*`), so the TUI can share them later; this is
//! the thin CLI layer. All real branches and a ref under `refs/rgit/lanes`, so
//! plain git keeps working and `lanes off` returns to a plain repo.

use std::sync::Arc;

use anyhow::Result;
use rgit_git::GitBackend;

pub fn init(backend: &Arc<dyn GitBackend>) -> Result<String> {
    backend.lanes_init()?;
    Ok("lanes on; assign files with `rgit lanes assign <lane> <path>`".to_owned())
}

pub fn off(backend: &Arc<dyn GitBackend>) -> Result<String> {
    backend.lanes_off()?;
    Ok("lanes off (lane branches kept)".to_owned())
}

pub fn new_lane(backend: &Arc<dyn GitBackend>, name: &str) -> Result<String> {
    backend.lane_new(name)?;
    Ok(format!("created lane {name}"))
}

pub fn assign(backend: &Arc<dyn GitBackend>, lane: &str, path: &str) -> Result<String> {
    backend.lane_assign(lane, path)?;
    Ok(format!("{path} -> {lane}"))
}

pub fn assign_hunk(
    backend: &Arc<dyn GitBackend>,
    lane: &str,
    path: &str,
    new_start: u32,
) -> Result<String> {
    backend.lane_assign_hunk(lane, path, new_start)?;
    Ok(format!("{path}:{new_start} -> {lane}"))
}

pub fn unassign(backend: &Arc<dyn GitBackend>, path: &str) -> Result<String> {
    backend.lane_unassign(path)?;
    Ok(format!("{path} -> default"))
}

pub fn commit(backend: &Arc<dyn GitBackend>, lane: &str, message: &str) -> Result<String> {
    Ok(backend.lane_commit(lane, message)?)
}

pub fn rename(backend: &Arc<dyn GitBackend>, old: &str, new: &str) -> Result<String> {
    backend.lane_rename(old, new)?;
    Ok(format!("{old} -> {new}"))
}

pub fn delete(backend: &Arc<dyn GitBackend>, name: &str) -> Result<String> {
    backend.lane_delete(name)?;
    Ok(format!("deleted lane {name}"))
}

pub fn push(backend: &Arc<dyn GitBackend>, lane: &str) -> Result<String> {
    Ok(backend.lane_push(lane)?)
}

pub fn pr(backend: &Arc<dyn GitBackend>, lane: &str) -> Result<String> {
    Ok(backend.lane_pr(lane)?)
}

/// List the lanes, each with its branch and owned files.
pub fn list(backend: &Arc<dyn GitBackend>) -> Result<String> {
    let state = backend.lanes_state()?;
    let mut out = Vec::new();
    for lane in &state.lanes {
        out.push(format!("{} [{}]", lane.name, lane.branch));
        for (short, summary) in &lane.commits {
            out.push(format!("  * {short} {summary}"));
        }
        if lane.paths.is_empty() && lane.hunks.is_empty() {
            if lane.commits.is_empty() {
                out.push("  (no files)".to_owned());
            } else {
                out.push("  (no pending changes)".to_owned());
            }
        }
        for path in &lane.paths {
            out.push(format!("  {path}"));
        }
        for h in &lane.hunks {
            let short: String = h.anchor.chars().take(7).collect();
            out.push(format!("  {} (hunk {short})", h.path));
        }
    }
    Ok(out.join("\n"))
}
