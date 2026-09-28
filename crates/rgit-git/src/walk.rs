//! git's revision walk (revision.c): which commits `log` and `rev-list` show,
//! in which order, with history simplification, symmetric-range marks,
//! cherry-pick detection, boundary commits and parent rewriting.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::rc::Rc;

use git2::{DiffOptions, Oid, Repository};

use crate::{GitError, LogOptions, LogOrder};

/// A commit a walk shows.
pub(crate) struct Walked {
    pub id: Oid,
    /// Its parents as the walk sees them: simplified, and rewritten to the
    /// nearest shown ancestors when `rewrite_parents` is set.
    pub parents: Vec<Oid>,
    pub mark: Option<char>,
    pub source: Option<String>,
}

/// A commit the walk started from, or (`bottom`) whose history it excludes.
struct Tip {
    id: Oid,
    bottom: bool,
    left: bool,
    name: Rc<str>,
}

/// What the walk learned about a commit it reached.
struct Node {
    /// The parents it follows, after simplification.
    parents: Vec<Oid>,
    /// How many parents it has for `--merges`: one once simplified.
    count: usize,
    treesame: bool,
    /// Per parent, whether the commit is the same as it where the walk looks
    /// (for merges walked with their full history).
    same: Vec<bool>,
    left: bool,
    source: Option<Rc<str>>,
    patch_same: bool,
}

fn commit_of(repo: &Repository, rev: &str) -> Result<Oid, GitError> {
    Ok(repo.revparse_single(rev)?.peel_to_commit()?.id())
}

/// The commits a walk starts from and excludes, in the order git queues them:
/// every ref then HEAD for `--all`, the globs, then each revision as given.
fn tips(repo: &Repository, opts: &LogOptions) -> Result<Vec<Tip>, GitError> {
    let mut tips = Vec::new();
    let mut add = |id: Oid, bottom: bool, left: bool, name: &str| {
        tips.push(Tip {
            id,
            bottom,
            left,
            name: name.into(),
        });
    };
    let refs = |glob: &str| -> Result<Vec<(String, Oid)>, GitError> {
        let mut refs: Vec<(String, Oid)> = repo
            .references_glob(glob)?
            .flatten()
            .filter_map(|r| {
                let id = r.peel_to_commit().ok()?.id();
                Some((String::from_utf8_lossy(r.name_bytes()).into_owned(), id))
            })
            .collect();
        refs.sort();
        Ok(refs)
    };
    let head = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    if opts.all {
        for (name, id) in refs("refs/*")? {
            add(id, false, false, &name);
        }
        if let Some(h) = &head {
            add(h.id(), false, false, "HEAD");
        }
    }
    for glob in &opts.globs {
        for (name, id) in refs(glob)? {
            add(id, false, false, &name);
        }
    }
    if opts.merge {
        let Some(h) = &head else {
            return Err(GitError::Other("--merge without HEAD?".into()));
        };
        let other = [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "REBASE_HEAD",
        ]
        .into_iter()
        .find_map(|name| Some((name, commit_of(repo, name).ok()?)));
        let Some((name, other)) = other else {
            return Err(GitError::Other(
                "--merge requires one of the pseudorefs MERGE_HEAD, CHERRY_PICK_HEAD, REVERT_HEAD or REBASE_HEAD".into(),
            ));
        };
        add(h.id(), false, false, "HEAD");
        add(other, false, false, name);
        for base in repo.merge_bases(h.id(), other)?.iter() {
            add(*base, true, false, "");
        }
    }
    let or_head = |s: &str| if s.is_empty() { "HEAD" } else { s }.to_owned();
    for rev in &opts.revs {
        if let Some(neg) = rev.strip_prefix('^') {
            add(commit_of(repo, neg)?, true, false, neg);
        } else if let Some((a, b)) = rev.split_once("...") {
            let (a, b) = (or_head(a), or_head(b));
            let (x, y) = (commit_of(repo, &a)?, commit_of(repo, &b)?);
            add(x, false, true, &a);
            add(y, false, false, &b);
            if let Ok(bases) = repo.merge_bases(x, y) {
                for base in bases.iter() {
                    add(*base, true, false, "");
                }
            }
        } else if let Some((a, b)) = rev.split_once("..") {
            let (a, b) = (or_head(a), or_head(b));
            add(commit_of(repo, &a)?, true, false, &a);
            add(commit_of(repo, &b)?, false, false, &b);
        } else if let Some(base) = rev.strip_suffix("^@") {
            for p in repo.find_commit(commit_of(repo, base)?)?.parent_ids() {
                add(p, false, false, rev);
            }
        } else if let Some(base) = rev.strip_suffix("^!") {
            let c = repo.find_commit(commit_of(repo, base)?)?;
            add(c.id(), false, false, base);
            for p in c.parent_ids() {
                add(p, true, false, base);
            }
        } else if let Some((base, n)) = rev
            .rsplit_once("^-")
            .filter(|(_, n)| n.is_empty() || n.bytes().all(|b| b.is_ascii_digit()))
        {
            let n = if n.is_empty() { "1" } else { n };
            add(commit_of(repo, &format!("{base}^{n}"))?, true, false, base);
            add(commit_of(repo, base)?, false, false, base);
        } else {
            add(commit_of(repo, rev)?, false, false, rev);
        }
    }
    if !opts.all
        && !opts.merge
        && opts.revs.is_empty()
        && opts.globs.is_empty()
        && let Some(h) = &head
    {
        add(h.id(), false, false, "HEAD");
    }
    Ok(tips)
}

