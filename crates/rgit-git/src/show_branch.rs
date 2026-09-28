//! `git show-branch`: builtin/show-branch.c's walk, naming and layout, byte
//! for byte.

use crate::rev::RevParse;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::Path;

use git2::{Oid, Repository};

use crate::GitError;

const REV_SHIFT: u32 = 2;
const MAX_REVS: usize = 26;
const UNINTERESTING: u32 = 1;
const COLORS: [&str; 12] = [
    "\x1b[31m",
    "\x1b[32m",
    "\x1b[33m",
    "\x1b[34m",
    "\x1b[35m",
    "\x1b[36m",
    "\x1b[1;31m",
    "\x1b[1;32m",
    "\x1b[1;33m",
    "\x1b[1;34m",
    "\x1b[1;35m",
    "\x1b[1;36m",
];

/// `git show-branch`'s options.
#[derive(Default)]
pub struct ShowBranchOpts {
    pub revs: Vec<String>,
    pub all: bool,
    pub remotes: bool,
    pub current: bool,
    pub date_order: bool,
    pub sparse: bool,
    /// `--more=<n>`; -1 is `--list`.
    pub extra: i32,
    pub merge_base: bool,
    pub independent: bool,
    pub no_name: bool,
    pub sha1_name: bool,
    pub topics: bool,
    /// `-g`: how many entries, and the `,<base>` after it.
    pub reflog: Option<(usize, Option<String>)>,
    /// The time a `<base>` that is not a count names (approxidate).
    pub reflog_date: Option<i64>,
    pub color: bool,
}

struct Walk<'r> {
    repo: &'r Repository,
    /// Parsed commits: date and parents.
    parsed: HashMap<Oid, (i64, Vec<Oid>)>,
    flags: HashMap<Oid, u32>,
    names: HashMap<Oid, (String, u32)>,
}

