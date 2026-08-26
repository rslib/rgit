//! Stacked branches: a chain of branches each based on the one below, tracked
//! with a `branch.<name>.rgit-stack-parent` git config key. The mechanics live
//! in the backend (`stack_parents`, `restack`) so the TUI shares them; this is
//! the CLI surface. All real branches and real config, so plain git still works.

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
    let parent_of: HashMap<String, String> = backend
        .stack_parents()?
        .into_iter()
        .filter_map(|(b, p)| p.map(|p| (b, p)))
        .collect();
    let current = current_branch(backend).ok();

    let mut chain = Vec::new();
    let mut cursor = current.clone();
    while let Some(branch) = cursor {
        let parent = parent_of.get(&branch).cloned();
        chain.push((branch.clone(), parent.clone()));
        cursor = parent;
    }
    if chain.len() <= 1 && parent_of.is_empty() {
        return Ok("no stacked branches (use `rgit stack new <name>`)".to_owned());
    }

    let lines = chain
        .iter()
        .map(|(branch, parent)| {
            let mark = if current.as_deref() == Some(branch) {
                "*"
            } else {
                " "
            };
            match parent {
                Some(p) => format!("{mark} {branch}  (on {p})"),
                None => format!("{mark} {branch}  (base)"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(lines)
}

/// Rebase every stacked branch onto its parent's current tip.
pub fn restack(backend: &Arc<dyn GitBackend>) -> Result<String> {
    Ok(render_restack(&backend.restack()?))
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