/// Commits by committer date, newest first, ties in the order they came.
#[derive(Default)]
struct DateQueue {
    heap: BinaryHeap<(i64, std::cmp::Reverse<u64>, Oid)>,
    count: u64,
}

impl DateQueue {
    fn push(&mut self, time: i64, id: Oid) {
        self.heap.push((time, std::cmp::Reverse(self.count), id));
        self.count += 1;
    }

    fn pop(&mut self) -> Option<Oid> {
        self.heap.pop().map(|(_, _, id)| id)
    }
}

/// git's sort_in_topological_order: parents after all their children; ties
/// by `key` (newest first) or, with none, in graph order.
fn topo_sort(
    list: Vec<Oid>,
    parents: &dyn Fn(Oid) -> Vec<Oid>,
    key: Option<&dyn Fn(Oid) -> i64>,
) -> Vec<Oid> {
    let mut indegree: HashMap<Oid, usize> = list.iter().map(|&id| (id, 1)).collect();
    for &id in &list {
        for p in parents(id) {
            if let Some(n) = indegree.get_mut(&p) {
                *n += 1;
            }
        }
    }
    let mut stack = Vec::new();
    let mut queue = DateQueue::default();
    let put = |id: Oid, stack: &mut Vec<Oid>, queue: &mut DateQueue| match key {
        Some(key) => queue.push(key(id), id),
        None => stack.push(id),
    };
    for &id in &list {
        if indegree[&id] == 1 {
            put(id, &mut stack, &mut queue);
        }
    }
    stack.reverse();
    let mut out = Vec::with_capacity(list.len());
    loop {
        let next = match key {
            Some(_) => queue.pop(),
            None => stack.pop(),
        };
        let Some(id) = next else { break };
        for p in parents(id) {
            let Some(n) = indegree.get_mut(&p) else {
                continue;
            };
            if *n == 0 {
                continue;
            }
            *n -= 1;
            if *n == 1 {
                put(p, &mut stack, &mut queue);
            }
        }
        indegree.insert(id, 0);
        out.push(id);
    }
    out
}

/// Every commit reachable from the tips but not from the bottoms.
fn interesting(repo: &Repository, tips: &[Tip]) -> Result<HashSet<Oid>, GitError> {
    let mut walk = repo.revwalk()?;
    for t in tips {
        if t.bottom {
            walk.hide(t.id)?;
        } else {
            walk.push(t.id)?;
        }
    }
    Ok(walk.collect::<Result<_, _>>()?)
}

/// The patterns `grep`, `author` and `committer` compile to, as git's
/// grep_filter matches them: author and committer headers must match, then
/// any (or with `all_match` every) message pattern, or none with `invert_grep`.
struct Filter {
    grep: Vec<regex::Regex>,
    author: Option<regex::Regex>,
    committer: Option<regex::Regex>,
    changes: Option<regex::Regex>,
    all_match: bool,
    invert: bool,
}

