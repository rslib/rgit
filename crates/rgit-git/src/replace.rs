//! `git replace`: refs under refs/replace/ that stand one object in for
//! another, made from two objects, a commit with new parents (--graft), an
//! edited object (--edit) or the old info/grafts file.

use crate::rev::RevParse;
use std::path::Path;

use git2::{ObjectType, Oid, Repository};

use crate::GitError;
use crate::index_ops::Report;

fn err(message: impl Into<String>) -> GitError {
    GitError::Other(message.into())
}

/// Where replace refs live: GIT_REPLACE_REF_BASE, else refs/replace/.
fn base() -> String {
    let mut b = std::env::var("GIT_REPLACE_REF_BASE")
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "refs/replace/".to_owned());
    if !b.ends_with('/') {
        b.push('/');
    }
    b
}

fn resolve(repo: &Repository, name: &str) -> Result<Oid, GitError> {
    repo.rev_single(name)
        .map(|o| o.id())
        .map_err(|_| err(format!("failed to resolve '{name}' as a valid ref")))
}

/// `id` after its replacements, as git reads objects.
fn replaced(repo: &Repository, mut id: Oid) -> Oid {
    for _ in 0..5 {
        match repo.refname_to_id(&format!("{}{id}", base())) {
            Ok(next) => id = next,
            Err(_) => break,
        }
    }
    id
}

fn kind(repo: &Repository, id: Oid) -> Result<ObjectType, GitError> {
    let id = replaced(repo, id);
    Ok(repo.odb()?.read_header(id)?.1)
}

/// Point `refs/replace/<object>` at `repl`, as git's replace_object_oid.
fn set(
    repo: &Repository,
    (object_name, object): (&str, Oid),
    (repl_name, repl): (&str, Oid),
    force: bool,
) -> Result<(), GitError> {
    let (ok, rk) = (kind(repo, object)?, kind(repo, repl)?);
    if !force && ok != rk {
        return Err(err(format!(
            "Objects must be of the same type.\n'{object_name}' points to a replaced object of \
             type '{ok}'\nwhile '{repl_name}' points to a replacement object of type '{rk}'."
        )));
    }
    let name = format!("{}{object}", base());
    if !force && repo.refname_to_id(&name).is_ok() {
        return Err(err(format!("replace ref '{name}' already exists")));
    }
    repo.reference(&name, repl, true, "")?;
    Ok(())
}

/// `git replace [-f] <object> <replacement>`.
pub fn replace_object(
    git_dir: &Path,
    object: &str,
    repl: &str,
    force: bool,
) -> Result<(), GitError> {
    let repo = Repository::open(git_dir)?;
    let (o, r) = (resolve(&repo, object)?, resolve(&repo, repl)?);
    set(&repo, (object, o), (repl, r), force)
}

/// `git replace -l [<pattern>] [--format=<fmt>]`.
pub fn replace_list(
    git_dir: &Path,
    pattern: Option<&str>,
    format: Option<&str>,
) -> Result<String, GitError> {
    let format = format.unwrap_or("short");
    if !matches!(format, "short" | "medium" | "long") {
        return Err(err(format!(
            "invalid replace format '{format}'\nvalid formats are 'short', 'medium' and 'long'"
        )));
    }
    let repo = Repository::open(git_dir)?;
    let base = base();
    let mut refs: Vec<(String, Oid)> = repo
        .references_glob(&format!("{base}*"))?
        .flatten()
        .filter_map(|r| Some((r.name().ok()?.to_owned(), r.target()?)))
        .collect();
    refs.sort();
    let mut out = String::new();
    for (name, target) in refs {
        let short = &name[base.len()..];
        if !crate::apply::wildmatch(pattern.unwrap_or("*"), short) {
            continue;
        }
        match format {
            "short" => out.push_str(&format!("{short}\n")),
            "medium" => out.push_str(&format!("{short} -> {target}\n")),
            _ => {
                let t = repo.odb()?.read_header(target).map(|h| h.1.to_string());
                let t = t.unwrap_or_else(|_| "unknown".into());
                out.push_str(&format!("{short} ({t}) -> {target} ({t})\n"));
            }
        }
    }
    Ok(out)
}

/// `git replace -d <object>...`: stdout names what went, stderr what could
/// not; failed when any could not.
pub fn replace_delete(git_dir: &Path, names: &[String]) -> Result<Report, GitError> {
    let repo = Repository::open(git_dir)?;
    let mut r = Report::default();
    for name in names {
        let id = match resolve(&repo, name) {
            Ok(id) => id,
            Err(e) => {
                r.err.push_str(&format!("error: {e}\n"));
                r.failed = true;
                continue;
            }
        };
        match repo.find_reference(&format!("{}{id}", base())) {
            Ok(mut reference) => {
                reference.delete()?;
                r.out.push_str(&format!("Deleted replace ref '{id}'\n"));
            }
            Err(_) => {
                r.err
                    .push_str(&format!("error: replace ref '{id}' not found\n"));
                r.failed = true;
            }
        }
    }
    Ok(r)
}

/// A commit header's fields: each a name and its (unfolded) value.
fn headers(head: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        match (line.strip_prefix(' '), out.last_mut()) {
            (Some(more), Some((_, v))) => {
                v.push('\n');
                v.push_str(more);
            }
            _ => {
                let (k, v) = line.split_once(' ').unwrap_or((line, ""));
                out.push((k.to_owned(), v.to_owned()));
            }
        }
    }
    out
}

