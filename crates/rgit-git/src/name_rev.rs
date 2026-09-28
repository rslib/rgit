//! `git name-rev`: name commits relative to the refs that reach them, with
//! git's tip order, traversal and tie-breaking (builtin/name-rev.c).

use crate::rev::RevParse;
use std::collections::{HashMap, HashSet};
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
    /// git's is_better_name: a `~<n>` name costs as much as a merge hop.
    fn worse_than(&self, date: i64, generation: u32, distance: u32, from_tag: bool) -> bool {
        let effective = |d: u32, g: u32| d + if g > 0 { MERGE_TRAVERSAL_WEIGHT } else { 0 };
        let (old, new) = (
            effective(self.distance, self.generation),
            effective(distance, generation),
        );
        if from_tag && self.from_tag {
            return old > new;
        }
        if self.from_tag != from_tag {
            return from_tag;
        }
        if old != new {
            return old > new;
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

/// git's in-memory object hash (object.c): linear probing from the first
/// four id bytes, doubled at half full, and a lookup that swaps a hit into
/// the slot it hashed to. `--all` lists the commits in its slot order, so
/// every lookup git makes is replayed here in git's order.
#[derive(Default)]
struct ObjHash {
    slots: Vec<Option<Oid>>,
    nr: usize,
    commits: HashSet<Oid>,
    parsed: HashSet<Oid>,
}

impl ObjHash {
    fn home(id: Oid, n: usize) -> usize {
        let b = id.as_bytes();
        u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as usize & (n - 1)
    }

    fn place(slots: &mut [Option<Oid>], id: Oid) {
        let n = slots.len();
        let mut j = Self::home(id, n);
        while slots[j].is_some() {
            j = (j + 1) % n;
        }
        slots[j] = Some(id);
    }

    fn lookup(&mut self, id: Oid) -> bool {
        let n = self.slots.len();
        if n == 0 {
            return false;
        }
        let first = Self::home(id, n);
        let mut i = first;
        while let Some(o) = self.slots[i] {
            if o == id {
                self.slots.swap(i, first);
                return true;
            }
            i = (i + 1) % n;
        }
        false
    }

    /// lookup_commit and friends: find `id`, or create it.
    fn get(&mut self, id: Oid, commit: bool) {
        if commit {
            self.commits.insert(id);
        }
        if self.lookup(id) {
            return;
        }
        if self.slots.len() <= self.nr * 2 + 1 {
            let size = (self.slots.len() * 2).max(32);
            let mut grown = vec![None; size];
            for id in self.slots.iter().flatten() {
                Self::place(&mut grown, *id);
            }
            self.slots = grown;
        }
        Self::place(&mut self.slots, id);
        self.nr += 1;
    }
}

struct Graph<'r> {
    repo: &'r Repository,
    commits: HashMap<Oid, (i64, Vec<Oid>, Oid)>,
    /// Replays git's object lookups, for `--all`.
    hash: Option<ObjHash>,
    in_graph: HashSet<Oid>,
}

impl Graph<'_> {
    fn get(&mut self, id: Oid) -> Option<(i64, Vec<Oid>, Oid)> {
        if let Some(c) = self.commits.get(&id) {
            return Some(c.clone());
        }
        let c = self.repo.find_commit(id).ok()?;
        let v = (c.time().seconds(), c.parent_ids().collect(), c.tree_id());
        self.commits.insert(id, v.clone());
        Some(v)
    }

    /// repo_parse_commit: the commit-graph gives the parents, else the
    /// buffer gives the tree and parents (parse_commit_buffer).
    fn parse_commit(&mut self, id: Oid, buffer: bool) {
        if self.hash.as_ref().is_none_or(|h| h.parsed.contains(&id)) {
            return;
        }
        let Some((_, parents, tree)) = self.get(id) else {
            return;
        };
        let graph = !buffer && self.in_graph.contains(&id);
        let Some(h) = self.hash.as_mut() else {
            return;
        };
        h.parsed.insert(id);
        if !graph {
            h.get(tree, false);
        }
        for p in parents {
            h.get(p, true);
        }
    }

    /// parse_object's lookups.
    fn parse_object(&mut self, id: Oid) {
        let Some(h) = self.hash.as_mut() else {
            return;
        };
        if h.lookup(id) && h.parsed.contains(&id) {
            return;
        }
        let Ok(obj) = self.repo.find_object(id, None) else {
            return;
        };
        match obj.kind() {
            Some(ObjectType::Commit) => {
                h.get(id, true);
                self.parse_commit(id, true);
            }
            Some(ObjectType::Tag) => {
                h.get(id, false);
                h.parsed.insert(id);
                if let Some(t) = obj.as_tag() {
                    h.get(t.target_id(), t.target_type() == Some(ObjectType::Commit));
                }
            }
            Some(ObjectType::Blob) => {
                h.get(id, false);
                h.lookup(id);
                h.parsed.insert(id);
            }
            _ => {
                h.get(id, false);
                h.parsed.insert(id);
            }
        }
    }
}

