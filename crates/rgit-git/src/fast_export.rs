//! `git fast-export`: history as a fast-import stream, byte for byte as git
//! writes it, and the revision walk (`--topo-order`, git's tie-breaks) it and
//! `git replay` share.

use crate::rev::RevParse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use git2::{ObjectType, Oid, Repository, Tree};

use crate::GitError;

fn usage(msg: impl Into<String>) -> GitError {
    GitError::Other(msg.into())
}

/// The revisions of a rev-list style command line.
#[derive(Default)]
pub(crate) struct Revs {
    /// Positive tips: the argument as typed, its full ref name when it names
    /// one ref, and the object.
    pub tips: Vec<(String, Option<String>, Oid)>,
    pub hide: Vec<Oid>,
}

/// The full name of the one ref `name` names (`HEAD` is its branch), as
/// git's dwim_ref finds it.
pub(crate) fn dwim_ref(repo: &Repository, name: &str) -> Option<String> {
    let r = repo.resolve_reference_from_short_name(name).ok()?;
    let r = r.resolve().ok()?;
    r.name().ok().map(str::to_owned)
}

/// `--all`, `--branches`, `--tags`, `--remotes`, `A..B`, `A...B`, `^A` and
/// `A`, in the order given.
pub(crate) fn parse_revs(repo: &Repository, args: &[String]) -> Result<Revs, GitError> {
    let mut revs = Revs::default();
    let mut not = false;
    let resolve = |s: &str| {
        repo.rev_single(s).map(|o| o.id()).map_err(|_| {
            usage(format!(
                "ambiguous argument '{s}': unknown revision or path not in the working tree."
            ))
        })
    };
    let commit_of = |s: &str| -> Result<Oid, GitError> {
        Ok(repo
            .rev_single(s)
            .and_then(|o| o.peel_to_commit())
            .map_err(|_| usage(format!("bad revision '{s}'")))?
            .id())
    };
    for arg in args {
        let refs = |prefix: &str| -> Result<Vec<(String, Oid)>, GitError> {
            let mut out = Vec::new();
            for r in repo.references()? {
                let r = r?;
                let (Some(name), Some(id)) = (r.name().ok(), r.target()) else {
                    continue;
                };
                if name.starts_with(prefix) {
                    out.push((name.to_owned(), id));
                }
            }
            out.sort();
            Ok(out)
        };
        let group = match arg.as_str() {
            "--all" => {
                let mut all = refs("refs/")?;
                if let Ok(head) = repo.head()
                    && let Some(id) = head.target()
                {
                    all.push(("HEAD".to_owned(), id));
                }
                Some(all)
            }
            "--branches" => Some(refs("refs/heads/")?),
            "--tags" => Some(refs("refs/tags/")?),
            "--remotes" => Some(refs("refs/remotes/")?),
            "--not" => {
                not = !not;
                continue;
            }
            _ => None,
        };
        if let Some(group) = group {
            for (name, id) in group {
                if not {
                    revs.hide.push(commit_of(&id.to_string())?);
                } else {
                    let full = dwim_ref(repo, &name);
                    revs.tips.push((name, full, id));
                }
            }
            continue;
        }
        if arg.starts_with('-') {
            return Err(usage(format!("unknown option '{arg}'")));
        }
        if let Some((a, b)) = arg.split_once("...") {
            let (a, b) = (or_head(a), or_head(b));
            let (x, y) = (commit_of(a)?, commit_of(b)?);
            for base in repo.merge_bases(x, y)?.iter() {
                revs.hide.push(*base);
            }
            for s in [a, b] {
                revs.tips
                    .push((s.to_owned(), dwim_ref(repo, s), resolve(s)?));
            }
        } else if let Some((a, b)) = arg.split_once("..") {
            let (a, b) = (or_head(a), or_head(b));
            revs.hide.push(commit_of(a)?);
            revs.tips
                .push((b.to_owned(), dwim_ref(repo, b), resolve(b)?));
        } else if let Some(a) = arg.strip_prefix('^') {
            revs.hide.push(commit_of(a)?);
        } else if not {
            revs.hide.push(commit_of(arg)?);
        } else {
            revs.tips
                .push((arg.clone(), dwim_ref(repo, arg), resolve(arg)?));
        }
    }
    Ok(revs)
}