impl Filter {
    fn new(opts: &LogOptions) -> Result<Filter, GitError> {
        let re = |p: &str, what: &str| {
            regex::RegexBuilder::new(&crate::plumbing::basic_to_extended(p))
                .case_insensitive(opts.grep_ignore_case)
                .multi_line(true)
                .build()
                .map_err(|e| GitError::Other(format!("bad {what} pattern: {e}")))
        };
        Ok(Filter {
            grep: opts
                .grep
                .iter()
                .map(|g| re(g, "--grep"))
                .collect::<Result<_, _>>()?,
            author: opts
                .author
                .as_deref()
                .map(|a| re(a, "--author"))
                .transpose()?,
            committer: opts
                .committer
                .as_deref()
                .map(|a| re(a, "--committer"))
                .transpose()?,
            changes: opts
                .changes_matching
                .as_deref()
                .map(regex::Regex::new)
                .transpose()
                .map_err(|e| GitError::Other(format!("bad -G pattern: {e}")))?,
            all_match: opts.all_match,
            invert: opts.invert_grep,
        })
    }

    fn matches(&self, commit: &git2::Commit<'_>) -> bool {
        let header = |re: &Option<regex::Regex>, who: git2::Signature<'_>| {
            re.as_ref().is_none_or(|re| {
                let t = who.when();
                let off = t.offset_minutes();
                let sign = if off < 0 { '-' } else { '+' };
                re.is_match(&format!(
                    "{} <{}> {} {sign}{:02}{:02}",
                    String::from_utf8_lossy(who.name_bytes()),
                    String::from_utf8_lossy(who.email_bytes()),
                    t.seconds(),
                    off.abs() / 60,
                    off.abs() % 60
                ))
            })
        };
        if !header(&self.author, commit.author()) || !header(&self.committer, commit.committer()) {
            return false;
        }
        if self.grep.is_empty() {
            return true;
        }
        let message = String::from_utf8_lossy(commit.message_bytes());
        let hit = if self.all_match {
            self.grep.iter().all(|re| re.is_match(&message))
        } else {
            self.grep.iter().any(|re| re.is_match(&message))
        };
        hit != self.invert
    }
}

/// What a streaming walk hands each commit to; false stops the walk.
type Stream<'a, 'r> = dyn FnMut(&mut Walker<'r>, Oid) -> Result<bool, GitError> + 'a;

/// Which commits the walk shows, and how it simplifies history.
struct Walker<'r> {
    repo: &'r Repository,
    opts: &'r LogOptions,
    /// Commits in the range, when it excludes some.
    allowed: Option<HashSet<Oid>>,
    /// Commits dropped from a limited list (`--ancestry-path`).
    dropped: HashSet<Oid>,
    decorated: HashSet<Oid>,
    bottoms: HashSet<Oid>,
    prune: bool,
    simplify_merges: bool,
    /// Name each shown commit's parents as its nearest shown ancestors.
    rewrite: bool,
    /// Whether the whole list is walked before any commit is shown.
    limited: bool,
    queue: DateQueue,
    seen: HashSet<Oid>,
    sources: HashMap<Oid, Rc<str>>,
    lefts: HashSet<Oid>,
    nodes: HashMap<Oid, Node>,
}

impl<'r> Walker<'r> {
    /// Whether the range keeps a commit (git's !UNINTERESTING).
    fn interesting(&self, id: Oid) -> bool {
        self.allowed.as_ref().is_none_or(|a| a.contains(&id)) && !self.dropped.contains(&id)
    }

    /// git's relevant_commit: interesting, or an excluded end of the range.
    fn relevant(&self, id: Oid) -> bool {
        self.interesting(id) || self.bottoms.contains(&id)
    }

    /// git's rev_compare_tree: whether `commit` is the same as `parent` (the
    /// empty tree for None) where the walk looks.
    fn same(&self, commit: &git2::Commit<'_>, parent: Option<&git2::Commit<'_>>) -> bool {
        if self.opts.simplify_by_decoration && parent.is_some() {
            if self.decorated.contains(&commit.id()) {
                return false;
            }
            if self.opts.paths.is_empty() {
                return true;
            }
        }
        let mut dopts = DiffOptions::new();
        for p in self.opts.paths.iter().filter(|p| *p != ".") {
            dopts.pathspec(p);
        }
        let old = parent.and_then(|p| p.tree().ok());
        let Ok(tree) = commit.tree() else {
            return false;
        };
        self.repo
            .diff_tree_to_tree(old.as_ref(), Some(&tree), Some(&mut dopts))
            .is_ok_and(|d| d.deltas().len() == 0)
    }

