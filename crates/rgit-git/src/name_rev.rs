//! `git name-rev`: name commits relative to the refs that reach them, with
//! git's tip order, traversal and tie-breaking (builtin/name-rev.c).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use git2::{ObjectType, Oid, Repository};

use crate::GitError;

const MERGE_TRAVERSAL_WEIGHT: u32 = 65535;
const CUTOFF_DATE_SLOP: i64 = 86400;

/// `git name-rev`'s options.
#[derive(Default)]
pub struct NameRevOpts {
    pub name_only: bool,
    pub tags: bool,
    pub refs: Vec<String>,
    pub exclude: Vec<String>,
    pub all: bool,
    pub annotate_stdin: Option<String>,
    pub undefined: bool,
    pub always: bool,
    pub peel_tag: bool,
}

struct Tip {
    oid: Oid,
    name: String,
    commit: Option<Oid>,
    date: i64,
    from_tag: bool,
    deref: bool,
}

#[derive(Clone)]
struct Name {
    tip: String,
    date: i64,
    generation: u32,
    distance: u32,
    from_tag: bool,
}

impl Name {
    fn worse_than(&self, date: i64, distance: u32, from_tag: bool) -> bool {
        if from_tag && self.from_tag {
            return self.date > date || self.date == date && self.distance > distance;
        }
        if self.from_tag != from_tag {
            return from_tag;
        }
        if self.distance != distance {
            return self.distance > distance;
        }
        self.date > date
    }
}

/// How far a `refs/...` path's tail matches `pattern`: the offset of the
/// first matching subpath.
fn subpath_matches(path: &str, pattern: &str) -> Option<usize> {
    let mut at = 0;
    loop {
        if crate::apply::wildmatch(pattern, &path[at..]) {
            return Some(at);
        }
        at += path[at..].find('/')? + 1;
    }
}

/// refs.c's `shorten_unambiguous_ref`, not strict.
fn shorten_unambiguous(repo: &Repository, name: &str) -> String {
    const RULES: [(&str, &str); 6] = [
        ("", ""),
        ("refs/", ""),
        ("refs/tags/", ""),
        ("refs/heads/", ""),
        ("refs/remotes/", ""),
        ("refs/remotes/", "/HEAD"),
    ];
    for i in (1..RULES.len()).rev() {
        let (pre, suf) = RULES[i];
        let Some(short) = name.strip_prefix(pre).and_then(|s| s.strip_suffix(suf)) else {
            continue;
        };
        if short.is_empty() {
            continue;
        }
        let taken = (0..i).any(|j| {
            let (p, s) = RULES[j];
            repo.find_reference(&format!("{p}{short}{s}")).is_ok()
        });
        if !taken {
            return short.to_owned();
        }
    }
    name.to_owned()
}

struct Graph<'r> {
    repo: &'r Repository,
    commits: HashMap<Oid, (i64, Vec<Oid>)>,
}

impl Graph<'_> {
    fn get(&mut self, id: Oid) -> Option<(i64, Vec<Oid>)> {
        if let Some(c) = self.commits.get(&id) {
            return Some(c.clone());
        }
        let c = self.repo.find_commit(id).ok()?;
        let v = (c.time().seconds(), c.parent_ids().collect());
        self.commits.insert(id, v.clone());
        Some(v)
    }
}