fn or_head(s: &str) -> &str {
    if s.is_empty() { "HEAD" } else { s }
}

/// Commits a pathspec simplified away, each with the one parent it stands
/// for (`None` for a root).
pub(crate) type Omitted = HashMap<Oid, Option<Oid>>;

/// Whether a commit's tree matches its parent's (or, for a root, the empty
/// tree) within the pathspec.
pub(crate) type TreeSame<'a> = &'a mut dyn FnMut(Oid, Option<Oid>) -> Result<bool, GitError>;

/// A walked commit: its parents as the walk sees them.
pub(crate) struct Walked {
    pub id: Oid,
    pub parents: Vec<Oid>,
}

/// Commits reachable from `tips` but not `hide`, parents first, as git's
/// limited `--topo-order --reverse` walk yields them; `sources` gets the
/// first tip name reaching each commit (git's revision_sources).
/// `treesame` simplifies history as a pathspec does, leaving out commits
/// that match a parent.
// ponytail: git walks a commit-graph repo incrementally (init_topo_walk);
// this is the limited walk, whose tie-breaks can differ there.
pub(crate) fn topo_walk(
    repo: &Repository,
    tips: &[(String, Oid)],
    hide: &[Oid],
    sources: &mut HashMap<Oid, String>,
    mut treesame: Option<TreeSame>,
) -> Result<(Vec<Walked>, Omitted), GitError> {
    let mut hidden = HashSet::new();
    if !hide.is_empty() {
        let mut w = repo.revwalk()?;
        for h in hide {
            w.push(*h)?;
        }
        for id in w {
            hidden.insert(id?);
        }
    }
    let time = |id: Oid| repo.find_commit(id).map(|c| c.committer().when().seconds());
    let mut start: Vec<(i64, String, Oid)> = Vec::new();
    let mut seen = HashSet::new();
    for (name, id) in tips {
        if seen.insert(*id) {
            start.push((time(*id)?, name.clone(), *id));
        }
    }
    start.sort_by_key(|s| std::cmp::Reverse(s.0));
    let mut heap = BinaryHeap::new();
    let mut ctr = 0u64;
    for (t, name, id) in start {
        sources.entry(id).or_insert(name);
        heap.push((t, std::cmp::Reverse(ctr), id));
        ctr += 1;
    }
    let mut list: Vec<Walked> = Vec::new();
    let mut omitted: Omitted = HashMap::new();
    while let Some((_, _, id)) = heap.pop() {
        if hidden.contains(&id) {
            continue;
        }
        let c = repo.find_commit(id)?;
        let mut parents: Vec<Oid> = c.parent_ids().collect();
        let src = sources.get(&id).cloned();
        for p in &parents {
            if let Some(s) = &src {
                sources.entry(*p).or_insert_with(|| s.clone());
            }
            if seen.insert(*p) {
                heap.push((time(*p)?, std::cmp::Reverse(ctr), *p));
                ctr += 1;
            }
        }
        if let Some(same) = treesame.as_mut() {
            if parents.is_empty() {
                if same(id, None)? {
                    omitted.insert(id, None);
                }
            } else if let Some(p) = parents
                .iter()
                .copied()
                .find(|p| same(id, Some(*p)).unwrap_or(false))
            {
                parents = vec![p];
                omitted.insert(id, Some(p));
            }
        }
        list.push(Walked { id, parents });
    }
    let order = topo_sort(&list);
    let mut out = Vec::new();
    for i in order.into_iter().rev() {
        let w = &list[i];
        if omitted.contains_key(&w.id) {
            continue;
        }
        let mut parents: Vec<Oid> = Vec::new();
        for p in &w.parents {
            if let Some(p) = rewrite_parent(*p, &omitted)
                && !parents.contains(&p)
            {
                parents.push(p);
            }
        }
        out.push(Walked { id: w.id, parents });
    }
    Ok((out, omitted))
}

/// The commit `id` stands for once pathspec-omitted commits are skipped;
/// `None` when nothing is left.
fn rewrite_parent(mut id: Oid, omitted: &HashMap<Oid, Option<Oid>>) -> Option<Oid> {
    while let Some(next) = omitted.get(&id) {
        id = (*next)?;
    }
    Some(id)
}

