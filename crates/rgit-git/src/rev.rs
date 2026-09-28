//! git's revision syntax on top of libgit2's revparse: index entries
//! (`:path`, `:<stage>:path`), paths relative to the current folder (`./`,
//! `../`) after `<rev>:` or `:`, and `@{push}`.

use std::path::Path;

use git2::{Error, ErrorCode, Object, Oid, Repository};

/// [`Repository::revparse_single`] with the git forms libgit2 lacks.
pub trait RevParse {
    fn rev_single(&self, spec: &str) -> Result<Object<'_>, Error>;
}

impl RevParse for Repository {
    fn rev_single(&self, spec: &str) -> Result<Object<'_>, Error> {
        single(self, spec)
    }
}

fn single<'r>(repo: &'r Repository, spec: &str) -> Result<Object<'r>, Error> {
    if let Some(rest) = spec.strip_prefix(':')
        && !rest.is_empty()
        && !rest.starts_with('/')
    {
        let (stage, path) = match rest.split_once(':') {
            Some((n @ ("0" | "1" | "2" | "3"), p)) => (n.parse().unwrap_or(0), p),
            _ => (0, rest),
        };
        let path = relative(repo, path);
        return match repo.index()?.get_path(Path::new(&path), stage) {
            Some(e) => repo.find_object(e.id, None),
            None => Err(Error::from_str(&format!(
                "path '{path}' does not exist in the index"
            ))),
        };
    }
    if let Some(at) = split_path(spec) {
        let (rev, path) = spec.split_at(at);
        let path = &path[1..];
        if path.starts_with("./") || path.starts_with("../") || path == "." || path == ".." {
            let path = relative(repo, path);
            let tree = single(repo, rev)?.peel_to_tree()?;
            return if path.is_empty() {
                Ok(tree.into_object())
            } else {
                tree.get_path(Path::new(&path))?.to_object(repo)
            };
        }
        if rev.contains("@{push}") {
            let tree = single(repo, rev)?.peel_to_tree()?;
            return tree.get_path(Path::new(path))?.to_object(repo);
        }
    }
    if let Some(pat) = spec.strip_prefix(":/")
        && pat.starts_with('!')
    {
        let mut tips: Vec<Oid> = repo
            .references()?
            .flatten()
            .filter_map(|r| r.peel_to_commit().ok().map(|c| c.id()))
            .collect();
        tips.extend(repo.head().ok().and_then(|h| h.target()));
        return find_message(repo, &tips, pat);
    }
    if let Some(at) = spec.find("^{/")
        && let Some(end) = spec[at..].find('}')
    {
        let pat = &spec[at + 3..at + end];
        if pat.is_empty() || pat.starts_with('!') {
            let start = single(repo, &spec[..at])?.peel_to_commit()?.id();
            let found = find_message(repo, &[start], pat)?;
            let rest = &spec[at + end + 1..];
            return if rest.is_empty() {
                Ok(found)
            } else {
                single(repo, &format!("{}{rest}", found.id()))
            };
        }
    }
    if let Some((branch, rest)) = spec.split_once("@{push}") {
        let target = push_ref(repo, branch)?;
        return repo.revparse_single(&format!("{target}{rest}"));
    }
    repo.revparse_single(spec)
        .map_err(|e| upstream_error(repo, spec).unwrap_or(e))
}

/// The commit a `<rev>^!`, `<rev>^@` or `<rev>^-<n>` range starts from, or
/// `rev` itself.
pub fn range_base(rev: &str) -> &str {
    if let Some(base) = rev.strip_suffix("^!").or_else(|| rev.strip_suffix("^@")) {
        return base;
    }
    match rev.rsplit_once("^-") {
        Some((base, n)) if n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => rev,
    }
}

/// The newest commit from `tips` whose message matches `pat`, as git's
/// get_oid_oneline reads it: `!-` negates, `!!` is a literal `!`.
fn find_message<'r>(repo: &'r Repository, tips: &[Oid], pat: &str) -> Result<Object<'r>, Error> {
    let (negate, re) = match pat.strip_prefix('!') {
        Some(p) if p.starts_with('!') => (false, p),
        Some(p) if p.starts_with('-') => (true, &p[1..]),
        Some(_) => return Err(Error::from_str(&format!("invalid search pattern '{pat}'"))),
        None => (false, pat),
    };
    let re = regex::Regex::new(&crate::plumbing::basic_to_extended(re))
        .map_err(|e| Error::from_str(&e.to_string()))?;
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TIME)?;
    for id in tips {
        walk.push(*id)?;
    }
    for id in walk {
        let commit = repo.find_commit(id?)?;
        let msg = String::from_utf8_lossy(commit.message_bytes()).into_owned();
        if re.is_match(&msg) != negate {
            return Ok(commit.into_object());
        }
    }
    Err(Error::from_str(&format!(
        "no commit message matches '{pat}'"
    )))
}