fn name_from(g: &mut Graph, names: &mut HashMap<Oid, Name>, tip: &Tip, start: Oid, cutoff: i64) {
    g.parse_commit(start, false);
    let Some((date, ..)) = g.get(start) else {
        return;
    };
    if date < cutoff {
        return;
    }
    if names
        .get(&start)
        .is_some_and(|n| !n.worse_than(tip.date, 0, 0, tip.from_tag))
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
            g.parse_commit(p, false);
            let Some((pdate, ..)) = g.get(p) else {
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
                .is_some_and(|n| !n.worse_than(tip.date, generation, distance, tip.from_tag))
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

#[repr(C)]
struct TipKey {
    from_tag: i32,
    date: i64,
    at: usize,
}

unsafe extern "C" {
    fn qsort(
        base: *mut std::ffi::c_void,
        n: usize,
        size: usize,
        cmp: extern "C" fn(*const std::ffi::c_void, *const std::ffi::c_void) -> std::ffi::c_int,
    );
}

extern "C" fn tag_and_age(
    a: *const std::ffi::c_void,
    b: *const std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: qsort hands back pointers into the TipKey slice it sorts.
    let (a, b) = unsafe { (&*a.cast::<TipKey>(), &*b.cast::<TipKey>()) };
    match b.from_tag - a.from_tag {
        0 => a.date.cmp(&b.date) as std::ffi::c_int,
        d => d,
    }
}

/// name_tips' QSORT by cmp_by_tag_and_age: tags first, older first. It runs
/// the C library's qsort, as git does, since which of two equal tips names
/// a commit depends on that sort's order.
fn sort_tips(tips: &mut Vec<Tip>) {
    let mut keys: Vec<TipKey> = tips
        .iter()
        .enumerate()
        .map(|(at, t)| TipKey {
            from_tag: i32::from(t.from_tag),
            date: t.date,
            at,
        })
        .collect();
    if keys.len() > 1 {
        // SAFETY: the slice is valid for its length and tag_and_age only
        // reads the two TipKeys it is given.
        unsafe {
            qsort(
                keys.as_mut_ptr().cast(),
                keys.len(),
                std::mem::size_of::<TipKey>(),
                tag_and_age,
            );
        }
    }
    let mut old: Vec<Option<Tip>> = tips.drain(..).map(Some).collect();
    tips.extend(keys.iter().filter_map(|k| old[k.at].take()));
}

/// Whether git parses commits from the commit-graph here (core.commitGraph,
/// and no grafts, shallow file or replace refs).
fn graph_usable(repo: &Repository) -> bool {
    let common = repo.commondir();
    repo.config()
        .and_then(|c| c.get_bool("core.commitGraph"))
        .unwrap_or(true)
        && !common.join("info/grafts").exists()
        && !common.join("shallow").exists()
        && repo
            .references_glob("refs/replace/*")
            .map_or(true, |mut r| r.next().is_none())
}

/// `git name-rev`'s output for `revs` (or `--all`, or the text to annotate).
/// Unnamed revisions fail unless `--always` or `--undefined`.
pub fn name_rev(
    git_dir: &Path,
    o: &NameRevOpts,
    revs: &[String],
) -> Result<(String, Option<String>), GitError> {
    let repo = Repository::open(git_dir)?;
    let mut g = Graph {
        repo: &repo,
        commits: HashMap::new(),
        hash: o.all.then(ObjHash::default),
        in_graph: HashSet::new(),
    };
    if o.all && graph_usable(&repo) {
        g.in_graph = crate::commit_graph::listed(&repo);
    }
    let mut tips = Vec::new();
    let mut refs: Vec<(String, Oid)> = repo
        .references()?
        .flatten()
        .filter_map(|r| Some((r.name().ok()?.to_owned(), r.resolve().ok()?.target()?)))
        .collect();
    refs.sort();
    for (path, oid) in refs {
        g.parse_object(oid);
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
            g.parse_object(target);
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
        let Ok(obj) = repo.rev_single(rev) else {
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

    sort_tips(&mut tips);
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
    let ids: Vec<(Option<&str>, Oid)> = match &g.hash {
        Some(h) => h
            .slots
            .iter()
            .flatten()
            .filter(|id| h.commits.contains(id))
            .map(|&id| (None, id))
            .collect(),
        None => args.iter().map(|(r, id)| (Some(r.as_str()), *id)).collect(),
    };
    for (label, id) in ids {
        if let Some(e) = show(label, id)? {
            return Ok((out, Some(e)));
        }
    }
    Ok((out, None))
}