/// git's sort_in_topological_order (graph order): indexes into `list`,
/// children first.
fn topo_sort(list: &[Walked]) -> Vec<usize> {
    let pos: HashMap<Oid, usize> = list.iter().enumerate().map(|(i, w)| (w.id, i)).collect();
    let mut indegree = vec![1usize; list.len()];
    for w in list {
        for p in &w.parents {
            if let Some(&j) = pos.get(p)
                && indegree[j] > 0
            {
                indegree[j] += 1;
            }
        }
    }
    let mut stack: Vec<usize> = (0..list.len()).filter(|&i| indegree[i] == 1).collect();
    stack.reverse();
    let mut out = Vec::with_capacity(list.len());
    while let Some(i) = stack.pop() {
        for p in &list[i].parents {
            if let Some(&j) = pos.get(p) {
                if indegree[j] == 0 {
                    continue;
                }
                indegree[j] -= 1;
                if indegree[j] == 1 {
                    stack.push(j);
                }
            }
        }
        indegree[i] = 0;
        out.push(i);
    }
    out
}

/// One change of a tree diff.
pub(crate) struct Change {
    pub status: u8,
    pub path: String,
    pub mode: u32,
    pub id: Oid,
}

/// git's recursive tree diff, in tree order: `A`, `D`, `M` and `T` (a file
/// turned symlink or submodule), limited to `specs`.
pub(crate) fn diff_trees(
    repo: &Repository,
    old: Option<&Tree>,
    new: Option<&Tree>,
    prefix: &str,
    specs: &[String],
    out: &mut Vec<Change>,
) -> Result<(), GitError> {
    type Entry = (Vec<u8>, String, u32, Oid);
    let entries = |t: Option<&Tree>| -> Vec<Entry> {
        t.map(|t| {
            t.iter()
                .map(|e| {
                    let mode = e.filemode() as u32;
                    let name = String::from_utf8_lossy(e.name_bytes()).into_owned();
                    let mut key = e.name_bytes().to_vec();
                    if mode == 0o040000 {
                        key.push(b'/');
                    }
                    (key, name, mode, e.id())
                })
                .collect()
        })
        .unwrap_or_default()
    };
    let (a, b) = (entries(old), entries(new));
    let (mut i, mut j) = (0, 0);
    let subtree = |id: Oid| repo.find_tree(id);
    let one = |e: &Entry, deleted: bool, out: &mut Vec<Change>| -> Result<(), GitError> {
        let path = format!("{prefix}{}", e.1);
        if e.2 == 0o040000 {
            let t = subtree(e.3)?;
            let (o, n) = if deleted {
                (Some(&t), None)
            } else {
                (None, Some(&t))
            };
            return diff_trees(repo, o, n, &format!("{path}/"), specs, out);
        }
        if specs.is_empty() || crate::pathspec_matches(specs, &path) {
            out.push(Change {
                status: if deleted { b'D' } else { b'A' },
                path,
                mode: if deleted { 0 } else { e.2 },
                id: if deleted { Oid::ZERO_SHA1 } else { e.3 },
            });
        }
        Ok(())
    };
    while i < a.len() || j < b.len() {
        let ord = match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) => x.0.cmp(&y.0),
            (Some(_), None) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Greater,
        };
        match ord {
            std::cmp::Ordering::Less => {
                one(&a[i], true, out)?;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                one(&b[j], false, out)?;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                let (x, y) = (&a[i], &b[j]);
                i += 1;
                j += 1;
                if x.2 == y.2 && x.3 == y.3 {
                    continue;
                }
                let path = format!("{prefix}{}", y.1);
                if y.2 == 0o040000 {
                    let (t, u) = (subtree(x.3)?, subtree(y.3)?);
                    diff_trees(repo, Some(&t), Some(&u), &format!("{path}/"), specs, out)?;
                } else if specs.is_empty() || crate::pathspec_matches(specs, &path) {
                    let kind = |m: u32| m & 0o170000;
                    out.push(Change {
                        status: if kind(x.2) == kind(y.2) { b'M' } else { b'T' },
                        path,
                        mode: y.2,
                        id: y.3,
                    });
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Abort,
    Verbatim,
    Warn,
    WarnStrip,
    Strip,
}

fn signed_mode(v: &str, what: &str) -> Result<Mode, GitError> {
    Ok(match v {
        "abort" => Mode::Abort,
        "verbatim" | "ignore" => Mode::Verbatim,
        "warn" | "warn-verbatim" => Mode::Warn,
        "warn-strip" => Mode::WarnStrip,
        "strip" => Mode::Strip,
        _ => return Err(usage(format!("unknown {what} mode '{v}'"))),
    })
}

#[derive(Default)]
struct Opts {
    signed_tags: Option<Mode>,
    signed_commits: Option<Mode>,
    tag_filtered: Option<String>,
    reencode: Option<String>,
    export_marks: Option<String>,
    import_marks: Option<(String, bool)>,
    fake_missing_tagger: bool,
    full_tree: bool,
    done: bool,
    no_data: bool,
    refspecs: Vec<(String, String)>,
    reference_excluded: bool,
    original_ids: bool,
    mark_tags: bool,
    revs: Vec<String>,
    paths: Vec<String>,
}

fn parse_opts(args: &[String]) -> Result<Opts, GitError> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let value = |it: &mut std::slice::Iter<String>| {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| usage(format!("option '{}' requires a value", &name[2..])))
        };
        match name {
            "--" => {
                o.paths.extend(it.by_ref().cloned());
                break;
            }
            "--signed-tags" => o.signed_tags = Some(signed_mode(&value(&mut it)?, "signed tag")?),
            "--signed-commits" => {
                o.signed_commits = Some(signed_mode(&value(&mut it)?, "signed commit")?)
            }
            "--tag-of-filtered-object" => {
                let v = value(&mut it)?;
                if !matches!(v.as_str(), "abort" | "drop" | "rewrite") {
                    return Err(usage(format!("unknown tag-of-filtered mode '{v}'")));
                }
                o.tag_filtered = Some(v);
            }
            "--reencode" => {
                let v = value(&mut it)?;
                let v = match v.as_str() {
                    "yes" | "true" | "on" => "yes",
                    "no" | "false" | "off" => "no",
                    "abort" => "abort",
                    _ => return Err(usage(format!("unknown reencoding mode '{v}'"))),
                };
                o.reencode = Some(v.to_owned());
            }
            "--export-marks" => o.export_marks = Some(value(&mut it)?),
            "--import-marks" => o.import_marks = Some((value(&mut it)?, false)),
            "--import-marks-if-exists" => o.import_marks = Some((value(&mut it)?, true)),
            "--progress" => {
                value(&mut it)?;
            }
            "--fake-missing-tagger" => o.fake_missing_tagger = true,
            "--no-fake-missing-tagger" => o.fake_missing_tagger = false,
            "--full-tree" => o.full_tree = true,
            "--no-full-tree" => o.full_tree = false,
            "--use-done-feature" => o.done = true,
            "--no-use-done-feature" => o.done = false,
            "--no-data" => o.no_data = true,
            "--data" => o.no_data = false,
            "--refspec" => {
                let v = value(&mut it)?;
                let v = v.strip_prefix('+').unwrap_or(&v).to_owned();
                let (src, dst) = v.split_once(':').unwrap_or((&v, &v));
                o.refspecs.push((src.to_owned(), dst.to_owned()));
            }
            "--reference-excluded-parents" => o.reference_excluded = true,
            "--no-reference-excluded-parents" => o.reference_excluded = false,
            "--show-original-ids" => o.original_ids = true,
            "--no-show-original-ids" => o.original_ids = false,
            "--mark-tags" => o.mark_tags = true,
            "--no-mark-tags" => o.mark_tags = false,
            "-M" | "-C" | "--anonymize" => {
                return Err(usage(format!("rgit fast-export does not support {name}")));
            }
            _ => o.revs.push(arg.clone()),
        }
    }
    Ok(o)
}