    /// git's try_to_simplify_commit: whether `commit` is TREESAME, its
    /// parents (the one it is the same as, when simplified to it), and per
    /// parent whether it is the same as that one.
    fn simplify(&self, commit: &git2::Commit<'_>) -> (bool, Vec<Oid>, Vec<bool>) {
        let parents: Vec<Oid> = commit.parent_ids().collect();
        if !self.prune || self.opts.follow {
            return (false, parents, Vec::new());
        }
        if parents.is_empty() {
            return (self.same(commit, None), parents, Vec::new());
        }
        let simplify_history = !self.opts.full_history && !self.simplify_merges;
        let (mut relevant, mut relevant_change, mut irrelevant_change) = (false, false, false);
        let mut same = vec![false; parents.len()];
        let consider = if self.opts.first_parent {
            1
        } else {
            parents.len()
        };
        for (i, &p) in parents.iter().enumerate().take(consider) {
            let rel = self.relevant(p);
            relevant |= rel;
            same[i] = commit
                .parent(i)
                .is_ok_and(|parent| self.same(commit, Some(&parent)));
            if same[i] {
                if simplify_history && rel {
                    return (true, vec![p], vec![true]);
                }
            } else if rel {
                relevant_change = true;
            } else {
                irrelevant_change = true;
            }
        }
        let changed = if relevant {
            relevant_change
        } else {
            irrelevant_change
        };
        (!changed, parents, same)
    }

    /// Queue the commits the walk starts from.
    fn start(&mut self, tips: &[Tip]) -> Result<(), GitError> {
        for t in tips {
            if t.bottom || !self.interesting(t.id) {
                continue;
            }
            if self.opts.source {
                self.sources.entry(t.id).or_insert_with(|| t.name.clone());
            }
            if t.left {
                self.lefts.insert(t.id);
            }
            if self.seen.insert(t.id) {
                self.queue
                    .push(self.repo.find_commit(t.id)?.time().seconds(), t.id);
            }
        }
        Ok(())
    }

    /// git's process_parents: simplify a commit and queue the parents it
    /// follows, once.
    fn process(&mut self, id: Oid) -> Result<(), GitError> {
        if self.nodes.contains_key(&id) {
            return Ok(());
        }
        let commit = self.repo.find_commit(id)?;
        let (treesame, parents, same) = self.simplify(&commit);
        let left = self.lefts.contains(&id);
        let source = self.sources.get(&id).cloned();
        let follow = if self.opts.first_parent {
            1
        } else {
            parents.len()
        };
        for &p in parents.iter().take(follow) {
            if let Some(s) = &source {
                self.sources.entry(p).or_insert_with(|| s.clone());
            }
            if left {
                self.lefts.insert(p);
            }
            if self.interesting(p) && self.seen.insert(p) {
                self.queue
                    .push(self.repo.find_commit(p)?.time().seconds(), p);
            }
        }
        let count = if treesame && parents.len() == 1 && commit.parent_count() > 1 {
            1
        } else {
            commit.parent_count()
        };
        self.nodes.insert(
            id,
            Node {
                parents,
                count,
                treesame,
                same,
                left,
                source,
                patch_same: false,
            },
        );
        Ok(())
    }

    /// Walk from `tips` in date order, recording each commit reached. With
    /// `stream`, each commit goes to it as it is reached, until it says stop.
    fn walk(
        &mut self,
        tips: &[Tip],
        mut stream: Option<&mut Stream<'_, 'r>>,
    ) -> Result<Vec<Oid>, GitError> {
        self.start(tips)?;
        let mut order = Vec::new();
        while let Some(id) = self.queue.pop() {
            let commit = self.repo.find_commit(id)?;
            if self.opts.since.is_some_and(|s| commit.time().seconds() < s) {
                continue;
            }
            self.process(id)?;
            match stream.as_deref_mut() {
                Some(f) => {
                    if !f(self, id)? {
                        break;
                    }
                }
                None => order.push(id),
            }
        }
        // A commit can learn it is on the left after it was reached.
        for (id, node) in &mut self.nodes {
            node.left |= self.lefts.contains(id);
        }
        Ok(order)
    }

