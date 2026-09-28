//! `git describe`: the nearest tag (or ref) a commit descends from, found
//! with git's own walk so the depth and the choice between tags match.

use crate::rev::RevParse;
use std::collections::HashMap;

use git2::{Oid, Repository};

use crate::GitError;

/// How [`crate::GitBackend::describe`] names a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescribeOptions {
    /// Any ref, not just annotated tags (git's `--all`).
    pub all: bool,
    /// Lightweight tags too (git's `--tags`).
    pub tags: bool,
    /// `tag-N-gid` even on a tag.
    pub long: bool,
    /// The abbreviated id when nothing describes the commit.
    pub always: bool,
    /// Digits of the id; 0 prints only the name, None git's default.
    pub abbrev: Option<u32>,
    pub first_parent: bool,
    /// How many tags to consider (git's default 10; 0: exact matches only).
    pub candidates: u32,
    /// Only names matching one of these globs, and none of `excludes`.
    pub matches: Vec<String>,
    pub excludes: Vec<String>,
    /// Suffixes for a working tree with changes, or one git cannot read.
    pub dirty: Option<String>,
    pub broken: Option<String>,
}

impl Default for DescribeOptions {
    fn default() -> Self {
        DescribeOptions {
            all: false,
            tags: false,
            long: false,
            always: false,
            abbrev: None,
            first_parent: false,
            candidates: 10,
            matches: Vec::new(),
            excludes: Vec::new(),
            dirty: None,
            broken: None,
        }
    }
}

struct Name {
    path: String,
    prio: u8,
    oid: Oid,
    /// An annotated tag's own name and date.
    tag: Option<(String, i64)>,
}

fn err(message: impl Into<String>) -> GitError {
    GitError::Other(message.into())
}

pub(crate) fn describe(
    repo: &Repository,
    rev: &str,
    o: &DescribeOptions,
) -> Result<String, GitError> {
    let target = repo.rev_single(rev)?.peel_to_commit()?.id();
    let names = names(repo, o)?;
    if names.is_empty() && !o.always {
        return Err(err("No names found, cannot describe anything."));
    }
    let suffix = match (&o.dirty, &o.broken) {
        (None, None) => String::new(),
        (dirty, broken) => match changed(repo) {
            Ok(true) => dirty
                .clone()
                .or_else(|| Some("-dirty".into()))
                .unwrap_or_default(),
            Ok(false) => String::new(),
            Err(e) => broken.clone().ok_or(e)?,
        },
    };
    let abbrev = |id: Oid, n: u32| crate::plumbing::abbrev(repo, &id.to_string(), n as usize);
    let hex = |id: Oid| -> Result<String, GitError> {
        Ok(match o.abbrev {
            Some(0) => String::new(),
            Some(n) => abbrev(id, n.max(4))?,
            None => abbrev(id, 0)?,
        })
    };
    if let Some(n) = names.get(&target)
        && (o.tags || o.all || n.prio == 2)
    {
        let (name, misnamed) = name(n, o.all);
        let mut out = name;
        if misnamed || o.long {
            let id = match &n.tag {
                Some(_) => repo.find_tag(n.oid)?.target_id(),
                None => target,
            };
            out.push_str(&format!("-0-g{}", hex(id)?));
        }
        return Ok(out + &suffix);
    }
    if o.candidates == 0 {
        return Err(err(format!("no tag exactly matches '{target}'")));
    }

    struct Candidate<'n> {
        name: &'n Name,
        depth: usize,
        flag: u64,
        order: usize,
    }
    let max = o.candidates.min(62) as usize;
    let mut info: HashMap<Oid, (i64, Vec<Oid>)> = HashMap::new();
    let mut commit = |id: Oid| -> Result<(i64, Vec<Oid>), GitError> {
        if let Some(c) = info.get(&id) {
            return Ok(c.clone());
        }
        let c = repo.find_commit(id)?;
        let v = (c.time().seconds(), c.parent_ids().collect::<Vec<_>>());
        info.insert(id, v.clone());
        Ok(v)
    };
    // Newest first; commits of one date in the order they were queued.
    let insert = |list: &mut Vec<(Oid, i64)>, id: Oid, date: i64| {
        let at = list.partition_point(|(_, d)| *d >= date);
        list.insert(at, (id, date));
    };
    const SEEN: u64 = 1;
    let mut flags: HashMap<Oid, u64> = HashMap::from([(target, SEEN)]);
    let mut list = vec![(target, commit(target)?.0)];
    let mut found: Vec<Candidate> = Vec::new();
    let (mut annotated, mut unannotated, mut seen) = (0, 0, 0usize);
    let mut gave_up = None;
    while !list.is_empty() {
        let (c, date) = list.remove(0);
        seen += 1;
        // Stop once every name is a candidate: no other can turn up.
        if found.len() == max || found.len() == names.len() {
            gave_up = Some((c, date));
            break;
        }
        if let Some(n) = names.get(&c) {
            if !o.tags && !o.all && n.prio < 2 {
                unannotated += 1;
            } else {
                let order = found.len() + 1;
                let flag = 1u64 << order;
                found.push(Candidate {
                    name: n,
                    depth: seen - 1,
                    flag,
                    order,
                });
                *flags.entry(c).or_default() |= flag;
                if n.prio == 2 {
                    annotated += 1;
                }
            }
        }
        let cf = flags.get(&c).copied().unwrap_or(0);
        for t in &mut found {
            if cf & t.flag == 0 {
                t.depth += 1;
            }
        }
        if annotated > 0 && list.is_empty() {
            let best = found.iter().map(|t| t.depth).min().unwrap_or(usize::MAX);
            let within = found
                .iter()
                .filter(|t| t.depth == best)
                .fold(0, |w, t| w | t.flag);
            if cf & within == within {
                break;
            }
        }
        let parents = commit(c)?.1;
        for p in parents
            .iter()
            .take(if o.first_parent { 1 } else { usize::MAX })
        {
            let pf = flags.entry(*p).or_default();
            if *pf & SEEN == 0 {
                *pf |= cf;
                let d = commit(*p)?.0;
                insert(&mut list, *p, d);
            } else {
                *pf |= cf;
            }
        }
    }
    if found.is_empty() {
        if o.always {
            let id = match o.abbrev {
                Some(0) => target.to_string(),
                _ => hex(target)?,
            };
            return Ok(id + &suffix);
        }
        return Err(err(if unannotated > 0 {
            format!(
                "No annotated tags can describe '{target}'.\n\
                 However, there were unannotated tags: try --tags."
            )
        } else {
            format!("No tags can describe '{target}'.\nTry --always, or create some tags.")
        }));
    }
    found.sort_by_key(|t| (t.depth, t.order));
    if let Some((c, date)) = gave_up {
        insert(&mut list, c, date);
    }
    // Count the commits the best name does not reach, until it reaches
    // every commit left.
    let best_flag = found[0].flag;
    let mut extra = 0;
    while !list.is_empty() {
        let (c, _) = list.remove(0);
        let cf = flags.get(&c).copied().unwrap_or(0);
        if cf & best_flag != 0 {
            if list
                .iter()
                .all(|(i, _)| flags.get(i).copied().unwrap_or(0) & best_flag != 0)
            {
                break;
            }
        } else {
            extra += 1;
        }
        let parents = commit(c)?.1;
        for p in parents
            .iter()
            .take(if o.first_parent { 1 } else { usize::MAX })
        {
            let pf = flags.entry(*p).or_default();
            let fresh = *pf & SEEN == 0;
            *pf |= cf;
            if fresh {
                let d = commit(*p)?.0;
                insert(&mut list, *p, d);
            }
        }
    }
    let best = &found[0];
    let (mut out, misnamed) = name(best.name, o.all);
    if misnamed || o.abbrev != Some(0) {
        out.push_str(&format!("-{}-g{}", best.depth + extra, hex(target)?));
    }
    Ok(out + &suffix)
}

