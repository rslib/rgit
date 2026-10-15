//! The stack file, `<common dir>/rgit/stacks.toml`: for each stacked branch
//! its parent branch and the parent tip it was last based on. Shared by all
//! worktrees of a repo. Writes take the file lock of `rgit_config::edit_toml`.

use std::collections::BTreeMap;
use std::path::Path;

use rgit_config::RepoDirs;
use rgit_config::toml_edit::{DocumentMut, Item, Table, value};

use crate::backend::GitBackend;
use crate::error::GitError;

const BRANCH: &str = "branch";
const PARENT: &str = "parent";
const BASE: &str = "base";

/// What the stack file records for one branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackRecord {
    pub parent: String,
    /// The tip of the parent the branch was last based on.
    pub base: Option<String>,
}

pub(crate) fn dirs(workdir: &Path, common_dir: std::path::PathBuf) -> RepoDirs {
    RepoDirs {
        worktree_root: Some(workdir.to_owned()),
        common_dir,
    }
}

/// The repo folders of a backend.
pub fn dirs_of(backend: &dyn GitBackend) -> RepoDirs {
    dirs(backend.workdir(), backend.common_dir())
}

pub(crate) fn dirs_of_repo(repo: &git2::Repository) -> RepoDirs {
    RepoDirs {
        worktree_root: repo.workdir().map(Path::to_owned),
        common_dir: repo.commondir().to_owned(),
    }
}

fn other(error: rgit_config::Error) -> GitError {
    GitError::Other(error.to_string())
}

/// Every branch the stack file records. A missing file is empty; a file that
/// does not parse is an error naming it. An entry with no parent is skipped.
pub fn read(dirs: &RepoDirs) -> Result<BTreeMap<String, StackRecord>, GitError> {
    let table = rgit_config::read_toml(&dirs.stack_file()).map_err(other)?;
    let mut out = BTreeMap::new();
    let Some(branches) = table.get(BRANCH).and_then(|v| v.as_table()) else {
        return Ok(out);
    };
    for (name, entry) in branches {
        let Some(parent) = entry.get(PARENT).and_then(|v| v.as_str()) else {
            continue;
        };
        let base = entry.get(BASE).and_then(|v| v.as_str()).map(str::to_owned);
        out.insert(
            name.clone(),
            StackRecord {
                parent: parent.to_owned(),
                base,
            },
        );
    }
    Ok(out)
}

fn edit(dirs: &RepoDirs, branch: &str, f: impl FnOnce(&mut Table)) -> Result<(), GitError> {
    rgit_config::edit_toml(&dirs.stack_file(), |doc: &mut DocumentMut| {
        let branches = doc
            .entry(BRANCH)
            .or_insert_with(|| {
                let mut table = Table::new();
                table.set_implicit(true);
                Item::Table(table)
            })
            .as_table_mut()
            .ok_or_else(|| rgit_config::Error::BadKey(BRANCH.to_owned()))?;
        let table = branches
            .entry(branch)
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| rgit_config::Error::BadKey(branch.to_owned()))?;
        f(table);
        Ok::<(), rgit_config::Error>(())
    })
    .map_err(other)
}

/// Record a branch with its parent and base; a `None` base clears the old one.
pub fn record(
    dirs: &RepoDirs,
    branch: &str,
    parent: &str,
    base: Option<&str>,
) -> Result<(), GitError> {
    edit(dirs, branch, |table| {
        table.insert(PARENT, value(parent));
        match base {
            Some(base) => {
                table.insert(BASE, value(base));
            }
            None => {
                table.remove(BASE);
            }
        }
    })
}

/// Change the parent of a branch, keeping its base.
pub fn set_parent(dirs: &RepoDirs, branch: &str, parent: &str) -> Result<(), GitError> {
    edit(dirs, branch, |table| {
        table.insert(PARENT, value(parent));
    })
}

/// Change the base of a branch, keeping its parent.
pub fn set_base(dirs: &RepoDirs, branch: &str, base: &str) -> Result<(), GitError> {
    edit(dirs, branch, |table| {
        table.insert(BASE, value(base));
    })
}

fn branches_mut(doc: &mut DocumentMut) -> Result<&mut Table, rgit_config::Error> {
    doc.get_mut(BRANCH)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| rgit_config::Error::BadKey(BRANCH.to_owned()))
}

fn parent_of(item: &Item) -> Option<&str> {
    item.get(PARENT).and_then(Item::as_str)
}

