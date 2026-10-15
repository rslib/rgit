//! Stacked branches: a chain of branches each based on the one below, tracked
//! with a `parent` entry in the stack file. The mechanics live
//! in the backend (`stack_parents`, `restack`) so the TUI shares them; this is
//! the CLI surface. All real branches, so plain git still works.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use rgit_git::GitBackend;

fn current_branch(backend: &Arc<dyn GitBackend>) -> Result<String> {
    backend
        .status()?
        .head
        .branch
        .ok_or_else(|| anyhow!("HEAD is detached; not on a branch"))
}

/// Create a new branch stacked on top of the current one.
pub fn new(backend: &Arc<dyn GitBackend>, name: &str) -> Result<String> {
    Ok(backend.stack_new(name)?)
}

/// Show the stack containing the current branch, newest on top.
pub fn list(backend: &Arc<dyn GitBackend>) -> Result<String> {
    let records = rgit_git::stack_file::read(&rgit_git::stack_file::dirs_of(backend.as_ref()))?;
    let parent_of: HashMap<String, String> = records
        .into_iter()
        .map(|(branch, record)| (branch, record.parent))
        .collect();
    let current = current_branch(backend).ok();

    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cursor = current.clone();
    let mut missing = false;
    while let Some(branch) = cursor {
        if !seen.insert(branch.clone()) {
            break;
        }
        let parent = parent_of.get(&branch).cloned();
        let gone = parent.as_ref().is_some_and(|p| !backend.branch_exists(p));
        chain.push((branch.clone(), parent.clone(), gone));
        if gone {
            missing = true;
            break;
        }
        cursor = parent;
    }
    if chain.len() <= 1 && parent_of.is_empty() {
        return Ok("no stacked branches (use `rgit stack new <name>`)".to_owned());
    }

    let mut lines = chain
        .iter()
        .map(|(branch, parent, gone)| {
            let mark = if current.as_deref() == Some(branch) {
                "*"
            } else {
                " "
            };
            match parent {
                Some(p) if *gone => format!("{mark} {branch}  (on {p}, missing)"),
                Some(p) => format!("{mark} {branch}  (on {p})"),
                None => format!("{mark} {branch}  (base)"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if missing {
        lines.push_str("\nRun `rgit stack repair` to fix missing parents");
    }
    Ok(lines)
}

/// Fix stack records whose branch or parent no longer exists.
pub fn repair(backend: &Arc<dyn GitBackend>) -> Result<String> {
    let dirs = rgit_git::stack_file::dirs_of(backend.as_ref());
    let fixes = rgit_git::stack_file::repair(&dirs, |name| backend.branch_exists(name))?;
    if fixes.is_empty() {
        return Ok("nothing to repair".to_owned());
    }
    Ok(fixes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Rebase every stacked branch onto its parent's current tip.
pub fn restack(backend: &Arc<dyn GitBackend>) -> Result<String> {
    Ok(render_restack(&backend.restack()?))
}

/// Retarget the children of merged parents onto the grandparent branch,
/// record the new parent and restack: when a parent pull request is merged,
/// each child's pull request base moves to the parent's base and the stack
/// config parent follows it, so the local restack lands on live branches.
/// Without an origin remote this is a plain restack; when the forge cannot be
/// reached (no credentials, network error) the retarget is skipped with a
/// warning line and the restack still runs.
pub fn sync(backend: &Arc<dyn GitBackend>) -> Result<String> {
    let parents: HashMap<String, String> = backend
        .stack_parents()?
        .into_iter()
        .filter_map(|(b, p)| p.map(|p| (b, p)))
        .collect();
    let mut retargeted = Vec::new();
    let mut warning = None;
    if !parents.is_empty()
        && let Some(origin) = backend.config_get("remote.origin.url")?
        && let Ok(target) = forge_target(&origin)
        && let Err(error) = retarget_merged_children(backend, &parents, &target, &mut retargeted)
    {
        warning = Some(format!("warning: skipped PR retarget: {error}"));
    }
    let outcome = backend.restack()?;
    let mut out = String::new();
    if !retargeted.is_empty() {
        out.push_str(&format!("retargeted:\n{}", retargeted.join("\n")));
    }
    let restacked = render_restack(&outcome);
    if restacked != "nothing to restack" {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&restacked);
    }
    if out.is_empty() {
        out.push_str("nothing to restack");
    }
    if let Some(warning) = warning {
        out.push('\n');
        out.push_str(&warning);
    }
    Ok(out)
}

/// The forge target of the origin remote, when it names a forge repository.
fn forge_target(origin: &str) -> Result<rgit_forge::ForgeTarget> {
    let selection = rgit_forge::ForgeSelection::default().resolve()?;
    Ok(rgit_forge::ForgeTarget::from_remote(&selection, origin)?)
}

/// Every stacked child whose parent pull request is merged: point the child
/// pull request at the parent's base and record that base as the new stack
/// parent, collecting one `child -> base` line per move.
fn retarget_merged_children(
    backend: &Arc<dyn GitBackend>,
    parents: &HashMap<String, String>,
    target: &rgit_forge::ForgeTarget,
    retargeted: &mut Vec<String>,
) -> Result<()> {
    let repo = target.repo()?.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let forge = rgit_forge::Forge::connect(target).await?;
        for (child, parent) in parents {
            let Some(parent_pr) = forge.pull_request_for_branch(&repo, parent).await? else {
                continue;
            };
            if !parent_pr.merged && parent_pr.state != "merged" {
                continue;
            }
            let base = parent_pr.base_branch.clone();
            if base == *parent {
                continue;
            }
            if let Some(child_pr) = forge.pull_request_for_branch(&repo, child).await?
                && child_pr.base_branch != base
            {
                forge
                    .retarget_pull_request(&repo, child_pr.number, &base)
                    .await?;
            }
            rgit_git::stack_file::set_parent(
                &rgit_git::stack_file::dirs_of(backend.as_ref()),
                child,
                &base,
            )
            .map_err(|error| anyhow!("{error}"))?;
            retargeted.push(format!("{child} -> {base}"));
        }
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(())
}

/// Human summary of a restack: what moved, and what conflicted and needs a
/// manual resolve.
pub fn render_restack(outcome: &rgit_git::RestackOutcome) -> String {
    if outcome.is_empty() {
        return "nothing to restack".to_owned();
    }
    let mut out = String::new();
    if !outcome.restacked.is_empty() {
        out.push_str("restacked:\n");
        out.push_str(&outcome.restacked.join("\n"));
    }
    if !outcome.conflicted.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "conflicted (left for you to resolve): {}",
            outcome.conflicted.join(", ")
        ));
    }
    out
}