/// `git replace --graft <commit> [<parent>...]`; warnings go in the report.
/// `gentle` (for --convert-graft-file) only warns when nothing would change.
pub fn replace_graft(
    git_dir: &Path,
    args: &[String],
    force: bool,
    gentle: bool,
) -> Result<Report, GitError> {
    let repo = Repository::open(git_dir)?;
    let name = &args[0];
    let old = repo
        .rev_single(name)
        .map_err(|_| err(format!("not a valid object name: '{name}'")))?
        .id();
    let old = repo
        .find_object(old, None)?
        .peel_to_commit()
        .map_err(|_| err(format!("could not parse {name}")))?
        .id();
    let mut parents = Vec::new();
    for p in &args[1..] {
        let id = repo
            .rev_single(p)
            .map_err(|_| err(format!("not a valid object name: '{p}'")))?;
        let c = id
            .peel_to_commit()
            .map_err(|_| err(format!("could not parse {p} as a commit")))?;
        parents.push(c.id());
    }
    let odb = repo.odb()?;
    let raw = odb.read(old)?;
    let text = String::from_utf8_lossy(raw.data()).into_owned();
    let (head, body) = text.split_once("\n\n").unwrap_or((&text, ""));
    let mut report = Report::default();
    let mut out = String::new();
    let mut signed = false;
    for (k, v) in headers(head) {
        match k.as_str() {
            "parent" => continue,
            "gpgsig" | "gpgsig-sha256" => {
                signed = true;
                continue;
            }
            "mergetag" => {
                let tag = Oid::hash_object(ObjectType::Tag, format!("{v}\n").as_bytes())?;
                let tagged = v
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("object "))
                    .and_then(|h| Oid::from_str(h).ok());
                if !tagged.is_some_and(|t| parents.contains(&t)) {
                    return Err(err(format!(
                        "original commit '{name}' contains mergetag '{tag}' that is discarded; \
                         use --edit instead of --graft"
                    )));
                }
            }
            _ => {}
        }
        out.push_str(&k);
        out.push(' ');
        out.push_str(&v.replace('\n', "\n "));
        out.push('\n');
        if k == "tree" {
            for p in &parents {
                out.push_str(&format!("parent {p}\n"));
            }
        }
    }
    if signed {
        report.err.push_str(&format!(
            "warning: the original commit '{name}' has a gpg signature\n\
             warning: the signature will be removed in the replacement commit!\n"
        ));
    }
    out.push('\n');
    out.push_str(body);
    let new = repo.odb()?.write(ObjectType::Commit, out.as_bytes())?;
    if new == old {
        if gentle {
            report
                .err
                .push_str(&format!("warning: graft for '{old}' unnecessary\n"));
            return Ok(report);
        }
        return Err(err(format!(
            "new commit is the same as the old one: '{old}'"
        )));
    }
    let old_id = repo.rev_single(name)?.id();
    set(&repo, (name, old_id), ("replacement", new), force)?;
    Ok(report)
}

/// `git replace --convert-graft-file`: each line of info/grafts as a
/// --graft; the file goes when all convert.
pub fn replace_convert_grafts(git_dir: &Path, force: bool) -> Result<Report, GitError> {
    let repo = Repository::open(git_dir)?;
    let file = repo.commondir().join("info/grafts");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut report = Report::default();
    let mut bad = String::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        match replace_graft(git_dir, &args, force, true) {
            Ok(r) => report.err.push_str(&r.err),
            Err(e) => {
                report.err.push_str(&format!("error: {e}\n"));
                bad.push_str(&format!("\n\t{line}"));
            }
        }
    }
    if bad.is_empty() {
        let _ = std::fs::remove_file(&file);
    } else {
        report.err.push_str(&format!(
            "warning: could not convert the following graft(s):\n{bad}\n"
        ));
        report.failed = true;
    }
    Ok(report)
}

/// The object `name` names for `git replace --edit`: its id, type and
/// content as `cat-file -p` shows it (raw bytes with `raw`).
pub fn replace_edit_export(
    git_dir: &Path,
    name: &str,
    force: bool,
    raw: bool,
) -> Result<(Oid, ObjectType, Vec<u8>), GitError> {
    let repo = Repository::open(git_dir)?;
    let old = repo
        .rev_single(name)
        .map_err(|_| err(format!("not a valid object name: '{name}'")))?
        .id();
    let refname = format!("{}{old}", base());
    if !force && repo.refname_to_id(&refname).is_ok() {
        return Err(err(format!("replace ref '{refname}' already exists")));
    }
    let odb = repo.odb()?;
    let obj = odb.read(replaced(&repo, old))?;
    let kind = obj.kind();
    if kind != ObjectType::Tree || raw {
        return Ok((old, kind, obj.data().to_vec()));
    }
    let tree = repo.find_tree(obj.id())?;
    let mut out = Vec::new();
    for e in tree.iter() {
        let k = e.kind().map_or("blob", |k| k.str());
        out.extend(format!("{:06o} {k} {}\t", e.filemode(), e.id()).as_bytes());
        out.extend(e.name_bytes());
        out.push(b'\n');
    }
    Ok((old, kind, out))
}

/// Store the edited object and make it the replacement for `old`.
pub fn replace_edit_import(
    git_dir: &Path,
    old: Oid,
    kind: ObjectType,
    data: &[u8],
    raw: bool,
    force: bool,
) -> Result<(), GitError> {
    let repo = Repository::open(git_dir)?;
    let new = if kind == ObjectType::Tree && !raw {
        crate::index_ops::mktree(git_dir, data, false, false, false)?
    } else {
        crate::plumbing::hash_object(Some(git_dir), kind.str(), data, None, true, false)?
    };
    let new = Oid::from_str(new.trim())?;
    if new == old {
        return Err(err(format!(
            "new object is the same as the old one: '{old}'"
        )));
    }
    let hex = old.to_string();
    set(&repo, (&hex, old), ("replacement", new), force)
}