/// Move the record of `old` to `new` and point its children at `new`.
/// Writes nothing when no record and no child refers to `old`.
pub fn rename(dirs: &RepoDirs, old: &str, new: &str) -> Result<(), GitError> {
    let records = read(dirs)?;
    if !records.contains_key(old) && records.values().all(|r| r.parent != old) {
        return Ok(());
    }
    rgit_config::edit_toml(&dirs.stack_file(), |doc: &mut DocumentMut| {
        let branches = branches_mut(doc)?;
        if let Some(item) = branches.remove(old) {
            branches.insert(new, item);
        }
        for (_, item) in branches.iter_mut() {
            if parent_of(item) == Some(old)
                && let Some(table) = item.as_table_mut()
            {
                table.insert(PARENT, value(new));
            }
        }
        Ok::<(), rgit_config::Error>(())
    })
    .map_err(other)
}

/// Drop the record of `branch`. Each child takes the parent of `branch`, or
/// loses its record when `branch` had none. Writes nothing when no record and
/// no child refers to `branch`.
pub fn remove(dirs: &RepoDirs, branch: &str) -> Result<(), GitError> {
    let records = read(dirs)?;
    if !records.contains_key(branch) && records.values().all(|r| r.parent != branch) {
        return Ok(());
    }
    let grandparent = records.get(branch).map(|r| r.parent.clone());
    rgit_config::edit_toml(&dirs.stack_file(), |doc: &mut DocumentMut| {
        let branches = branches_mut(doc)?;
        branches.remove(branch);
        let children: Vec<String> = branches
            .iter()
            .filter(|(_, item)| parent_of(item) == Some(branch))
            .map(|(name, _)| name.to_owned())
            .collect();
        for child in children {
            match &grandparent {
                Some(parent) => {
                    if let Some(table) = branches.get_mut(&child).and_then(Item::as_table_mut) {
                        table.insert(PARENT, value(parent.as_str()));
                    }
                }
                None => {
                    branches.remove(&child);
                }
            }
        }
        Ok::<(), rgit_config::Error>(())
    })
    .map_err(other)
}

/// One change `repair` made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repair {
    Reparented {
        branch: String,
        from: String,
        to: String,
    },
    Orphaned {
        branch: String,
        from: String,
    },
    Removed {
        branch: String,
    },
}

impl std::fmt::Display for Repair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Repair::Reparented { branch, from, to } => {
                write!(f, "{branch}: parent {from} is gone, now on {to}")
            }
            Repair::Orphaned { branch, from } => {
                write!(f, "{branch}: parent {from} is gone, now has no parent")
            }
            Repair::Removed { branch } => write!(f, "{branch}: branch is gone, record removed"),
        }
    }
}

/// The nearest ancestor of `branch` through the records that `exists`.
fn nearest_existing(
    records: &BTreeMap<String, StackRecord>,
    branch: &str,
    exists: &impl Fn(&str) -> bool,
) -> Option<String> {
    let mut seen = std::collections::BTreeSet::from([branch.to_owned()]);
    let mut cursor = records.get(branch)?.parent.clone();
    while seen.insert(cursor.clone()) {
        if exists(&cursor) {
            return Some(cursor);
        }
        cursor = records.get(&cursor)?.parent.clone();
    }
    None
}

/// Fix the records whose branch or parent is gone: drop the record of a
/// missing branch, and move a branch with a missing parent to its nearest
/// existing ancestor, or to no parent. Writes nothing when there is nothing
/// to fix.
pub fn repair(dirs: &RepoDirs, exists: impl Fn(&str) -> bool) -> Result<Vec<Repair>, GitError> {
    let records = read(dirs)?;
    let mut fixes = Vec::new();
    for (branch, record) in &records {
        if !exists(branch) {
            fixes.push(Repair::Removed {
                branch: branch.clone(),
            });
        } else if !exists(&record.parent) {
            let from = record.parent.clone();
            let branch = branch.clone();
            fixes.push(match nearest_existing(&records, &branch, &exists) {
                Some(to) => Repair::Reparented { branch, from, to },
                None => Repair::Orphaned { branch, from },
            });
        }
    }
    if fixes.is_empty() {
        return Ok(fixes);
    }
    rgit_config::edit_toml(&dirs.stack_file(), |doc: &mut DocumentMut| {
        let branches = branches_mut(doc)?;
        for fix in &fixes {
            match fix {
                Repair::Removed { branch } | Repair::Orphaned { branch, .. } => {
                    branches.remove(branch);
                }
                Repair::Reparented { branch, to, .. } => {
                    if let Some(table) = branches.get_mut(branch).and_then(Item::as_table_mut) {
                        table.insert(PARENT, value(to.as_str()));
                    }
                }
            }
        }
        Ok::<(), rgit_config::Error>(())
    })
    .map_err(other)?;
    Ok(fixes)
}