    /// git's cherry_pick_list: mark commits whose patch the other side of the
    /// range has too.
    fn cherry(&mut self, list: &[Oid]) -> Result<(), GitError> {
        let sides: Vec<(Oid, bool)> = list
            .iter()
            .filter(|id| self.nodes[*id].count < 2)
            .map(|&id| (id, self.nodes[&id].left))
            .collect();
        let lefts = sides.iter().filter(|(_, l)| *l).count();
        if lefts == 0 || lefts == sides.len() {
            return Ok(());
        }
        let mut ids: HashMap<Oid, Vec<Oid>> = HashMap::new();
        for &(id, _) in &sides {
            let pid = crate::format_patch::patch_id(self.repo, &self.repo.find_commit(id)?)?;
            ids.entry(pid).or_default().push(id);
        }
        for group in ids.values() {
            let l = group.iter().any(|id| self.nodes[id].left);
            let r = group.iter().any(|id| !self.nodes[id].left);
            if l && r {
                for id in group {
                    self.nodes.get_mut(id).expect("walked").patch_same = true;
                }
            }
        }
        Ok(())
    }

    /// git's limit_to_ancestry: drop the commits that do not descend from a
    /// bottom.
    fn ancestry(&mut self, list: &[Oid], bottoms: &HashSet<Oid>) {
        let mut marked = bottoms.clone();
        loop {
            let mut progress = false;
            for id in list.iter().rev() {
                if marked.contains(id) {
                    continue;
                }
                if self.nodes[id].parents.iter().any(|p| marked.contains(p)) {
                    marked.insert(*id);
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }
        self.dropped
            .extend(list.iter().filter(|id| !marked.contains(id)));
    }

    /// Whether a walked commit is shown: git's get_commit_action, less the
    /// counting.
    fn shown(
        &self,
        id: Oid,
        filter: &Filter,
        follow: &mut Option<String>,
    ) -> Result<bool, GitError> {
        let node = &self.nodes[&id];
        if self.dropped.contains(&id) {
            return Ok(false);
        }
        if let Some(left) = self.opts.side
            && node.left != left
        {
            return Ok(false);
        }
        if self.opts.cherry == Some(true) && node.patch_same {
            return Ok(false);
        }
        let commit = self.repo.find_commit(id)?;
        if self.opts.until.is_some_and(|u| commit.time().seconds() > u) {
            return Ok(false);
        }
        if self.opts.merges.is_some_and(|m| m != (node.count > 1)) {
            return Ok(false);
        }
        if !filter.matches(&commit) {
            return Ok(false);
        }
        if self.prune && !self.opts.sparse && node.treesame && !self.opts.follow {
            // Merges tie shown history together when parents are rewritten.
            let ties =
                self.rewrite && node.parents.iter().filter(|p| self.relevant(**p)).count() >= 2;
            if !ties {
                return Ok(false);
            }
        }
        if let Some(path) = follow {
            // git's --follow prunes nothing; it shows the commits whose own
            // diff touches the file, which leaves merges out.
            if commit.parent_count() > 1 || !crate::git_repo::follow_path(self.repo, &commit, path)
            {
                return Ok(false);
            }
        }
        if (self.opts.occurrences.is_some() || filter.changes.is_some())
            && !crate::git_repo::pickaxe(self.repo, &commit, self.opts, filter.changes.as_ref())?
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// git's rewrite_parents: each parent of `id` as the nearest ancestor
    /// shown. Unless the list was limited first, this walks ahead.
    fn rewrite(&mut self, id: Oid) -> Result<Vec<Oid>, GitError> {
        let parents = self.nodes[&id].parents.clone();
        let mut out: Vec<Oid> = Vec::new();
        for mut p in parents {
            let kept = loop {
                if !self.limited {
                    self.process(p)?;
                }
                if !self.interesting(p) {
                    break Some(p);
                }
                let Some(node) = self.nodes.get(&p) else {
                    break Some(p);
                };
                if !node.treesame || !self.prune {
                    break Some(p);
                }
                if node.parents.is_empty() {
                    break None;
                }
                let next = if node.parents.len() == 1 || self.opts.first_parent {
                    Some(node.parents[0])
                } else {
                    let mut rel = node.parents.iter().filter(|q| self.relevant(**q));
                    match (rel.next(), rel.next()) {
                        (Some(&q), None) => Some(q),
                        _ => None,
                    }
                };
                match next {
                    Some(q) => p = q,
                    None => break Some(p),
                }
            };
            if let Some(p) = kept.filter(|p| !out.contains(p)) {
                out.push(p);
            }
        }
        self.nodes.get_mut(&id).expect("walked").parents = out.clone();
        Ok(out)
    }

    /// git's simplify_merges: rewrite each commit's parents to what they
    /// simplify to, drop parents another parent descends from, and keep the
    /// commits that simplify to themselves.
    fn simplify_merges(&mut self, list: &[Oid]) -> Vec<Oid> {
        let mut to: HashMap<Oid, Oid> = HashMap::new();
        let mut todo: Vec<Oid> = list.iter().rev().copied().collect();
        while !todo.is_empty() {
            for c in std::mem::take(&mut todo) {
                self.simplify_one(c, &mut to, &mut todo);
            }
        }
        list.iter()
            .filter(|c| to.get(c) == Some(c))
            .copied()
            .collect()
    }

    fn simplify_one(&mut self, c: Oid, to: &mut HashMap<Oid, Oid>, todo: &mut Vec<Oid>) {
        if to.contains_key(&c) {
            return;
        }
        let first_parent = self.opts.first_parent;
        let parents = match self.nodes.get(&c) {
            Some(n) if self.interesting(c) && !n.parents.is_empty() => n.parents.clone(),
            _ => {
                to.insert(c, c);
                return;
            }
        };
        let take = if first_parent { 1 } else { parents.len() };
        let mut waiting = false;
        for &p in parents.iter().take(take) {
            if to.contains_key(&p) {
                continue;
            }
            if self.nodes.contains_key(&p) && self.interesting(p) {
                todo.push(p);
                waiting = true;
            } else {
                to.insert(p, p);
            }
        }
        if waiting {
            todo.push(c);
            return;
        }
        let mut node = self.nodes.remove(&c).expect("walked");
        for p in node.parents.iter_mut().take(take) {
            *p = to[p];
        }
        node.same.resize(node.parents.len(), false);
        let keep: Vec<bool> = (0..node.parents.len())
            .map(|i| !node.parents[..i].contains(&node.parents[i]))
            .collect();
        let retain = |node: &mut Node, keep: &[bool]| {
            let mut k = keep.iter();
            node.same.retain(|_| *k.next().expect("same per parent"));
            let mut k = keep.iter();
            node.parents.retain(|_| *k.next().expect("keep per parent"));
        };
        if !first_parent {
            retain(&mut node, &keep);
        }
        if !first_parent && node.parents.len() > 1 {
            let n = node.parents.len();
            let mut marked: Vec<bool> = (0..n)
                .map(|i| {
                    let a = node.parents[i];
                    let rooted = self.nodes.get(&a);
                    (0..n).any(|j| {
                        j != i
                            && self
                                .repo
                                .graph_descendant_of(node.parents[j], a)
                                .unwrap_or(false)
                    }) || rooted.is_some_and(|r| r.parents.is_empty() && r.treesame)
                })
                .collect();
            if marked.iter().any(|m| *m) {
                let mut first_marked = None;
                let mut unmarked = false;
                for (i, &same) in node.same.iter().enumerate() {
                    if same {
                        if marked[i] {
                            first_marked.get_or_insert(i);
                        } else {
                            unmarked = true;
                            break;
                        }
                    }
                }
                if let (false, Some(i)) = (unmarked, first_marked) {
                    marked[i] = false;
                }
                if marked.iter().any(|m| *m) {
                    let keep: Vec<bool> = marked.iter().map(|m| !m).collect();
                    retain(&mut node, &keep);
                    node.treesame = if node.parents.len() == 1 {
                        node.same[0]
                    } else {
                        let (mut rel, mut rel_change, mut irr_change) = (false, false, false);
                        for (p, same) in node.parents.iter().zip(&node.same) {
                            if self.relevant(*p) {
                                rel = true;
                                rel_change |= !same;
                            } else {
                                irr_change |= !same;
                            }
                        }
                        !(if rel { rel_change } else { irr_change })
                    };
                }
            }
        }
        let parent = if node.parents.len() == 1 || first_parent {
            node.parents.first().copied()
        } else {
            let mut rel = node.parents.iter().filter(|q| self.relevant(**q));
            match (rel.next(), rel.next()) {
                (Some(&q), None) => Some(q),
                _ => None,
            }
        };
        let target = match parent {
            Some(p) if node.treesame && !node.parents.is_empty() => to[&p],
            _ => c,
        };
        to.insert(c, target);
        self.nodes.insert(c, node);
    }

    fn mark(&self, id: Oid, symmetric: bool) -> Option<char> {
        let node = self.nodes.get(&id)?;
        if node.patch_same {
            Some('=')
        } else if symmetric {
            Some(if node.left { '<' } else { '>' })
        } else {
            None
        }
    }
}

/// The commits a log shows for `opts`, in order, after its skip and limit.
pub(crate) fn walk(repo: &Repository, opts: &LogOptions) -> Result<Vec<Walked>, GitError> {
    let mut opts = std::borrow::Cow::Borrowed(opts);
    if opts.merge && opts.paths.is_empty() {
        let conflicted: Vec<String> = repo
            .index()?
            .conflicts()?
            .flatten()
            .filter_map(|c| {
                let e = c.our.or(c.their).or(c.ancestor)?;
                Some(String::from_utf8_lossy(&e.path).into_owned())
            })
            .collect();
        opts.to_mut().paths = conflicted;
    }
    let opts = &*opts;
    if opts.follow && opts.paths.len() != 1 {
        return Err(GitError::Other(
            "--follow requires exactly one pathspec".into(),
        ));
    }
    let tips = tips(repo, opts)?;
    let bottoms: HashSet<Oid> = tips.iter().filter(|t| t.bottom).map(|t| t.id).collect();
    let symmetric = tips.iter().any(|t| t.left);
    let prune = (!opts.paths.is_empty() && !opts.follow) || opts.simplify_by_decoration;
    let decorated = if opts.simplify_by_decoration {
        let mut set: HashSet<Oid> = repo
            .references()?
            .flatten()
            .filter(|r| {
                let n = String::from_utf8_lossy(r.name_bytes()).into_owned();
                ["refs/heads/", "refs/remotes/", "refs/tags/"]
                    .iter()
                    .any(|p| n.starts_with(p))
                    || n == "refs/stash"
            })
            .filter_map(|r| r.peel_to_commit().ok().map(|c| c.id()))
            .collect();
        set.extend(
            repo.head()
                .ok()
                .and_then(|h| h.peel_to_commit().ok())
                .map(|c| c.id()),
        );
        set
    } else {
        HashSet::new()
    };
    let mut w = Walker {
        repo,
        opts,
        allowed: None,
        dropped: HashSet::new(),
        decorated,
        bottoms: bottoms.clone(),
        prune,
        simplify_merges: prune && (opts.simplify_merges || opts.simplify_by_decoration),
        rewrite: opts.rewrite_parents || opts.simplify_merges || opts.simplify_by_decoration,
        limited: false,
        queue: DateQueue::default(),
        seen: HashSet::new(),
        sources: HashMap::new(),
        lefts: HashSet::new(),
        nodes: HashMap::new(),
    };
    let filter = Filter::new(opts)?;
    let mut follow = opts.paths.first().filter(|_| opts.follow).cloned();
    let mut passed = 0usize;
    let mut shown: Vec<(Oid, Vec<Oid>)> = Vec::new();
    w.limited = !bottoms.is_empty()
        || w.simplify_merges
        || opts.order != LogOrder::Walk
        || opts.ancestry_path
        || opts.cherry.is_some();
    if let Some(sorted) = opts.no_walk {
        let mut list: Vec<Oid> = Vec::new();
        for t in tips.iter().filter(|t| !t.bottom) {
            if !list.contains(&t.id) && !bottoms.contains(&t.id) {
                list.push(t.id);
            }
        }
        if sorted {
            let time = |id: &Oid| repo.find_commit(*id).map_or(0, |c| c.time().seconds());
            list.sort_by_key(|id| std::cmp::Reverse(time(id)));
        }
        for id in list {
            let commit = repo.find_commit(id)?;
            w.nodes.insert(
                id,
                Node {
                    parents: commit.parent_ids().collect(),
                    count: commit.parent_count(),
                    treesame: false,
                    same: Vec::new(),
                    left: false,
                    source: tips
                        .iter()
                        .find(|t| t.id == id && opts.source)
                        .map(|t| t.name.clone()),
                    patch_same: false,
                },
            );
            if w.shown(id, &filter, &mut follow)? {
                passed += 1;
                if passed > opts.offset && shown.len() < opts.limit {
                    shown.push((id, w.nodes[&id].parents.clone()));
                }
            }
        }
    } else if w.limited {
        if !bottoms.is_empty() {
            w.allowed = Some(interesting(repo, &tips)?);
        }
        let mut list = w.walk(&tips, None)?;
        if opts.cherry.is_some() {
            w.cherry(&list)?;
        }
        if opts.ancestry_path {
            w.ancestry(&list, &bottoms);
        }
        let nodes = &w.nodes;
        let parents = |id: Oid| nodes.get(&id).map_or(Vec::new(), |n| n.parents.clone());
        let author = |id: Oid| {
            repo.find_commit(id)
                .map_or(0, |c| c.author().when().seconds())
        };
        let committer = |id: Oid| repo.find_commit(id).map_or(0, |c| c.time().seconds());
        list = match (opts.order, w.simplify_merges) {
            (LogOrder::Walk, false) => list,
            (LogOrder::Walk, true) => topo_sort(list, &parents, None),
            (LogOrder::Topo, _) => topo_sort(list, &parents, None),
            (LogOrder::Date, _) => topo_sort(list, &parents, Some(&committer)),
            (LogOrder::AuthorDate, _) => topo_sort(list, &parents, Some(&author)),
        };
        if w.simplify_merges {
            list = w.simplify_merges(&list);
        }
        for id in list {
            if shown.len() >= opts.limit {
                break;
            }
            if w.shown(id, &filter, &mut follow)? {
                let parents = if w.rewrite {
                    w.rewrite(id)?
                } else {
                    w.nodes[&id].parents.clone()
                };
                passed += 1;
                if passed > opts.offset {
                    shown.push((id, parents));
                }
            }
        }
    } else if opts.limit > 0 {
        let mut stream = |w: &mut Walker<'_>, id: Oid| -> Result<bool, GitError> {
            if w.shown(id, &filter, &mut follow)? {
                let parents = if w.rewrite {
                    w.rewrite(id)?
                } else {
                    w.nodes[&id].parents.clone()
                };
                passed += 1;
                if passed > opts.offset {
                    shown.push((id, parents));
                }
            }
            Ok(shown.len() < opts.limit)
        };
        w.walk(&tips, Some(&mut stream))?;
    }
    let mut out: Vec<Walked> = shown
        .iter()
        .map(|(id, parents)| Walked {
            id: *id,
            parents: parents.clone(),
            mark: w.mark(*id, symmetric),
            source: w.nodes[id].source.as_deref().map(str::to_owned),
        })
        .collect();
    if opts.boundary {
        // Commits --cherry-pick, --left-only and --right-only leave out count
        // as shown.
        let mut done: HashSet<Oid> = shown.iter().map(|(id, _)| *id).collect();
        done.extend(w.nodes.iter().filter_map(|(id, n)| {
            let dropped = (opts.cherry == Some(true) && n.patch_same)
                || opts.side.is_some_and(|l| l != n.left);
            dropped.then_some(*id)
        }));
        let mut found: Vec<Oid> = Vec::new();
        for c in &out {
            for &p in &c.parents {
                if !done.contains(&p) && !found.contains(&p) {
                    found.push(p);
                }
            }
        }
        found.reverse();
        // git never reads a second parent --first-parent does not follow,
        // so it knows no parents of it.
        let raw = |id: Oid| -> Vec<Oid> {
            match w.nodes.get(&id) {
                Some(n) => n.parents.clone(),
                None if opts.first_parent && w.interesting(id) => Vec::new(),
                None => repo
                    .find_commit(id)
                    .map_or(Vec::new(), |c| c.parent_ids().collect()),
            }
        };
        for id in topo_sort(found, &raw, None) {
            out.push(Walked {
                id,
                parents: raw(id),
                mark: Some('-'),
                source: w
                    .nodes
                    .get(&id)
                    .and_then(|n| n.source.as_deref().map(str::to_owned)),
            });
        }
    }
    if opts.reverse {
        out.reverse();
    }
    Ok(out)
}
