//! Interactive prompts for missing arguments, shown only on a real terminal.
//! When stdin/stdout are not a TTY (an agent, a pipe, CI) or `--no-input` is
//! set, the caller errors instead, so scripted use stays non-interactive.

use std::io::IsTerminal;
use std::sync::Arc;

use rgit_git::{GitBackend, LogOptions};

use crate::prompt::{self, Item};

/// Whether prompts should be shown: a real terminal and not `--no-input`.
pub fn enabled(no_input: bool) -> bool {
    !no_input && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
}

/// Select a local branch.
pub fn pick_branch(backend: &Arc<dyn GitBackend>, prompt: &str) -> anyhow::Result<String> {
    let branches = backend.local_branches()?;
    if branches.is_empty() {
        anyhow::bail!("no local branches");
    }
    let items = branches
        .into_iter()
        .map(|b| Item::new(b.clone(), b))
        .collect();
    prompt::select(prompt, items).map_err(err)
}

/// Select one of the recent commits (short id + subject).
pub fn pick_commit(backend: &Arc<dyn GitBackend>, prompt: &str) -> anyhow::Result<String> {
    let entries = backend.log(&LogOptions {
        limit: 30,
        ..Default::default()
    })?;
    if entries.is_empty() {
        anyhow::bail!("no commits");
    }
    let items = entries
        .into_iter()
        .map(|e| Item::new(e.short_id.clone(), format!("{}  {}", e.short_id, e.summary)))
        .collect();
    prompt::select(prompt, items).map_err(err)
}

/// Select a stash by index, labelled with its message.
pub fn pick_stash(backend: &Arc<dyn GitBackend>, prompt: &str) -> anyhow::Result<usize> {
    let stashes = backend.status()?.stashes;
    if stashes.is_empty() {
        anyhow::bail!("no stashes");
    }
    let items = stashes
        .into_iter()
        .map(|s| Item::new(s.index, format!("stash@{{{}}}  {}", s.index, s.message)))
        .collect();
    prompt::select(prompt, items).map_err(err)
}

/// Select one or more local branches (for deletion).
pub fn multiselect_branches(
    backend: &Arc<dyn GitBackend>,
    prompt: &str,
) -> anyhow::Result<Vec<String>> {
    let current = backend.status().ok().and_then(|s| s.head.branch);
    let branches: Vec<String> = backend
        .local_branches()?
        .into_iter()
        .filter(|b| Some(b) != current.as_ref())
        .collect();
    if branches.is_empty() {
        anyhow::bail!("no other branches");
    }
    let items = branches
        .into_iter()
        .map(|b| Item::new(b.clone(), b))
        .collect();
    prompt::multiselect(prompt, items).map_err(err)
}

/// Select a tag name.
pub fn pick_tag(backend: &Arc<dyn GitBackend>, prompt: &str) -> anyhow::Result<String> {
    use rgit_git::RefKind;
    let tags: Vec<String> = backend
        .refs()?
        .into_iter()
        .filter(|r| r.kind == RefKind::Tag)
        .map(|r| r.name)
        .collect();
    if tags.is_empty() {
        anyhow::bail!("no tags");
    }
    let items = tags.into_iter().map(|t| Item::new(t.clone(), t)).collect();
    prompt::select(prompt, items).map_err(err)
}

/// Select a changed path from the working tree.
pub fn pick_file(backend: &Arc<dyn GitBackend>, prompt: &str) -> anyhow::Result<String> {
    let paths: Vec<String> = backend
        .status()?
        .entries
        .into_iter()
        .map(|e| e.path)
        .collect();
    if paths.is_empty() {
        anyhow::bail!("no changed files");
    }
    let items = paths.into_iter().map(|p| Item::new(p.clone(), p)).collect();
    prompt::select(prompt, items).map_err(err)
}

/// Free-text input (rejects an empty value).
pub fn input(prompt: &str) -> anyhow::Result<String> {
    prompt::input(prompt, |v| {
        if v.trim().is_empty() {
            Err("required".to_owned())
        } else {
            Ok(())
        }
    })
    .map_err(err)
}

/// A yes/no confirmation.
pub fn confirm(prompt: &str) -> anyhow::Result<bool> {
    prompt::confirm(prompt, false).map_err(err)
}