/// Map `name` through the `--refspec`s: the first match's destination.
fn apply_refspecs(specs: &[(String, String)], name: &str) -> String {
    for (src, dst) in specs {
        match (src.split_once('*'), dst.split_once('*')) {
            (Some((sp, ss)), Some((dp, ds))) => {
                if let Some(mid) = name.strip_prefix(sp).and_then(|r| r.strip_suffix(ss)) {
                    return format!("{dp}{mid}{ds}");
                }
            }
            _ if src == name => return dst.clone(),
            _ => {}
        }
    }
    name.to_owned()
}

fn print_path(out: &mut dyn Write, path: &str) -> std::io::Result<()> {
    let q = crate::quote_path(path);
    if q != path {
        write!(out, "{q}")
    } else if path.contains(' ') {
        write!(out, "\"{path}\"")
    } else {
        write!(out, "{path}")
    }
}

/// git's depth_first: files inside a folder before the folder itself.
fn depth_first(a: &Change, b: &Change) -> std::cmp::Ordering {
    let (x, y) = (a.path.as_bytes(), b.path.as_bytes());
    let n = x.len().min(y.len());
    x[..n].cmp(&y[..n]).then(y.len().cmp(&x.len()))
}

struct Exporter<'r> {
    repo: &'r Repository,
    out: &'r mut dyn Write,
    o: Opts,
    marks: HashMap<Oid, u32>,
    last: u32,
    imported_last: u32,
}

