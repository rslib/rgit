//! `git archive`'s export attributes: `export-ignore` drops paths and
//! `export-subst` expands `$Format:...$` in file contents.

use std::path::Path;

use git2::{Commit, Oid, Repository, Tree};

use crate::GitError;

/// `git archive` options.
#[derive(Debug, Clone, Default)]
pub struct ArchiveOpts {
    /// The revision (default HEAD).
    pub rev: String,
    /// tar, tgz, tar.gz or zip.
    pub format: String,
    /// Put every entry under this folder.
    pub prefix: String,
    /// Limit to these paths.
    pub paths: Vec<String>,
    /// zlib level 0-9 for tgz and zip.
    pub level: Option<u32>,
    /// Untracked files to add as `(name, mode, content)`, under the prefix.
    pub extra: Vec<(String, i32, Vec<u8>)>,
    /// Also read .gitattributes from the working tree.
    pub worktree_attributes: bool,
    /// The entries' modification time (default: the commit's).
    pub mtime: Option<i64>,
}

/// One `.gitattributes` line: its folder, pattern and settings.
struct Rule {
    base: String,
    pattern: regex::Regex,
    /// Matched against the whole path below `base`, not just the name.
    anchored: bool,
    attrs: Vec<(String, bool)>,
}

/// The attribute rules that apply to `tree`, lowest precedence first: the
/// tree's own .gitattributes files (shallow before deep), the working tree's
/// with `worktree`, then $GIT_DIR/info/attributes.
pub(crate) struct Attributes(Vec<Rule>);

impl Attributes {
    pub(crate) fn load(repo: &Repository, tree: &Tree, worktree: bool) -> Result<Self, GitError> {
        let mut files: Vec<(String, String)> = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
            if e.name().ok() == Some(".gitattributes")
                && let Ok(blob) = repo.find_blob(e.id())
            {
                files.push((
                    root.to_owned(),
                    String::from_utf8_lossy(blob.content()).into_owned(),
                ));
            }
            git2::TreeWalkResult::Ok
        })?;
        if worktree && let Some(workdir) = repo.workdir() {
            let mut local = Vec::new();
            let mut bases: Vec<String> = files.iter().map(|(b, _)| b.clone()).collect();
            if !bases.iter().any(String::is_empty) {
                bases.push(String::new());
            }
            for base in &bases {
                if let Ok(text) = std::fs::read_to_string(workdir.join(base).join(".gitattributes"))
                {
                    local.push((base.clone(), text));
                }
            }
            files.extend(local);
        }
        files.sort_by_key(|(base, _)| base.matches('/').count());
        if let Ok(text) = std::fs::read_to_string(repo.path().join("info/attributes")) {
            files.push((String::new(), text));
        }
        let mut rules = Vec::new();
        for (base, text) in files {
            for line in text.lines() {
                let mut words = line.split_whitespace();
                let Some(pat) = words.next().filter(|p| !p.starts_with('#')) else {
                    continue;
                };
                let attrs: Vec<(String, bool)> = words
                    .filter(|w| !w.starts_with('!'))
                    .map(|w| match w.strip_prefix('-') {
                        Some(name) => (name.to_owned(), false),
                        None => (w.split('=').next().unwrap_or(w).to_owned(), true),
                    })
                    .collect();
                let anchored = pat.trim_start_matches('/').contains('/') || pat.starts_with('/');
                let Ok(pattern) = regex::Regex::new(&glob(pat.trim_start_matches('/'))) else {
                    continue;
                };
                rules.push(Rule {
                    base: base.clone(),
                    pattern,
                    anchored,
                    attrs,
                });
            }
        }
        Ok(Attributes(rules))
    }

    /// Whether `attr` is set for `path` (the last matching rule wins).
    pub(crate) fn is_set(&self, path: &str, attr: &str) -> bool {
        let mut set = false;
        for rule in &self.0 {
            let Some(rel) = path.strip_prefix(&rule.base) else {
                continue;
            };
            let subject = if rule.anchored {
                rel
            } else {
                rel.rsplit('/').next().unwrap_or(rel)
            };
            if rule.pattern.is_match(subject)
                && let Some((_, on)) = rule.attrs.iter().rfind(|(n, _)| n == attr)
            {
                set = *on;
            }
        }
        set
    }
}

/// A gitattributes pattern as an anchored regex: `*` stays inside a folder,
/// `**` crosses them.
fn glob(pat: &str) -> String {
    let mut re = String::from("^");
    let chars: Vec<char> = pat.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    re.push_str(".*");
                    i += 2;
                }
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '[' => {
                let end = chars[i..].iter().position(|c| *c == ']').map(|p| i + p);
                match end {
                    Some(end) => {
                        let class: String = chars[i + 1..end].iter().collect();
                        let class = class
                            .strip_prefix('!')
                            .map_or(class.clone(), |c| format!("^{c}"));
                        re.push_str(&format!("[{}]", class.replace('\\', "\\\\")));
                        i = end;
                    }
                    None => re.push_str("\\["),
                }
            }
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    re
}

