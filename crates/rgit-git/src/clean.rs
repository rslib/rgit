//! `git clean`: which untracked (or ignored) paths go, and removing them the
//! way git's clean.c does, nested repositories and all.

use std::collections::HashSet;
use std::path::Path;

use git2::Repository;

use crate::GitError;

/// What `git clean` removes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanOptions {
    /// Remove untracked folders too (git's -d).
    pub dirs: bool,
    /// Ignore the standard ignore rules, so ignored files go too (-x).
    pub ignored: bool,
    /// Remove only ignored files (-X).
    pub only_ignored: bool,
    /// Extra ignore patterns (-e).
    pub exclude: Vec<String>,
    /// Remove nested repositories too (-ff).
    pub force_repos: bool,
    /// Only report (-n).
    pub dry_run: bool,
    /// Only paths matching these pathspecs, from the repository root.
    pub paths: Vec<String>,
    /// The folder the command runs in, from the root; paths print relative
    /// to it.
    pub prefix: String,
}

struct Walk<'a> {
    repo: &'a Repository,
    root: &'a Path,
    o: &'a CleanOptions,
    tracked: HashSet<String>,
    tracked_dirs: HashSet<String>,
    exclude: ignore::gitignore::Gitignore,
    specs: Vec<String>,
}

impl Walk<'_> {
    fn is_ignored(&self, rel: &str, dir: bool) -> bool {
        let std = !self.o.ignored
            && self
                .repo
                .is_path_ignored(if dir {
                    format!("{rel}/")
                } else {
                    rel.to_owned()
                })
                .unwrap_or(false);
        std || self
            .exclude
            .matched_path_or_any_parents(rel, dir)
            .is_ignore()
    }

    fn children(&self, rel: &str) -> Vec<(String, bool)> {
        let dir = if rel.is_empty() {
            self.root.to_path_buf()
        } else {
            self.root.join(rel)
        };
        let mut out: Vec<(String, bool)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == ".git" {
                    return None;
                }
                let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                let path = if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                };
                Some((path, is_dir))
            })
            .collect();
        out.sort();
        out
    }

    fn nested_repo(&self, rel: &str) -> bool {
        !self.o.force_repos && self.root.join(rel).join(".git").exists()
    }

    /// Whether the pathspecs take `rel` whole, or reach inside it.
    fn covers(&self, rel: &str) -> bool {
        self.specs.is_empty() || crate::pathspec_matches(&self.specs, rel)
    }

    fn reaches_into(&self, rel: &str) -> bool {
        self.specs
            .iter()
            .any(|s| s.starts_with(&format!("{rel}/")) || s.contains(['*', '?', '[']))
    }

    fn exact(&self, rel: &str) -> bool {
        self.specs.iter().any(|s| s.trim_end_matches('/') == rel)
    }

    /// Whether anything under the untracked folder `rel` is ignored.
    fn holds_ignored(&self, rel: &str) -> bool {
        self.children(rel).into_iter().any(|(p, dir)| {
            self.is_ignored(&p, dir) || dir && !self.nested_repo(&p) && self.holds_ignored(&p)
        })
    }

    fn scan(&self, rel: &str, out: &mut Vec<String>) {
        for (p, dir) in self.children(rel) {
            if self.tracked.contains(&p) {
                continue;
            }
            if dir && self.tracked_dirs.contains(&p) {
                self.scan(&p, out);
                continue;
            }
            if dir && self.nested_repo(&p) {
                continue;
            }
            let ignored = self.is_ignored(&p, dir);
            if self.o.only_ignored {
                if !ignored {
                    if dir {
                        self.scan(&p, out);
                    }
                } else if !dir {
                    if self.covers(&p) {
                        out.push(p);
                    }
                } else if (self.o.dirs || self.exact(&p)) && self.covers(&p) {
                    out.push(format!("{p}/"));
                } else if self.reaches_into(&p) {
                    self.scan_ignored_dir(&p, out);
                }
                continue;
            }
            if ignored {
                continue;
            }
            if !dir {
                if self.covers(&p) {
                    out.push(p);
                }
            } else if self.o.dirs {
                if self.covers(&p) && !self.holds_ignored(&p) {
                    out.push(format!("{p}/"));
                } else if self.covers(&p) || self.reaches_into(&p) {
                    self.scan(&p, out);
                }
            } else if self.exact(&p) {
                out.push(format!("{p}/"));
            } else if self.reaches_into(&p) {
                self.scan(&p, out);
            }
        }
    }

    /// Everything the pathspecs name inside an ignored folder, for -X.
    fn scan_ignored_dir(&self, rel: &str, out: &mut Vec<String>) {
        for (p, dir) in self.children(rel) {
            if dir {
                self.scan_ignored_dir(&p, out);
            } else if self.covers(&p) {
                out.push(p);
            }
        }
    }
}