impl Walk<'_> {
    fn parse(&mut self, id: Oid) -> Result<(), GitError> {
        if !self.parsed.contains_key(&id) {
            let c = self.repo.find_commit(id)?;
            self.parsed
                .insert(id, (c.time().seconds(), c.parent_ids().collect()));
        }
        Ok(())
    }
    fn date(&self, id: Oid) -> i64 {
        self.parsed.get(&id).map_or(0, |c| c.0)
    }
    fn parents(&self, id: Oid) -> Vec<Oid> {
        self.parsed
            .get(&id)
            .map(|c| c.1.clone())
            .unwrap_or_default()
    }
    fn flag(&self, id: Oid) -> u32 {
        self.flags.get(&id).copied().unwrap_or(0)
    }
    /// commit.c's `commit_list_insert_by_date`: after equal dates.
    fn insert_by_date(&self, list: &mut VecDeque<Oid>, id: Oid) {
        let d = self.date(id);
        let at = list
            .iter()
            .position(|&c| self.date(c) < d)
            .unwrap_or(list.len());
        list.insert(at, id);
    }
    fn mark_seen(&self, id: Oid, seen: &mut Vec<Oid>) -> bool {
        if self.flag(id) == 0 {
            seen.push(id);
            return true;
        }
        false
    }

    fn join_revs(
        &mut self,
        list: &mut VecDeque<Oid>,
        seen: &mut Vec<Oid>,
        num_rev: usize,
        mut extra: i32,
    ) -> Result<(), GitError> {
        let all_mask = (1u32 << (REV_SHIFT as usize + num_rev)) - 1;
        let all_revs = all_mask & !((1 << REV_SHIFT) - 1);
        while !list.is_empty() {
            let still = list.iter().any(|&c| self.flag(c) & UNINTERESTING == 0);
            let id = list.pop_front().expect("not empty");
            let mut flags = self.flag(id) & all_mask;
            if !still && extra <= 0 {
                break;
            }
            self.mark_seen(id, seen);
            if flags & all_revs == all_revs {
                flags |= UNINTERESTING;
            }
            for p in self.parents(id) {
                if self.flag(p) & flags == flags {
                    continue;
                }
                self.parse(p)?;
                if self.mark_seen(p, seen) && !still {
                    extra -= 1;
                }
                *self.flags.entry(p).or_default() |= flags;
                self.insert_by_date(list, p);
            }
        }
        loop {
            let mut changed = false;
            for &c in seen.iter() {
                let f = self.flag(c);
                if f & all_revs != all_revs && f & UNINTERESTING == 0 {
                    continue;
                }
                for p in self.parents(c) {
                    let pf = self.flags.entry(p).or_default();
                    if *pf & UNINTERESTING == 0 {
                        *pf |= UNINTERESTING;
                        changed = true;
                    }
                }
            }
            if !changed {
                return Ok(());
            }
        }
    }

    /// commit.c's `sort_in_topological_order`, in graph or commit-date order.
    fn topo_sort(&self, list: Vec<Oid>, by_date: bool) -> Vec<Oid> {
        let mut indegree: HashMap<Oid, u32> = list.iter().map(|&c| (c, 1)).collect();
        for &c in &list {
            for p in self.parents(c) {
                if let Some(n) = indegree.get_mut(&p)
                    && *n > 0
                {
                    *n += 1;
                }
            }
        }
        // A LIFO stack for graph order, a heap by date (then insertion)
        // for date order.
        let mut queue: Vec<(Oid, u64)> = Vec::new();
        let mut ctr = 0u64;
        let mut put = |q: &mut Vec<(Oid, u64)>, c: Oid| {
            q.push((c, ctr));
            ctr += 1;
        };
        for &c in &list {
            if indegree[&c] == 1 {
                put(&mut queue, c);
            }
        }
        if !by_date {
            queue.reverse();
        }
        let get = |q: &mut Vec<(Oid, u64)>| -> Option<Oid> {
            if !by_date {
                return q.pop().map(|x| x.0);
            }
            let best = (0..q.len()).max_by(|&a, &b| {
                self.date(q[a].0)
                    .cmp(&self.date(q[b].0))
                    .then(q[b].1.cmp(&q[a].1))
            })?;
            Some(q.remove(best).0)
        };
        let mut out = Vec::new();
        while let Some(c) = get(&mut queue) {
            for p in self.parents(c) {
                let Some(n) = indegree.get_mut(&p) else {
                    continue;
                };
                if *n == 0 {
                    continue;
                }
                *n -= 1;
                if *n == 1 {
                    put(&mut queue, p);
                }
            }
            indegree.insert(c, 0);
            out.push(c);
        }
        out
    }

    fn name_parent(&mut self, c: Oid, p: Oid) {
        let Some((head, generation)) = self.names.get(&c).cloned() else {
            return;
        };
        if self.names.get(&p).is_none_or(|(_, g)| generation + 1 < *g) {
            self.names.insert(p, (head, generation + 1));
        }
    }

    fn name_first_parent_chain(&mut self, mut c: Oid) -> usize {
        let mut i = 0;
        loop {
            if !self.names.contains_key(&c) {
                break;
            }
            let Some(&p) = self.parents(c).first() else {
                break;
            };
            if self.names.contains_key(&p) {
                break;
            }
            self.name_parent(c, p);
            i += 1;
            c = p;
        }
        i
    }

    fn name_commits(&mut self, list: &[Oid], rev: &[Oid], ref_name: &[String]) {
        for &c in list {
            if self.names.contains_key(&c) {
                continue;
            }
            if let Some(i) = rev.iter().position(|&r| r == c) {
                self.names.insert(c, (ref_name[i].clone(), 0));
            }
        }
        while list
            .iter()
            .map(|&c| self.name_first_parent_chain(c))
            .sum::<usize>()
            > 0
        {}
        loop {
            let mut i = 0;
            for &c in list {
                let Some((head, generation)) = self.names.get(&c).cloned() else {
                    continue;
                };
                for (nth, p) in self.parents(c).into_iter().enumerate() {
                    if self.names.contains_key(&p) {
                        continue;
                    }
                    let mut name = match generation {
                        0 => head.clone(),
                        1 => format!("{head}^"),
                        g => format!("{head}~{g}"),
                    };
                    if nth == 0 {
                        name.push('^');
                    } else {
                        let _ = write!(name, "^{}", nth + 1);
                    }
                    self.names.insert(p, (name, 0));
                    i += 1;
                    self.name_first_parent_chain(p);
                }
            }
            if i == 0 {
                break;
            }
        }
    }

    fn show_one(&self, out: &mut String, id: Oid, no_name: bool) -> Result<(), GitError> {
        let subject = if self.parsed.contains_key(&id) {
            let c = self.repo.find_commit(id)?;
            let msg = String::from_utf8_lossy(c.message_bytes()).into_owned();
            let mut s = String::new();
            for line in msg.trim_start_matches('\n').split('\n') {
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(line);
            }
            s
        } else {
            "(unavailable)".to_owned()
        };
        let subject = subject.strip_prefix("[PATCH] ").unwrap_or(&subject);
        if !no_name {
            match self.names.get(&id) {
                Some((head, g)) => {
                    let _ = match g {
                        0 => write!(out, "[{head}] "),
                        1 => write!(out, "[{head}^] "),
                        g => write!(out, "[{head}~{g}] "),
                    };
                }
                None => {
                    let _ = write!(
                        out,
                        "[{}] ",
                        crate::plumbing::abbrev(self.repo, &id.to_string(), 0)?
                    );
                }
            }
        }
        let _ = writeln!(out, "{subject}");
        Ok(())
    }
}