fn name_from(g: &mut Graph, names: &mut HashMap<Oid, Name>, tip: &Tip, start: Oid, cutoff: i64) {
    let Some((date, _)) = g.get(start) else {
        return;
    };
    if date < cutoff {
        return;
    }
    if names
        .get(&start)
        .is_some_and(|n| !n.worse_than(tip.date, 0, tip.from_tag))
    {
        return;
    }
    let tip_name = if tip.deref {
        format!("{}^0", tip.name)
    } else {
        tip.name.clone()
    };
    names.insert(
        start,
        Name {
            tip: tip_name,
            date: tip.date,
            generation: 0,
            distance: 0,
            from_tag: tip.from_tag,
        },
    );
    let mut stack = vec![start];
    while let Some(id) = stack.pop() {
        let name = names[&id].clone();
        let parents = g.get(id).map(|c| c.1).unwrap_or_default();
        let mut queue = Vec::new();
        for (k, p) in parents.into_iter().enumerate() {
            let Some((pdate, _)) = g.get(p) else {
                continue;
            };
            if pdate < cutoff {
                continue;
            }
            let (generation, distance) = if k > 0 {
                (0, name.distance + MERGE_TRAVERSAL_WEIGHT)
            } else {
                (name.generation + 1, name.distance + 1)
            };
            if names
                .get(&p)
                .is_some_and(|n| !n.worse_than(tip.date, distance, tip.from_tag))
            {
                continue;
            }
            let tip_name = if k > 0 {
                let base = name.tip.strip_suffix("^0").unwrap_or(&name.tip);
                if name.generation > 0 {
                    format!("{base}~{}^{}", name.generation, k + 1)
                } else {
                    format!("{base}^{}", k + 1)
                }
            } else {
                name.tip.clone()
            };
            names.insert(
                p,
                Name {
                    tip: tip_name,
                    date: tip.date,
                    generation,
                    distance,
                    from_tag: tip.from_tag,
                },
            );
            queue.push(p);
        }
        stack.extend(queue.into_iter().rev());
    }
}

fn rev_name(names: &HashMap<Oid, Name>, id: Oid) -> Option<String> {
    let n = names.get(&id)?;
    Some(if n.generation == 0 {
        n.tip.clone()
    } else {
        format!(
            "{}~{}",
            n.tip.strip_suffix("^0").unwrap_or(&n.tip),
            n.generation
        )
    })
}