/// The paths `git clean` would remove, from the root, folders ending in `/`.
pub fn candidates(repo: &Repository, o: &CleanOptions) -> Result<Vec<String>, GitError> {
    let root = repo
        .workdir()
        .ok_or_else(|| GitError::Other("clean needs a working tree".to_owned()))?;
    let mut tracked = HashSet::new();
    let mut tracked_dirs = HashSet::new();
    for e in repo.index()?.iter() {
        let path = String::from_utf8_lossy(&e.path).into_owned();
        let mut at = path.as_str();
        while let Some((parent, _)) = at.rsplit_once('/') {
            if !tracked_dirs.insert(parent.to_owned()) {
                break;
            }
            at = parent;
        }
        tracked.insert(path);
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    for pattern in &o.exclude {
        builder
            .add_line(None, pattern)
            .map_err(|e| GitError::Other(format!("bad pattern {pattern}: {e}")))?;
    }
    let walk = Walk {
        repo,
        root,
        o,
        tracked,
        tracked_dirs,
        exclude: builder
            .build()
            .map_err(|e| GitError::Other(e.to_string()))?,
        specs: o
            .paths
            .iter()
            .filter(|p| !matches!(p.as_str(), "." | ""))
            .cloned()
            .collect(),
    };
    let mut out = Vec::new();
    walk.scan("", &mut out);
    out.sort();
    Ok(out)
}

/// Whether gitignore `patterns` match `path`.
pub fn ignore_match(patterns: &[&str], path: &str, dir: bool) -> bool {
    let mut builder = ignore::gitignore::GitignoreBuilder::new("/");
    for p in patterns {
        let _ = builder.add_line(None, p);
    }
    builder
        .build()
        .is_ok_and(|rules| rules.matched_path_or_any_parents(path, dir).is_ignore())
}

/// `path` (from the root) as seen from the folder `prefix`.
pub fn relative(path: &str, prefix: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return path.to_owned();
    }
    let mut rest = path;
    let mut kept = Vec::new();
    for part in prefix.split('/') {
        match rest.split_once('/') {
            Some((first, tail)) if first == part && kept.is_empty() => rest = tail,
            _ => kept.push(part),
        }
    }
    format!("{}{rest}", "../".repeat(kept.len()))
}

/// Remove `items` (from [`candidates`]) as git does, returning its report:
/// `Removing <path>` (`Would remove` with dry_run) and `Skipping repository`
/// for nested repositories left alone.
pub fn remove(root: &Path, items: &[String], o: &CleanOptions) -> Vec<String> {
    let mut lines = Vec::new();
    let verb = if o.dry_run {
        "Would remove"
    } else {
        "Removing"
    };
    for item in items {
        let full = root.join(item.trim_end_matches('/'));
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        let shown = relative(item, &o.prefix);
        if meta.is_dir() {
            if remove_dirs(&full, item.trim_end_matches('/'), o, &mut lines) {
                lines.push(format!("{verb} {shown}"));
            }
        } else if o.dry_run || std::fs::remove_file(&full).is_ok() {
            lines.push(format!("{verb} {shown}"));
        } else {
            lines.push(format!("warning: failed to remove {shown}"));
        }
    }
    lines
}

/// git's remove_dirs: empty `dir` bottom up; true when it is gone. What went
/// is reported one by one only when the folder itself stays.
fn remove_dirs(dir: &Path, rel: &str, o: &CleanOptions, lines: &mut Vec<String>) -> bool {
    let verb = if o.dry_run {
        "Would remove"
    } else {
        "Removing"
    };
    if !o.force_repos && dir.join(".git").exists() {
        let skip = if o.dry_run {
            "Would skip repository"
        } else {
            "Skipping repository"
        };
        lines.push(format!("{skip} {}", relative(rel, &o.prefix)));
        return false;
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    entries.sort();
    let mut gone = true;
    let mut dels = Vec::new();
    for name in entries {
        let path = dir.join(&name);
        let sub = format!("{rel}/{}", name.to_string_lossy());
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        if is_dir {
            if remove_dirs(&path, &sub, o, lines) {
                dels.push(sub);
            } else {
                gone = false;
            }
        } else if o.dry_run || std::fs::remove_file(&path).is_ok() {
            dels.push(sub);
        } else {
            lines.push(format!(
                "warning: failed to remove {}",
                relative(&sub, &o.prefix)
            ));
            gone = false;
        }
    }
    if gone && !o.dry_run && std::fs::remove_dir(dir).is_err() {
        gone = false;
    }
    if !gone {
        for d in dels {
            lines.push(format!("{verb} {}", relative(&d, &o.prefix)));
        }
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::relative;

    #[test]
    fn paths_show_from_the_current_folder() {
        assert_eq!(relative("a.txt", ""), "a.txt");
        assert_eq!(relative("t/new", "t"), "new");
        assert_eq!(relative("u/", "t"), "../u/");
        assert_eq!(relative("a/b/c", "a/x"), "../b/c");
    }
}