/// The showbranch.default values: the arguments to use when none are given.
pub fn show_branch_defaults(git_dir: &Path) -> Vec<String> {
    let Ok(config) = Repository::open(git_dir).and_then(|r| r.config()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Ok(entries) = config.multivar("showbranch.default", None) {
        let _ = entries.for_each(|e| {
            if let Ok(v) = e.value() {
                out.push(v.to_owned());
            }
        });
    }
    out
}

/// `git show-branch`'s output and exit status. `relative` formats a reflog
/// entry's time as git's relative date.
pub fn show_branch(
    git_dir: &Path,
    o: &ShowBranchOpts,
    relative: &dyn Fn(i64, i32) -> String,
) -> Result<(String, i32), GitError> {
    let repo = Repository::open(git_dir)?;
    let mut refs: Vec<(String, Oid)> = repo
        .references()?
        .flatten()
        .filter_map(|r| {
            let name = r.name().ok()?.to_owned();
            let id = r.resolve().ok()?.target()?;
            Some((name, id))
        })
        .collect();
    refs.sort();
    let commit_of = |id: Oid| -> Option<Oid> {
        repo.find_object(id, None)
            .ok()?
            .peel_to_commit()
            .ok()
            .map(|c| c.id())
    };
    let get_oid = |rev: &str| repo.rev_single(rev).ok().map(|x| x.id());
    let mut names: Vec<(String, Oid)> = Vec::new();
    let append = |names: &mut Vec<(String, Oid)>, name: &str, id: Oid, dups: bool| {
        let Some(c) = commit_of(id) else {
            return;
        };
        if !dups && names.iter().any(|(n, _)| n == name) {
            return;
        }
        if names.len() >= MAX_REVS {
            eprintln!("warning: ignoring {name}; cannot handle more than {MAX_REVS} refs");
            return;
        }
        names.push((name.to_owned(), c));
    };
    // show-branch.c's append_head_ref / append_remote_ref: the short name,
    // or `heads/x` when the short one names something else.
    let short = |full: &str, ofs: usize, id: Oid| -> String {
        if get_oid(&full[ofs..]) == Some(id) {
            full[ofs..].to_owned()
        } else {
            full[5..].to_owned()
        }
    };
    let snarf = |names: &mut Vec<(String, Oid)>, prefix: &str| {
        let start = names.len();
        for (full, id) in &refs {
            if full.starts_with(prefix) {
                let n = short(full, prefix.len(), *id);
                append(names, &n, *id, false);
            }
        }
        names[start..].sort_by(|a, b| a.0.cmp(&b.0));
    };

    let mut all_heads = o.all;
    let all_remotes = o.all || o.remotes;
    let mut reflog_msgs = Vec::new();
    if o.reflog.is_some() && (o.independent || o.merge_base || o.extra > 0 || all_remotes) {
        return Err(GitError::Other(
            "options '--reflog' and '--all/--remotes/--independent/--merge-base' cannot be used together"
                .to_owned(),
        ));
    }
    if o.extra != 0 && (o.independent || o.merge_base) {
        return Err(GitError::Other(
            "--more and --list cannot be used with --independent or --merge-base".to_owned(),
        ));
    }
    let head_ref = repo.find_reference("HEAD").ok().and_then(|h| {
        let r = h.resolve().ok()?;
        Some((r.name().ok()?.to_owned(), r.target()?))
    });
    if o.current && o.reflog.is_some() {
        return Err(GitError::Other(
            "options '--reflog' and '--current' cannot be used together".to_owned(),
        ));
    }
    if o.revs.len() <= usize::from(o.topics) && !all_heads && !all_remotes {
        all_heads = true;
    }
    if let Some((count, base)) = &o.reflog {
        let arg = match o.revs.as_slice() {
            [] => head_ref.as_ref().map(|h| h.0.clone()).ok_or_else(|| {
                GitError::Other("no branches given, and HEAD is not valid".into())
            })?,
            [one] => one.clone(),
            _ => {
                return Err(GitError::Other(
                    "--reflog option needs one branch name".into(),
                ));
            }
        };
        let full = ["", "refs/", "refs/tags/", "refs/heads/", "refs/remotes/"]
            .iter()
            .map(|p| format!("{p}{arg}"))
            .find(|n| repo.find_reference(n).is_ok())
            .ok_or_else(|| GitError::Other(format!("no such ref {arg}")))?;
        if *count > MAX_REVS {
            return Err(GitError::Other(format!(
                "only {MAX_REVS} entries can be shown at one time."
            )));
        }
        let log = repo.reflog(&full)?;
        // A base that is not all digits is a date: start at the newest
        // entry made at or before it (read_ref_at), or past the oldest.
        let base = base.as_deref().unwrap_or("");
        let digits = base.len() - base.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let mut base: usize = base[..digits].parse().unwrap_or(0);
        if digits
            < o.reflog
                .as_ref()
                .and_then(|r| r.1.as_ref())
                .map_or(0, String::len)
            && let Some(at) = o.reflog_date
        {
            if log.is_empty() {
                return Err(GitError::Other(format!("log for {full} is empty")));
            }
            base = log
                .iter()
                .position(|e| e.committer().when().seconds() <= at)
                .unwrap_or(log.len());
        }
        for i in 0..*count {
            let Some(e) = log.get(base + i) else {
                break;
            };
            let msg = e.message().ok().flatten().unwrap_or("");
            let msg = msg.split('\n').next().unwrap_or("");
            let msg = if msg.is_empty() { "(none)" } else { msg };
            let when = e.committer().when();
            reflog_msgs.push(format!(
                "({}) {msg}",
                relative(when.seconds(), when.offset_minutes())
            ));
            append(
                &mut names,
                &format!("{arg}@{{{}}}", base + i),
                e.id_new(),
                true,
            );
        }
    } else {
        for rev in &o.revs {
            if let Some(id) = get_oid(rev) {
                append(&mut names, rev, id, false);
                continue;
            }
            if !rev.contains(['*', '?', '[']) {
                return Err(GitError::Other(format!("bad sha1 reference {rev}")));
            }
            let start = names.len();
            let want = rev.matches('/').count();
            for (full, id) in &refs {
                let mut slash = full.matches('/').count();
                let mut tail = full.as_str();
                while !tail.is_empty() && want < slash {
                    if tail.starts_with('/') {
                        slash -= 1;
                    }
                    tail = &tail[1..];
                }
                if tail.is_empty() || !crate::apply::wildmatch(rev, tail) {
                    continue;
                }
                if full.starts_with("refs/heads/") {
                    let n = short(full, 11, *id);
                    append(&mut names, &n, *id, false);
                } else if full.starts_with("refs/tags/") {
                    append(&mut names, &full[5..], *id, false);
                } else {
                    append(&mut names, full, *id, false);
                }
            }
            if start == names.len() && names.len() < MAX_REVS {
                eprintln!("error: no matching refs with {rev}");
            }
            names[start..].sort_by(|a, b| a.0.cmp(&b.0));
        }
        if all_heads {
            snarf(&mut names, "refs/heads/");
        }
        if all_remotes {
            snarf(&mut names, "refs/remotes/");
        }
    }
    let rev_is_head = |name: &str| -> bool {
        let Some((head, _)) = &head_ref else {
            return false;
        };
        let head = head.strip_prefix("refs/heads/").unwrap_or(head);
        let name = name
            .strip_prefix("refs/heads/")
            .or_else(|| name.strip_prefix("heads/"))
            .unwrap_or(name);
        head == name
    };
    if o.current
        && let Some((head, id)) = &head_ref
        && !names.iter().any(|(n, _)| rev_is_head(n))
    {
        let n = head.strip_prefix("refs/heads/").unwrap_or(head).to_owned();
        append(&mut names, &n, *id, false);
    }
    if names.is_empty() {
        eprintln!("No revs to be shown.");
        return Ok((String::new(), 0));
    }

    let mut w = Walk {
        repo: &repo,
        parsed: HashMap::new(),
        flags: HashMap::new(),
        names: HashMap::new(),
    };
    let num_rev = names.len();
    let mut seen = Vec::new();
    let mut list = VecDeque::new();
    let rev: Vec<Oid> = names.iter().map(|n| n.1).collect();
    let ref_name: Vec<String> = names.iter().map(|n| n.0.clone()).collect();
    for (i, &c) in rev.iter().enumerate() {
        let flag = 1u32 << (i as u32 + REV_SHIFT);
        w.parse(c)?;
        w.mark_seen(c, &mut seen);
        let f = w.flags.entry(c).or_default();
        *f |= flag;
        if *f == flag {
            w.insert_by_date(&mut list, c);
        }
    }
    let rev_mask: Vec<u32> = rev.iter().map(|&c| w.flag(c)).collect();
    if o.extra >= 0 {
        w.join_revs(&mut list, &mut seen, num_rev, o.extra)?;
    }
    seen.reverse();
    seen.sort_by_key(|&c| std::cmp::Reverse(w.date(c)));

    let mut out = String::new();
    let all_mask = (1u32 << (REV_SHIFT as usize + num_rev)) - 1;
    let all_revs = all_mask & !((1 << REV_SHIFT) - 1);
    if o.merge_base {
        let mut status = 1;
        for &c in &seen {
            let f = w.flag(c) & all_mask;
            if f & UNINTERESTING == 0 && f & all_revs == all_revs {
                let _ = writeln!(out, "{c}");
                status = 0;
                *w.flags.entry(c).or_default() |= UNINTERESTING;
            }
        }
        return Ok((out, status));
    }
    if o.independent {
        for (i, &c) in rev.iter().enumerate() {
            if w.flag(c) == rev_mask[i] {
                let _ = writeln!(out, "{c}");
            }
            *w.flags.entry(c).or_default() |= UNINTERESTING;
        }
        return Ok((out, 0));
    }

    let paint = |i: usize| -> (&str, &str) {
        if o.color {
            (COLORS[i % COLORS.len()], "\x1b[m")
        } else {
            ("", "")
        }
    };
    let mut head_at = None;
    if num_rev > 1 || o.extra < 0 {
        for i in 0..num_rev {
            let is_head =
                rev_is_head(&ref_name[i]) && head_ref.as_ref().is_some_and(|h| h.1 == rev[i]);
            if o.extra < 0 {
                let _ = write!(
                    out,
                    "{} [{}] ",
                    if is_head { '*' } else { ' ' },
                    ref_name[i]
                );
            } else {
                let (c, r) = paint(i);
                let _ = write!(
                    out,
                    "{}{c}{}{r} [{}] ",
                    " ".repeat(i),
                    if is_head { '*' } else { '!' },
                    ref_name[i]
                );
            }
            match reflog_msgs.get(i) {
                Some(m) if o.reflog.is_some() => {
                    let _ = writeln!(out, "{m}");
                }
                _ => w.show_one(&mut out, rev[i], true)?,
            }
            if is_head {
                head_at = Some(i);
            }
        }
        if o.extra >= 0 {
            out.push_str(&"-".repeat(num_rev));
            out.push('\n');
        }
    }
    if o.extra < 0 {
        return Ok((out, 0));
    }

    let seen = w.topo_sort(seen, o.date_order);
    if !o.sha1_name && !o.no_name {
        w.name_commits(&seen, &rev, &ref_name);
    }
    let mut extra = o.extra;
    let mut shown_merge_point = false;
    for &c in &seen {
        let this = w.flag(c);
        let is_merge_point = this & all_revs == all_revs;
        shown_merge_point |= is_merge_point;
        if num_rev > 1 {
            let is_merge = w.parents(c).len() > 1;
            if o.topics && !is_merge_point && this & (1 << REV_SHIFT) != 0 {
                continue;
            }
            if !o.sparse && is_merge && !rev.contains(&c) {
                let count = (0..num_rev)
                    .filter(|&i| this & (1 << (i as u32 + REV_SHIFT)) != 0)
                    .count();
                if count == 1 {
                    continue;
                }
            }
            for i in 0..num_rev {
                let mark = if this & (1 << (i as u32 + REV_SHIFT)) == 0 {
                    ' '
                } else if is_merge {
                    '-'
                } else if head_at == Some(i) {
                    '*'
                } else {
                    '+'
                };
                if mark == ' ' {
                    out.push(' ');
                } else {
                    let (c, r) = paint(i);
                    let _ = write!(out, "{c}{mark}{r}");
                }
            }
            out.push(' ');
        }
        w.show_one(&mut out, c, o.no_name)?;
        if shown_merge_point {
            extra -= 1;
            if extra < 0 {
                break;
            }
        }
    }
    Ok((out, 0))
}