/// `git name-rev`'s output for `revs` (or `--all`, or the text to annotate).
/// Unnamed revisions fail unless `--always` or `--undefined`.
pub fn name_rev(
    git_dir: &Path,
    o: &NameRevOpts,
    revs: &[String],
) -> Result<(String, Option<String>), GitError> {
    let repo = Repository::open(git_dir)?;
    let mut tips = Vec::new();
    let mut refs: Vec<(String, Oid)> = repo
        .references()?
        .flatten()
        .filter_map(|r| Some((r.name().ok()?.to_owned(), r.target()?)))
        .collect();
    refs.sort();
    for (path, oid) in refs {
        if o.tags && !path.starts_with("refs/tags/") {
            continue;
        }
        if o.exclude
            .iter()
            .any(|p| subpath_matches(&path, p).is_some())
        {
            continue;
        }
        let mut abbreviate = o.tags && o.name_only;
        if !o.refs.is_empty() {
            let hits: Vec<usize> = o
                .refs
                .iter()
                .filter_map(|p| subpath_matches(&path, p))
                .collect();
            if hits.is_empty() {
                continue;
            }
            abbreviate |= hits.iter().any(|&h| h > 0);
        }
        let mut obj = repo.find_object(oid, None).ok();
        let mut date = None;
        let mut deref = false;
        while let Some(tag) = obj.as_ref().and_then(|x| x.as_tag()) {
            date = Some(tag.tagger().map_or(0, |t| t.when().seconds()));
            let target = tag.target_id();
            obj = repo.find_object(target, None).ok();
            deref = true;
        }
        let commit = obj
            .as_ref()
            .filter(|x| x.kind() == Some(ObjectType::Commit))
            .map(|x| x.id());
        let from_tag = commit.is_some() && path.starts_with("refs/tags/");
        let date = match (date, commit) {
            (Some(d), _) => d,
            (None, Some(c)) => repo.find_commit(c)?.time().seconds(),
            (None, None) => i64::MAX,
        };
        let name = if abbreviate {
            shorten_unambiguous(&repo, &path)
        } else {
            let p = path.strip_prefix("refs/heads/").unwrap_or(&path);
            p.strip_prefix("refs/").unwrap_or(p).to_owned()
        };
        tips.push(Tip {
            oid,
            name,
            commit,
            date,
            from_tag,
            deref,
        });
    }

    let mut out = String::new();
    let mut args = Vec::new();
    let mut cutoff = i64::MAX;
    for rev in revs {
        let Ok(obj) = repo.revparse_single(rev) else {
            eprintln!("Could not get sha1 for {rev}. Skipping.");
            continue;
        };
        let commit = obj.peel_to_commit().ok();
        if let Some(c) = &commit {
            cutoff = cutoff.min(c.time().seconds());
        }
        let id = if o.peel_tag {
            match &commit {
                Some(c) => c.id(),
                None => {
                    eprintln!("Could not get commit for {rev}. Skipping.");
                    continue;
                }
            }
        } else {
            obj.id()
        };
        args.push((rev.clone(), id));
    }
    if o.all || o.annotate_stdin.is_some() || cutoff == i64::MAX {
        cutoff = i64::MIN;
    } else {
        cutoff -= CUTOFF_DATE_SLOP;
    }

    tips.sort_by(|a, b| b.from_tag.cmp(&a.from_tag).then(a.date.cmp(&b.date)));
    let mut g = Graph {
        repo: &repo,
        commits: HashMap::new(),
    };
    let mut names = HashMap::new();
    for tip in &tips {
        if let Some(c) = tip.commit {
            name_from(&mut g, &mut names, tip, c, cutoff);
        }
    }
    let exact = |id: Oid| tips.iter().find(|t| t.oid == id).map(|t| t.name.clone());
    let name_of = |id: Oid| -> Option<String> {
        match repo.find_object(id, None).ok()?.kind() {
            Some(ObjectType::Commit) => rev_name(&names, id),
            _ => exact(id),
        }
    };

    if let Some(text) = &o.annotate_stdin {
        let b = text.as_bytes();
        let hex = |c: u8| c.is_ascii_digit() || (b'a'..=b'f').contains(&c);
        let (mut start, mut run) = (0, 0);
        for i in 0..b.len() {
            if !hex(b[i]) {
                run = 0;
                continue;
            }
            run += 1;
            if run != 40 || b.get(i + 1).is_some_and(|&c| hex(c)) {
                continue;
            }
            run = 0;
            let id = Oid::from_str(&text[i - 39..=i]).ok();
            let known =
                id.filter(|id| g.commits.contains_key(id) || tips.iter().any(|t| t.oid == *id));
            let Some(name) = known.and_then(name_of) else {
                continue;
            };
            if o.name_only {
                let _ = write!(out, "{}{name}", &text[start..i - 39]);
            } else {
                let _ = write!(out, "{} ({name})", &text[start..=i]);
            }
            start = i + 1;
        }
        out.push_str(&text[start..]);
        return Ok((out, None));
    }

    // A commit with no name stops the listing, after what came before it.
    let mut show = |label: Option<&str>, id: Oid| -> Result<Option<String>, GitError> {
        let hexid = id.to_string();
        if !o.name_only {
            let _ = write!(out, "{} ", label.unwrap_or(&hexid));
        }
        match name_of(id) {
            Some(n) => {
                let _ = writeln!(out, "{n}");
            }
            None if o.undefined => out.push_str("undefined\n"),
            None if o.always => {
                let _ = writeln!(out, "{}", crate::plumbing::abbrev(&repo, &hexid, 0)?);
            }
            None => return Ok(Some(format!("cannot describe '{hexid}'"))),
        }
        Ok(None)
    };
    let mut ids: Vec<(Option<&str>, Oid)> = if o.all {
        // ponytail: git lists --all in its object hash-table order; this
        // lists by object id.
        g.commits.keys().map(|&id| (None, id)).collect()
    } else {
        args.iter().map(|(r, id)| (Some(r.as_str()), *id)).collect()
    };
    if o.all {
        ids.sort();
    }
    for (label, id) in ids {
        if let Some(e) = show(label, id)? {
            return Ok((out, Some(e)));
        }
    }
    Ok((out, None))
}