/// The name to print, and whether an annotated tag calls itself otherwise
/// (git then warns and always adds the suffix).
fn name(n: &Name, all: bool) -> (String, bool) {
    match &n.tag {
        Some((tag, _)) => {
            let path = if all {
                n.path.strip_prefix("tags/").unwrap_or(&n.path)
            } else {
                &n.path
            };
            let misnamed = tag != path;
            if misnamed {
                eprintln!("warning: tag '{path}' is externally known as '{tag}'");
            }
            (format!("{}{tag}", if all { "tags/" } else { "" }), misnamed)
        }
        None => (n.path.clone(), false),
    }
}

/// The names describe may use, by the object they peel to: for each object
/// the best tag (annotated over lightweight over other refs, the newer of
/// two annotated tags).
fn names(repo: &Repository, o: &DescribeOptions) -> Result<HashMap<Oid, Name>, GitError> {
    let mut refs: Vec<(String, Oid)> = Vec::new();
    for r in repo.references()? {
        let r = r?;
        let (Ok(name), Ok(r)) = (r.name().map(str::to_owned), r.resolve()) else {
            continue;
        };
        if let Some(id) = r.target() {
            refs.push((name, id));
        }
    }
    refs.sort();
    let wild = crate::apply::wildmatch;
    let mut out: HashMap<Oid, Name> = HashMap::new();
    for (name, oid) in refs {
        let filtered = !o.matches.is_empty() || !o.excludes.is_empty();
        let (is_tag, to_match) = match name.strip_prefix("refs/tags/") {
            Some(t) => (true, t),
            None if !o.all => continue,
            None if !filtered => (false, ""),
            None => match name
                .strip_prefix("refs/heads/")
                .or_else(|| name.strip_prefix("refs/remotes/"))
            {
                Some(t) => (false, t),
                None => continue,
            },
        };
        if o.excludes.iter().any(|p| wild(p, to_match))
            || !o.matches.is_empty() && !o.matches.iter().any(|p| wild(p, to_match))
        {
            continue;
        }
        let obj = repo.find_object(oid, None)?;
        let peeled = match obj.kind() {
            Some(git2::ObjectType::Tag) => obj.peel(git2::ObjectType::Any)?.id(),
            _ => oid,
        };
        let annotated = peeled != oid;
        let prio = if annotated {
            2
        } else if is_tag {
            1
        } else {
            0
        };
        let tag = if annotated {
            repo.find_tag(oid).ok().map(|t| {
                let date = t.tagger().map_or(0, |s| s.when().seconds());
                (String::from_utf8_lossy(t.name_bytes()).into_owned(), date)
            })
        } else {
            None
        };
        let path = name[if o.all { 5 } else { 10 }..].to_owned();
        let replace = match out.get(&peeled) {
            None => true,
            Some(e) if e.prio < prio => true,
            Some(e) if e.prio == 2 && prio == 2 => match (&e.tag, &tag) {
                (Some((_, old)), Some((_, new))) => old < new,
                (None, _) => true,
                _ => false,
            },
            _ => false,
        };
        if replace {
            out.insert(
                peeled,
                Name {
                    path,
                    prio,
                    oid,
                    tag,
                },
            );
        }
    }
    Ok(out)
}

/// Whether tracked files differ from HEAD, in the index or the working tree.
fn changed(repo: &Repository) -> Result<bool, GitError> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(false).include_ignored(false);
    Ok(!repo.statuses(Some(&mut opts))?.is_empty())
}