impl Exporter<'_> {
    fn mark(&mut self, id: Oid) -> u32 {
        self.last += 1;
        self.marks.insert(id, self.last);
        self.last
    }

    fn blob(&mut self, id: Oid) -> Result<(), GitError> {
        if self.o.no_data || id.is_zero() || self.marks.contains_key(&id) {
            return Ok(());
        }
        let blob = self.repo.find_blob(id)?;
        let m = self.mark(id);
        writeln!(self.out, "blob\nmark :{m}")?;
        if self.o.original_ids {
            writeln!(self.out, "original-oid {id}")?;
        }
        writeln!(self.out, "data {}", blob.content().len())?;
        self.out.write_all(blob.content())?;
        writeln!(self.out)?;
        Ok(())
    }

    fn commit(&mut self, w: &Walked, refname: &str) -> Result<(), GitError> {
        let repo = self.repo;
        let c = repo.find_commit(w.id)?;
        let odb = repo.odb()?;
        let raw = odb.read(w.id)?;
        let buf = raw.data();
        let (head, message) = match buf.windows(2).position(|p| p == b"\n\n") {
            Some(i) => (&buf[..i + 1], Some(&buf[i + 2..])),
            None => (buf, None),
        };
        let message = message.map(|m| &m[..m.iter().position(|&b| b == 0).unwrap_or(m.len())]);
        let header = |key: &str| -> Option<String> {
            let text = String::from_utf8_lossy(head);
            text.lines()
                .find_map(|l| l.strip_prefix(key).map(str::to_owned))
        };
        let author = header("author ").map(|a| format!("author {a}"));
        let committer = header("committer ").map(|a| format!("committer {a}"));
        let encoding = header("encoding ");
        let mut signature = None;
        for key in ["gpgsig", "gpgsig-sha256"] {
            if let Ok(sig) = c.header_field_bytes(key) {
                let algo = if key == "gpgsig" { "sha1" } else { "sha256" };
                signature = Some((algo, sig.to_vec()));
                break;
            }
        }
        let first = w.parents.first().copied();
        let tree = c.tree()?;
        let mut changes = Vec::new();
        match first {
            Some(p)
                if (self.marks.contains_key(&p) || self.o.reference_excluded)
                    && !self.o.full_tree =>
            {
                let pt = repo.find_commit(p)?.tree()?;
                diff_trees(
                    repo,
                    Some(&pt),
                    Some(&tree),
                    "",
                    &self.o.paths,
                    &mut changes,
                )?;
            }
            _ => diff_trees(repo, None, Some(&tree), "", &self.o.paths, &mut changes)?,
        }
        for ch in &changes {
            if ch.mode != 0o160000 {
                self.blob(ch.id)?;
            }
        }
        let m = self.mark(w.id);
        let mut msg = message.map(<[u8]>::to_vec).unwrap_or_default();
        let mut reencoded = false;
        if let Some(enc) = &encoding {
            match self.o.reencode.as_deref().unwrap_or("abort") {
                "yes" => {
                    msg = to_utf8(&msg, enc).ok_or_else(|| {
                        usage(format!(
                            "rgit cannot reencode from {enc}; use --reencode=no"
                        ))
                    })?;
                    reencoded = true;
                }
                "no" => {}
                _ => {
                    return Err(usage(format!(
                        "Encountered commit-specific encoding {enc} in commit {}; use --reencode=[yes|no] to handle it",
                        w.id
                    )));
                }
            }
        }
        if let Some((_, _)) = &signature {
            match self.o.signed_commits.unwrap_or(Mode::Strip) {
                Mode::Abort => {
                    return Err(usage(format!(
                        "encountered signed commit {}; use --signed-commits=<mode> to handle it",
                        w.id
                    )));
                }
                Mode::Warn => eprintln!("warning: exporting signature of commit {}", w.id),
                Mode::WarnStrip => {
                    eprintln!("warning: stripping signature from commit {}", w.id);
                    signature = None;
                }
                Mode::Strip => signature = None,
                Mode::Verbatim => {}
            }
        }
        if w.parents.is_empty() {
            writeln!(self.out, "reset {refname}")?;
        }
        writeln!(self.out, "commit {refname}\nmark :{m}")?;
        if self.o.original_ids {
            writeln!(self.out, "original-oid {}", w.id)?;
        }
        writeln!(
            self.out,
            "{}\n{}",
            author.unwrap_or_default(),
            committer.unwrap_or_default()
        )?;
        if let Some((algo, sig)) = &signature {
            writeln!(self.out, "gpgsig {algo}\ndata {}", sig.len())?;
            self.out.write_all(sig)?;
        }
        if let (Some(enc), false) = (&encoding, reencoded) {
            writeln!(self.out, "encoding {enc}")?;
        }
        writeln!(self.out, "data {}", msg.len())?;
        self.out.write_all(&msg)?;
        let mut i = 0;
        for p in &w.parents {
            let mark = self.marks.get(p).copied();
            if mark.is_none() && !self.o.reference_excluded {
                continue;
            }
            let word = if i == 0 { "from" } else { "merge" };
            match mark {
                Some(n) => writeln!(self.out, "{word} :{n}")?,
                None => writeln!(self.out, "{word} {p}")?,
            }
            i += 1;
        }
        if self.o.full_tree {
            writeln!(self.out, "deleteall")?;
        }
        changes.sort_by(depth_first);
        for ch in &changes {
            if ch.status == b'D' {
                write!(self.out, "D ")?;
            } else if self.o.no_data || ch.mode == 0o160000 {
                write!(self.out, "M {:06o} {} ", ch.mode, ch.id)?;
            } else {
                write!(self.out, "M {:06o} :{} ", ch.mode, self.marks[&ch.id])?;
            }
            print_path(self.out, &ch.path)?;
            writeln!(self.out)?;
        }
        writeln!(self.out)?;
        Ok(())
    }

    fn tag(&mut self, name: &str, id: Oid, omitted: &Omitted) -> Result<(), GitError> {
        let repo = self.repo;
        let tag = repo.find_tag(id)?;
        let mut target = tag.target()?;
        while let Some(t) = target.as_tag() {
            target = t.target()?;
        }
        if target.kind() == Some(ObjectType::Tree) {
            eprintln!(
                "warning: Omitting tag {id},\nsince tags of trees (or tags of tags of trees, etc.) are not supported."
            );
            return Ok(());
        }
        let odb = repo.odb()?;
        let raw = odb.read(id)?;
        let buf = raw.data();
        let msg_at = buf.windows(2).position(|p| p == b"\n\n").map(|i| i + 2);
        let mut message: &[u8] = match msg_at {
            Some(i) => {
                let m = &buf[i..];
                &m[..m.iter().position(|&b| b == 0).unwrap_or(m.len())]
            }
            None => b"",
        };
        let head = String::from_utf8_lossy(&buf[..msg_at.unwrap_or(buf.len())]).into_owned();
        let tagger = head
            .lines()
            .find(|l| l.starts_with("tagger "))
            .map(str::to_owned)
            .or_else(|| {
                self.o
                    .fake_missing_tagger
                    .then(|| "tagger Unspecified Tagger <unspecified-tagger> 0 +0000".to_owned())
            });
        let sig = b"\n-----BEGIN PGP SIGNATURE-----\n";
        if msg_at.is_some()
            && let Some(at) = message.windows(sig.len()).position(|w| w == sig)
        {
            match self.o.signed_tags.unwrap_or(Mode::Abort) {
                Mode::Abort => {
                    return Err(usage(format!(
                        "encountered signed tag {id}; use --signed-tags=<mode> to handle it"
                    )));
                }
                Mode::Warn => eprintln!("warning: exporting signed tag {id}"),
                Mode::Verbatim => {}
                Mode::WarnStrip => {
                    eprintln!("warning: stripping signature from tag {id}");
                    message = &message[..at + 1];
                }
                Mode::Strip => message = &message[..at + 1],
            }
        }
        let tagged = tag.target_id();
        let tagged_kind = tag.target_type();
        let mut mark = self.marks.get(&tagged).copied();
        if mark.is_none() {
            match self.o.tag_filtered.as_deref().unwrap_or("abort") {
                "drop" => return Ok(()),
                "rewrite" => {
                    if tagged_kind == Some(ObjectType::Tag) && !self.o.mark_tags {
                        return Err(usage(
                            "Error: Cannot export nested tags unless --mark-tags is specified.",
                        ));
                    }
                    if tagged_kind == Some(ObjectType::Commit) {
                        match rewrite_parent(tagged, omitted) {
                            Some(p) => mark = self.marks.get(&p).copied(),
                            None => {
                                writeln!(self.out, "reset {name}\nfrom {}\n", Oid::ZERO_SHA1)?;
                                return Ok(());
                            }
                        }
                    }
                }
                _ => {
                    return Err(usage(format!(
                        "tag {id} tags unexported object; use --tag-of-filtered-object=<mode> to handle it"
                    )));
                }
            }
        }
        if tagged_kind == Some(ObjectType::Tag) {
            writeln!(self.out, "reset {name}\nfrom {}\n", Oid::ZERO_SHA1)?;
        }
        let short = name.strip_prefix("refs/tags/").unwrap_or(name);
        writeln!(self.out, "tag {short}")?;
        if self.o.mark_tags {
            let m = self.mark(id);
            writeln!(self.out, "mark :{m}")?;
        }
        match mark {
            Some(m) => writeln!(self.out, "from :{m}")?,
            None => writeln!(self.out, "from {tagged}")?,
        }
        if self.o.original_ids {
            writeln!(self.out, "original-oid {id}")?;
        }
        if let Some(t) = &tagger {
            writeln!(self.out, "{t}")?;
        }
        writeln!(self.out, "data {}", message.len())?;
        self.out.write_all(message)?;
        writeln!(self.out)?;
        Ok(())
    }

    fn import_marks(&mut self, file: &str, if_exists: bool) -> Result<(), GitError> {
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) if if_exists => return Ok(()),
            Err(e) => return Err(usage(format!("cannot read '{file}': {e}"))),
        };
        for line in text.lines() {
            let parsed = line
                .strip_prefix(':')
                .and_then(|l| l.split_once(' '))
                .and_then(|(m, id)| Some((m.parse::<u32>().ok()?, Oid::from_str(id).ok()?)));
            let Some((m, id)) = parsed else {
                return Err(usage(format!("corrupt mark line: {line}")));
            };
            if self.repo.find_commit(id).is_err() {
                continue;
            }
            self.marks.insert(id, m);
            self.last = self.last.max(m);
        }
        self.imported_last = self.last;
        Ok(())
    }
}