/// Where `<rev>:<path>` splits: the first `:` outside `{...}`.
fn split_path(spec: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, b) in spec.bytes().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' if depth > 0 => depth -= 1,
            b':' if depth == 0 => return (i > 0).then_some(i),
            _ => {}
        }
    }
    None
}

/// `path` from the top of the work tree when it starts with `./` or `../`,
/// as git reads it relative to the current folder.
fn relative(repo: &Repository, path: &str) -> String {
    if !(path.starts_with("./") || path.starts_with("../") || path == "." || path == "..") {
        return path.to_owned();
    }
    let prefix = repo
        .workdir()
        .and_then(|top| {
            let top = top.canonicalize().ok()?;
            let cwd = std::env::current_dir().ok()?.canonicalize().ok()?;
            Some(cwd.strip_prefix(top).ok()?.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let mut parts: Vec<&str> = prefix.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// The short name of the branch `name` means (`""` or `HEAD` is the current one).
fn branch_name(repo: &Repository, name: &str) -> Result<String, Error> {
    if name.is_empty() || name == "HEAD" {
        let head = repo.find_reference("HEAD")?;
        return match head.symbolic_target()? {
            Some(t) => Ok(t.strip_prefix("refs/heads/").unwrap_or(t).to_owned()),
            None => Err(Error::from_str("HEAD does not point to a branch")),
        };
    }
    if name.starts_with("@{-") {
        let (_, r) = repo.revparse_ext(name)?;
        return Ok(r
            .and_then(|r| r.shorthand().ok().map(str::to_owned))
            .unwrap_or_else(|| name.to_owned()));
    }
    Ok(name.strip_prefix("refs/heads/").unwrap_or(name).to_owned())
}

/// git's messages for `@{upstream}` without one.
fn upstream_error(repo: &Repository, spec: &str) -> Option<Error> {
    let at = ["@{u}", "@{upstream}", "@{U}", "@{UPSTREAM}"]
        .iter()
        .find_map(|u| spec.find(u))?;
    let name = branch_name(repo, &spec[..at]).ok()?;
    if repo.find_reference(&format!("refs/heads/{name}")).is_err() {
        return Some(Error::from_str(&format!("no such branch: '{name}'")));
    }
    let cfg = repo.config().ok()?;
    let merge = cfg.get_string(&format!("branch.{name}.merge")).ok();
    let remote = cfg.get_string(&format!("branch.{name}.remote")).ok();
    Some(Error::from_str(&match (merge, remote) {
        (Some(merge), Some(remote)) => format!(
            "upstream branch '{merge}' not stored as a remote-tracking branch of '{remote}'"
        ),
        _ => format!("no upstream configured for branch '{name}'"),
    }))
}

/// The remote-tracking ref `git push` from `branch` would update, as git's
/// branch_get_push works it out from push.default.
pub(crate) fn push_ref(repo: &Repository, branch: &str) -> Result<String, Error> {
    let name = branch_name(repo, branch)?;
    let cfg = repo.config()?;
    let get = |k: &str| cfg.get_string(k).ok();
    let remote = get(&format!("branch.{name}.pushRemote"))
        .or_else(|| get("remote.pushDefault"))
        .or_else(|| get(&format!("branch.{name}.remote")))
        .unwrap_or_else(|| "origin".to_owned());
    let upstream = || -> Result<String, Error> {
        match repo.branch_upstream_name(&format!("refs/heads/{name}")) {
            Ok(up) => Ok(String::from_utf8_lossy(&up).into_owned()),
            Err(e) => Err(upstream_error(repo, &format!("{name}@{{u}}")).unwrap_or(e)),
        }
    };
    let current = || -> Result<String, Error> {
        let target = format!("refs/remotes/{remote}/{name}");
        match repo.find_reference(&target) {
            Ok(_) => Ok(target),
            Err(e) if e.code() == ErrorCode::NotFound => Err(Error::from_str(&format!(
                "push destination 'refs/heads/{name}' on remote '{remote}' has no local tracking branch"
            ))),
            Err(e) => Err(e),
        }
    };
    match get("push.default").as_deref().unwrap_or("simple") {
        "nothing" => Err(Error::from_str(
            "push has no destination (push.default is 'nothing')",
        )),
        "upstream" | "tracking" => {
            if get(&format!("branch.{name}.merge")).is_none() {
                return Err(Error::from_str(
                    "push has no destination (push.default is 'upstream')",
                ));
            }
            upstream()
        }
        "simple" => {
            let up = upstream()?;
            if up != current()? {
                return Err(Error::from_str(
                    "cannot resolve 'simple' push to a single destination",
                ));
            }
            Ok(up)
        }
        _ => current(),
    }
}