/// `$Format:<fmt>$` placeholders in `data` expanded for `commit`, as git's
/// `export-subst` does.
pub(crate) fn export_subst(repo: &Repository, commit: &Commit, data: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(data);
    let mut out = String::with_capacity(text.len());
    let mut rest: &str = &text;
    while let Some(start) = rest.find("$Format:") {
        let after = &rest[start + "$Format:".len()..];
        let Some(end) = after.find('$').filter(|e| !after[..*e].contains('\n')) else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str(&pretty(repo, commit, &after[..end]));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.into_bytes()
}

/// The common `git log --format` placeholders for `c`.
pub(crate) fn pretty(repo: &Repository, c: &Commit, fmt: &str) -> String {
    let short = |id: Oid| {
        repo.find_object(id, None)
            .and_then(|o| o.short_id())
            .ok()
            .and_then(|b| b.as_str().ok().map(str::to_owned))
            .unwrap_or_else(|| id.to_string()[..7].to_owned())
    };
    let msg = String::from_utf8_lossy(c.message_bytes()).into_owned();
    let body = msg.split_once("\n\n").map_or("", |(_, b)| b).trim_end();
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let Some(p) = chars.next() else {
            out.push('%');
            break;
        };
        let who = |author: bool| if author { c.author() } else { c.committer() };
        match p {
            '%' => out.push('%'),
            'n' => out.push('\n'),
            'H' => out.push_str(&c.id().to_string()),
            'h' => out.push_str(&short(c.id())),
            'T' => out.push_str(&c.tree_id().to_string()),
            't' => out.push_str(&short(c.tree_id())),
            'P' => out.push_str(
                &c.parent_ids()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            'p' => out.push_str(&c.parent_ids().map(short).collect::<Vec<_>>().join(" ")),
            's' => out.push_str(c.summary().ok().flatten().unwrap_or("")),
            'b' => {
                if !body.is_empty() {
                    out.push_str(body);
                    out.push('\n');
                }
            }
            'B' => out.push_str(&msg),
            'd' | 'D' => {
                let refs = decorations(repo, c.id());
                if !refs.is_empty() {
                    out.push_str(&if p == 'd' { format!(" ({refs})") } else { refs });
                }
            }
            'a' | 'c' => {
                let sig = who(p == 'a');
                let t = sig.when();
                let field = chars.next().unwrap_or('n');
                let s = match field {
                    'n' | 'N' => String::from_utf8_lossy(sig.name_bytes()).into_owned(),
                    'e' | 'E' => String::from_utf8_lossy(sig.email_bytes()).into_owned(),
                    'l' | 'L' => String::from_utf8_lossy(sig.email_bytes())
                        .split('@')
                        .next()
                        .unwrap_or("")
                        .to_owned(),
                    't' => t.seconds().to_string(),
                    'd' => crate::git_repo::format_git_date(t, "default"),
                    'D' => crate::git_repo::rfc2822_date(t),
                    'i' => crate::git_repo::format_git_date(t, "iso"),
                    'I' => crate::git_repo::format_git_date(t, "iso-strict"),
                    's' => crate::git_repo::format_git_date(t, "short"),
                    other => format!("%{p}{other}"),
                };
                out.push_str(&s);
            }
            other => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out
}

/// `git log --decorate`'s names for `id`: `HEAD -> main, tag: v1, origin/main`.
fn decorations(repo: &Repository, id: Oid) -> String {
    let head = repo.head().ok();
    let head_branch = head
        .as_ref()
        .filter(|h| h.is_branch())
        .and_then(|h| h.name().ok().map(str::to_owned));
    let mut names = Vec::new();
    if head.as_ref().and_then(|h| h.target()) == Some(id) {
        names.push(match &head_branch {
            Some(b) => format!("HEAD -> {}", b.trim_start_matches("refs/heads/")),
            None => "HEAD".to_owned(),
        });
    }
    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            let Some(name) = r.name().ok().map(str::to_owned) else {
                continue;
            };
            if Some(&name) == head_branch.as_ref()
                || r.peel_to_commit().map(|c| c.id()).ok() != Some(id)
            {
                continue;
            }
            if let Some(b) = name.strip_prefix("refs/heads/") {
                names.push(b.to_owned());
            } else if let Some(b) = name.strip_prefix("refs/remotes/") {
                names.push(b.to_owned());
            } else if let Some(t) = name.strip_prefix("refs/tags/") {
                names.push(format!("tag: {t}"));
            }
        }
    }
    names.join(", ")
}

/// Read an untracked file for `--add-file`: its name, git mode and content.
pub fn add_file(path: &Path) -> Result<(String, i32, Vec<u8>), GitError> {
    let data = std::fs::read(path)?;
    #[cfg(unix)]
    let exec = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path)?.permissions())
        & 0o111
        != 0;
    #[cfg(not(unix))]
    let exec = false;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| GitError::Other(format!("not a file: {}", path.display())))?;
    Ok((name, if exec { 0o100755 } else { 0o100644 }, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_follow_gitattributes() {
        let re = |p: &str| regex::Regex::new(&glob(p)).unwrap();
        assert!(re("*.txt").is_match("a.txt"));
        assert!(!re("*.txt").is_match("d/a.txt"));
        assert!(re("docs/**").is_match("docs/a/b.md"));
        assert!(re("**/tmp").is_match("x/y/tmp"));
        assert!(re("[!a]b").is_match("cb"));
    }
}