/// Latin-1 to UTF-8, the one re-encoding rgit does.
fn to_utf8(bytes: &[u8], enc: &str) -> Option<Vec<u8>> {
    let e = enc.to_ascii_lowercase();
    if e == "utf-8" || e == "utf8" {
        return Some(bytes.to_vec());
    }
    if !matches!(
        e.as_str(),
        "iso-8859-1" | "iso8859-1" | "latin1" | "latin-1"
    ) {
        return None;
    }
    Some(
        bytes
            .iter()
            .map(|&b| b as char)
            .collect::<String>()
            .into_bytes(),
    )
}

/// `git fast-export <args>` into `out`.
pub fn fast_export(git_dir: &Path, args: &[String], out: &mut dyn Write) -> Result<(), GitError> {
    let repo = Repository::open(git_dir)?;
    let o = parse_opts(args)?;
    let revs = parse_revs(&repo, &o.revs)?;
    let mut ex = Exporter {
        repo: &repo,
        out,
        o,
        marks: HashMap::new(),
        last: 0,
        imported_last: 0,
    };
    if let Some((file, if_exists)) = ex.o.import_marks.clone() {
        ex.import_marks(&file, if_exists)?;
        if !ex.o.paths.is_empty() {
            ex.o.full_tree = true;
        }
    }
    if ex.o.done {
        writeln!(ex.out, "feature done")?;
    }
    let mut sources: HashMap<Oid, String> = HashMap::new();
    let mut extra: Vec<(String, Oid)> = Vec::new();
    let mut tag_refs: Vec<(String, Oid)> = Vec::new();
    let mut walk_tips: Vec<(String, Oid)> = Vec::new();
    for (name, full, id) in &revs.tips {
        let mut obj = repo.find_object(*id, None)?;
        while let Some(t) = obj.as_tag() {
            obj = t.target()?;
        }
        if obj.kind() == Some(ObjectType::Commit) {
            walk_tips.push((full.clone().unwrap_or_else(|| name.clone()), obj.id()));
        }
        let Some(full) = full else { continue };
        let full = apply_refspecs(&ex.o.refspecs, full);
        let item = repo.find_object(*id, None)?;
        if item.kind() == Some(ObjectType::Tag) {
            let mut t = item.clone();
            while let Some(tag) = t.as_tag() {
                tag_refs.push((full.clone(), t.id()));
                t = tag.target()?;
            }
        }
        match obj.kind() {
            Some(ObjectType::Commit) => {}
            Some(ObjectType::Blob) => {
                ex.blob(obj.id())?;
                continue;
            }
            Some(k) => {
                if item.kind() == Some(ObjectType::Tag) {
                    eprintln!("warning: Tag points to object of unexpected type {k}, skipping.");
                } else {
                    eprintln!("warning: {name}: Unexpected object of type {k}, skipping.");
                }
                continue;
            }
            None => continue,
        }
        if item.kind() != Some(ObjectType::Tag) {
            extra.push((full.clone(), obj.id()));
        }
        sources.entry(obj.id()).or_insert(full);
    }
    extra.sort();
    extra.dedup_by(|a, b| a.0 == b.0);
    let paths = ex.o.paths.clone();
    let repo_ref = &repo;
    let mut same = |id: Oid, parent: Option<Oid>| -> Result<bool, GitError> {
        let t = repo_ref.find_commit(id)?.tree()?;
        let pt = parent
            .map(|p| repo_ref.find_commit(p)?.tree())
            .transpose()?;
        let mut ch = Vec::new();
        diff_trees(repo_ref, pt.as_ref(), Some(&t), "", &paths, &mut ch)?;
        Ok(ch.is_empty())
    };
    let simplify: Option<TreeSame> = if paths.is_empty() {
        None
    } else {
        Some(&mut same)
    };
    let (walked, omitted) = topo_walk(&repo, &walk_tips, &revs.hide, &mut sources, simplify)?;
    for w in &walked {
        if ex.marks.contains_key(&w.id) {
            continue;
        }
        let src = sources.get(&w.id).cloned().unwrap_or_default();
        extra.retain(|(n, _)| *n != src);
        ex.commit(w, &src)?;
    }
    for (name, id) in extra.iter().rev() {
        let Some(c) = rewrite_parent(*id, &omitted) else {
            writeln!(ex.out, "reset {name}\nfrom {}\n", Oid::ZERO_SHA1)?;
            continue;
        };
        match ex.marks.get(&c) {
            Some(m) => writeln!(ex.out, "reset {name}\nfrom :{m}\n")?,
            None if ex.o.reference_excluded => writeln!(ex.out, "reset {name}\nfrom {c}\n")?,
            None => writeln!(ex.out, "reset {name}\nfrom {}\n", Oid::ZERO_SHA1)?,
        }
    }
    for (name, id) in tag_refs.iter().rev() {
        ex.tag(name, *id, &omitted)?;
    }
    for (src, dst) in &ex.o.refspecs {
        if src.is_empty() {
            writeln!(ex.out, "reset {dst}\nfrom {}\n", Oid::ZERO_SHA1)?;
        }
    }
    if let Some(file) = &ex.o.export_marks
        && ex.last != ex.imported_last
    {
        let mut marks: Vec<(u32, Oid)> = ex
            .marks
            .iter()
            .filter(|(id, _)| repo.find_commit(**id).is_ok())
            .map(|(id, m)| (*m, *id))
            .collect();
        marks.sort();
        let text: String = marks.iter().map(|(m, id)| format!(":{m} {id}\n")).collect();
        std::fs::write(file, text)?;
    }
    if ex.o.done {
        writeln!(ex.out, "done")?;
    }
    ex.out.flush()?;
    Ok(())
}
