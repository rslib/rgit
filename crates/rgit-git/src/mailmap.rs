//! git's mailmap, parsed and applied as mailmap.c does, for
//! `git check-mailmap`.

use crate::rev::RevParse;
use std::collections::HashMap;
use std::path::Path;

use git2::Repository;

use crate::GitError;

type Target = (Option<String>, Option<String>);

#[derive(Default)]
struct Entry {
    default: Target,
    by_name: HashMap<String, Target>,
}

/// Mappings keyed by lowercased email, as git compares them.
#[derive(Default)]
pub struct Mailmap(HashMap<String, Entry>);

/// `Name <email>` at the start of `s`: the trimmed name (none when empty),
/// the email, and what follows.
fn name_and_email(s: &str) -> Option<(Option<&str>, &str, &str)> {
    let lt = s.find('<')?;
    let gt = lt + s[lt..].find('>')?;
    let name = s[..lt].trim();
    Some((
        (!name.is_empty()).then_some(name),
        &s[lt + 1..gt],
        &s[gt + 1..],
    ))
}

impl Mailmap {
    /// Add the lines of a mailmap file; later lines win.
    pub fn read(&mut self, text: &str) {
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let Some((name1, email1, rest)) = name_and_email(line) else {
                continue;
            };
            let (new, old) = match name_and_email(rest) {
                Some((name2, email2, _)) => ((name1, Some(email1)), (name2, email2)),
                None => ((name1, None), (None, email1)),
            };
            let target = (new.0.map(str::to_owned), new.1.map(str::to_owned));
            let entry = self.0.entry(old.1.to_lowercase()).or_default();
            match old.0 {
                Some(name) => {
                    entry.by_name.insert(name.to_lowercase(), target);
                }
                None => {
                    if target.0.is_some() {
                        entry.default.0 = target.0;
                    }
                    if target.1.is_some() {
                        entry.default.1 = target.1;
                    }
                }
            }
        }
    }

    /// `name` and `email` as the mailmap maps them.
    pub fn map(&self, name: &str, email: &str) -> (String, String) {
        let found = self.0.get(&email.to_lowercase()).and_then(|e| {
            e.by_name
                .get(&name.to_lowercase())
                .or(Some(&e.default))
                .filter(|t| t.0.is_some() || t.1.is_some())
        });
        match found {
            Some((n, e)) => (
                n.clone().unwrap_or_else(|| name.to_owned()),
                e.clone().unwrap_or_else(|| email.to_owned()),
            ),
            None => (name.to_owned(), email.to_owned()),
        }
    }

    /// The repository's mailmap: .mailmap in the work tree, mailmap.blob
    /// (HEAD:.mailmap in a bare repository), then mailmap.file.
    pub fn load(git_dir: &Path) -> Result<Self, GitError> {
        let repo = Repository::open(git_dir)?;
        let mut map = Mailmap::default();
        if let Some(dir) = repo.workdir() {
            map.read(&std::fs::read_to_string(dir.join(".mailmap")).unwrap_or_default());
        }
        let cfg = repo.config()?;
        let blob = cfg
            .get_string("mailmap.blob")
            .ok()
            .or_else(|| repo.is_bare().then(|| "HEAD:.mailmap".to_owned()));
        if let Some(blob) = blob {
            map.read_blob(&repo, &blob);
        }
        if let Ok(file) = cfg.get_path("mailmap.file") {
            map.read(&std::fs::read_to_string(file).unwrap_or_default());
        }
        Ok(map)
    }

    /// Add the mailmap in the blob `spec` names, if any.
    pub fn read_blob(&mut self, repo: &Repository, spec: &str) {
        if let Ok(blob) = repo.rev_single(spec).and_then(|o| o.peel_to_blob()) {
            self.read(&String::from_utf8_lossy(blob.content()));
        }
    }
}

/// `git check-mailmap`: each contact (`Name <email>`, `<email>` or a bare
/// email) as the mailmap, plus `files` and `blobs`, maps it; one per line.
pub fn check_mailmap(
    git_dir: &Path,
    contacts: &[String],
    files: &[String],
    blobs: &[String],
) -> Result<String, GitError> {
    let repo = Repository::open(git_dir)?;
    let mut map = Mailmap::load(git_dir)?;
    for f in files {
        map.read(&std::fs::read_to_string(f).unwrap_or_default());
    }
    for b in blobs {
        map.read_blob(&repo, b);
    }
    let mut out = String::new();
    for contact in contacts {
        // git's split_ident_line: the name keeps leading blanks.
        let (name, email) = match name_and_email(contact) {
            Some((_, email, _)) => (contact[..contact.find('<').unwrap_or(0)].trim_end(), email),
            None => ("", contact.as_str()),
        };
        let (name, email) = map.map(name, email);
        if !name.is_empty() {
            out.push_str(&name);
            out.push(' ');
        }
        out.push_str(&format!("<{email}>\n"));
    }
    Ok(out)
}
