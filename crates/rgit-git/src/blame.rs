//! `git blame`, natively: each line's suspect is passed from a commit to its
//! parents along the lines their diff leaves alone, with git's rename, move
//! (`-M`), copy (`-C`), `--reverse` and ignored-revision handling.

use crate::rev::RevParse;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::rc::Rc;

use git2::{DiffFindOptions, DiffOptions, ObjectType, Oid, Patch, Repository};

use crate::{BlameLine, GitError};

/// What [`crate::GitBackend::blame_with`] annotates, and how.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlameOptions {
    pub path: String,
    /// The commit to blame from (none: the working tree), `^A` or `A..B` to
    /// stop at A's history; with `reverse`, the range to walk forward.
    pub revs: Vec<String>,
    /// git's `-L` specs: `N,M`, `N,+K`, `/regex/`, `:funcname`...
    pub ranges: Vec<String>,
    /// `-w`: ignore whitespace when comparing lines.
    pub ignore_whitespace: bool,
    /// `-M[score]`: find lines moved within the file (0: the default score).
    pub moves: Option<u32>,
    /// How many `-C` were given (up to 3), and the score of the last one.
    pub copies: u8,
    pub copy_score: u32,
    pub first_parent: bool,
    pub reverse: bool,
    /// Blame root commits instead of marking them as boundaries.
    pub show_root: bool,
    /// Revisions whose changes are passed through (`--ignore-rev`).
    pub ignore_revs: Vec<String>,
    /// Files of revisions to ignore, after blame.ignoreRevsFile; an empty
    /// name forgets those before it.
    pub ignore_revs_files: Vec<String>,
    /// `--contents`: blame these bytes as the working tree's version, on
    /// top of the commit given (else HEAD).
    pub contents: Option<Vec<u8>>,
}

/// A blame's lines, the file's length and git's `--show-stats` counters.
#[derive(Debug, Clone, Default)]
pub struct Blame {
    pub lines: Vec<BlameLine>,
    pub total: usize,
    /// Blobs read, patches computed and commits examined.
    pub stats: [usize; 3],
    /// Each group of lines as blame settled it, in that order: its first
    /// line in the file and its length (git's `--incremental`).
    pub found: Vec<(usize, usize)>,
}

struct Text {
    buf: Vec<u8>,
    starts: Vec<usize>,
}

impl Text {
    fn new(buf: Vec<u8>) -> Text {
        let mut starts = vec![0];
        starts.extend(
            buf.iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i + 1),
        );
        if starts.last() != Some(&buf.len()) {
            starts.push(buf.len());
        }
        Text { buf, starts }
    }

    fn len(&self) -> usize {
        self.starts.len() - 1
    }

    fn span(&self, from: usize, to: usize) -> &[u8] {
        let at = |i: usize| self.starts[i.min(self.len())];
        &self.buf[at(from)..at(to)]
    }
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    lno: usize,
    s_lno: usize,
    num: usize,
    suspect: usize,
    ignored: bool,
    unblamable: bool,
}

impl Entry {
    /// Cut `self` after `len` lines; the rest, without the ignore marks.
    fn cut(&mut self, len: usize) -> Entry {
        let rest = Entry {
            lno: self.lno + len,
            s_lno: self.s_lno + len,
            num: self.num - len,
            ignored: false,
            unblamable: false,
            ..*self
        };
        self.num = len;
        rest
    }
}

type Split = [Option<Entry>; 3];

struct Origin {
    commit: Oid,
    path: String,
    blob: Oid,
    text: Option<Rc<Text>>,
    prints: Option<Rc<Vec<Print>>>,
    suspects: Vec<Entry>,
    previous: Option<usize>,
}

/// A line's byte pairs, for matching lines an ignored commit changed.
type Print = HashMap<u16, u16>;

const COPY_HARDER: u8 = 2;
const COPY_HARDEST: u8 = 3;

struct Board<'r> {
    repo: &'r Repository,
    final_text: Rc<Text>,
    origins: Vec<Origin>,
    by_commit: HashMap<Oid, Vec<usize>>,
    commits: HashMap<Oid, (i64, Vec<Oid>)>,
    queue: BinaryHeap<(i64, Reverse<u64>, Oid)>,
    seq: u64,
    done: Vec<Entry>,
    found: Vec<(usize, usize)>,
    boundary: HashSet<Oid>,
    children: HashMap<Oid, Vec<Oid>>,
    worktree_parents: Vec<Oid>,
    reverse: bool,
    first_parent: bool,
    show_root: bool,
    ignore: HashSet<Oid>,
    move_score: Option<u32>,
    copies: u8,
    copy_score: u32,
    whitespace: bool,
    stats: [usize; 3],
}

fn err(message: impl Into<String>) -> GitError {
    GitError::Other(message.into())
}

pub(crate) fn blame(
    repo: &Repository,
    workdir: &Path,
    opts: &BlameOptions,
) -> Result<Blame, GitError> {
    let commit_of =
        |rev: &str| -> Result<Oid, GitError> { Ok(repo.rev_single(rev)?.peel_to_commit()?.id()) };
    let (mut pos, mut neg) = (Vec::new(), Vec::new());
    for rev in &opts.revs {
        if let Some((a, b)) = rev.split_once("..") {
            let b = b.strip_prefix('.').unwrap_or(b);
            neg.push(commit_of(if a.is_empty() { "HEAD" } else { a })?);
            pos.push(commit_of(if b.is_empty() { "HEAD" } else { b })?);
        } else if let Some(r) = rev.strip_prefix('^') {
            neg.push(commit_of(r)?);
        } else {
            pos.push(commit_of(rev)?);
        }
    }
    if opts.reverse && neg.is_empty() && pos.len() == 1 {
        neg.push(pos.remove(0));
        pos.push(repo.head()?.peel_to_commit()?.id());
    }
    if pos.len() > 1 {
        return Err(err("More than one commit to dig from"));
    }
    let mut board = Board {
        repo,
        final_text: Rc::new(Text::new(Vec::new())),
        origins: Vec::new(),
        by_commit: HashMap::new(),
        commits: HashMap::new(),
        queue: BinaryHeap::new(),
        seq: 0,
        done: Vec::new(),
        found: Vec::new(),
        boundary: HashSet::new(),
        children: HashMap::new(),
        worktree_parents: Vec::new(),
        reverse: opts.reverse,
        first_parent: opts.first_parent,
        show_root: opts.show_root
            || repo
                .config()
                .and_then(|c| c.get_bool("blame.showroot"))
                .unwrap_or(false),
        ignore: ignored(repo, workdir, opts)?,
        // `-C` implies `-M`.
        move_score: (opts.moves.is_some() || opts.copies > 0)
            .then(|| opts.moves.filter(|s| *s > 0).unwrap_or(20)),
        copies: opts.copies.min(3),
        copy_score: if opts.copy_score == 0 {
            40
        } else {
            opts.copy_score
        },
        whitespace: opts.ignore_whitespace,
        stats: [0; 3],
    };
    if !neg.is_empty() {
        let mut walk = repo.revwalk()?;
        for n in &neg {
            walk.push(*n)?;
        }
        board.boundary = walk.collect::<Result<_, _>>()?;
    }
    let (final_commit, blob) = if opts.reverse {
        let [start] = neg[..] else {
            return Err(err("--reverse needs one range to walk, as A..B"));
        };
        let end = match pos.first() {
            Some(end) => *end,
            None => repo.head()?.peel_to_commit()?.id(),
        };
        board.children_between(start, end)?;
        (start, board.blob_at(start, &opts.path))
    } else if let (Some(c), None) = (pos.first(), &opts.contents) {
        (*c, board.blob_at(*c, &opts.path))
    } else {
        let buf = match &opts.contents {
            Some(buf) => buf.clone(),
            None => std::fs::read(workdir.join(&opts.path))?,
        };
        board.final_text = Rc::new(Text::new(buf));
        if let Some(c) = pos.first() {
            board.worktree_parents.push(*c);
        } else if let Ok(head) = repo.head().and_then(|h| h.peel_to_commit()) {
            board.worktree_parents.push(head.id());
        }
        if opts.contents.is_none()
            && let Ok(merge) = std::fs::read_to_string(repo.path().join("MERGE_HEAD"))
        {
            board.worktree_parents.extend(
                merge
                    .split_whitespace()
                    .filter_map(|l| Oid::from_str(l).ok()),
            );
        }
        let blob = Oid::hash_object(ObjectType::Blob, &board.final_text.buf)?;
        (Oid::ZERO_SHA1, Some(blob))
    };
    let Some(blob) = blob else {
        return Err(err(format!(
            "no such path {} in {}",
            opts.path,
            if opts.reverse {
                "the range's start"
            } else {
                "the commit"
            }
        )));
    };
    if !final_commit.is_zero() {
        board.final_text = Rc::new(Text::new(repo.find_blob(blob)?.content().to_vec()));
        board.stats[0] += 1;
    }
    let total = board.final_text.len();
    let ranges = ranges(repo, &opts.path, &board.final_text, &opts.ranges)?;
    let o = board.origin(final_commit, &opts.path, blob);
    if final_commit.is_zero() {
        board.origins[o].text = Some(board.final_text.clone());
    }
    let entries = ranges
        .into_iter()
        .map(|(a, b)| Entry {
            lno: a,
            s_lno: a,
            num: b - a,
            suspect: o,
            ignored: false,
            unblamable: false,
        })
        .collect();
    board.queue_blames(o, entries);
    board.assign()?;
    let mut lines = board.lines()?;
    if opts.contents.is_some() {
        for b in lines.iter_mut().filter(|b| b.id.is_empty()) {
            b.author = "External file (--contents)".to_owned();
            b.email = "external.file".to_owned();
        }
    }
    Ok(Blame {
        lines,
        total,
        stats: board.stats,
        found: std::mem::take(&mut board.found),
    })
}

/// The revisions to pass through: blame.ignoreRevsFile, `--ignore-revs-file`
/// (an empty one forgets the files before it) and `--ignore-rev`.
fn ignored(
    repo: &Repository,
    workdir: &Path,
    opts: &BlameOptions,
) -> Result<HashSet<Oid>, GitError> {
    let mut files = Vec::new();
    if let Ok(config) = repo.config() {
        let mut entries = config.multivar("blame.ignorerevsfile", None)?;
        while let Some(e) = entries.next() {
            files.extend(e?.value().map(str::to_owned));
        }
    }
    for f in &opts.ignore_revs_files {
        if f.is_empty() {
            files.clear();
        } else {
            files.push(f.clone());
        }
    }
    let mut out = HashSet::new();
    for f in files {
        let text = std::fs::read_to_string(workdir.join(&f))
            .map_err(|e| err(format!("could not open object name list: {f}: {e}")))?;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let id = Oid::from_str(line)
                .ok()
                .filter(|_| line.len() == 40)
                .ok_or_else(|| err(format!("invalid object name: {line}")))?;
            if let Ok(c) = repo.find_object(id, None).and_then(|o| o.peel_to_commit()) {
                out.insert(c.id());
            }
        }
    }
    for rev in &opts.ignore_revs {
        let c = repo
            .rev_single(rev)
            .and_then(|o| o.peel_to_commit())
            .map_err(|_| err(format!("cannot find revision {rev} to ignore")))?;
        out.insert(c.id());
    }
    Ok(out)
}

impl Board<'_> {
    fn info(&mut self, id: Oid) -> Result<(i64, Vec<Oid>), GitError> {
        if id.is_zero() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            return Ok((now, self.worktree_parents.clone()));
        }
        if let Some(i) = self.commits.get(&id) {
            return Ok(i.clone());
        }
        let c = self.repo.find_commit(id)?;
        let info = (c.time().seconds(), c.parent_ids().collect::<Vec<_>>());
        self.commits.insert(id, info.clone());
        Ok(info)
    }

    /// The commits after `start` up to `end`, each named as its parents'
    /// child, for `--reverse`.
    fn children_between(&mut self, start: Oid, end: Oid) -> Result<(), GitError> {
        if self.first_parent {
            let mut c = end;
            while c != start {
                let (_, parents) = self.info(c)?;
                let Some(p) = parents.first() else {
                    return Err(err(
                        "--reverse --first-parent together require range along first-parent chain",
                    ));
                };
                self.children.insert(*p, vec![c]);
                c = *p;
            }
            return Ok(());
        }
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TIME)?;
        walk.push(end)?;
        walk.hide(start)?;
        for id in walk {
            let id = id?;
            for p in self.info(id)?.1 {
                self.children.entry(p).or_default().insert(0, id);
            }
        }
        Ok(())
    }

    fn blob_at(&self, commit: Oid, path: &str) -> Option<Oid> {
        let tree = self.repo.find_commit(commit).ok()?.tree().ok()?;
        let entry = tree.get_path(Path::new(path)).ok()?;
        (entry.kind() == Some(ObjectType::Blob)).then(|| entry.id())
    }

    fn is_link(&self, commit: Oid, path: &str) -> bool {
        self.repo
            .find_commit(commit)
            .and_then(|c| c.tree())
            .and_then(|t| t.get_path(Path::new(path)))
            .is_ok_and(|e| e.filemode() == 0o120000)
    }

    /// The origin for `path` in `commit`, made on first use and moved to the
    /// front of the commit's list.
    fn origin(&mut self, commit: Oid, path: &str, blob: Oid) -> usize {
        let list = self.by_commit.entry(commit).or_default();
        if let Some(i) = list.iter().position(|&o| self.origins[o].path == path) {
            let o = list.remove(i);
            list.insert(0, o);
            return o;
        }
        let o = self.origins.len();
        list.insert(0, o);
        self.origins.push(Origin {
            commit,
            path: path.to_owned(),
            blob,
            text: None,
            prints: None,
            suspects: Vec::new(),
            previous: None,
        });
        o
    }

    fn text(&mut self, o: usize) -> Result<Rc<Text>, GitError> {
        if let Some(t) = &self.origins[o].text {
            return Ok(t.clone());
        }
        self.stats[0] += 1;
        let t = if self.origins[o].commit.is_zero() {
            self.final_text.clone()
        } else {
            Rc::new(Text::new(
                self.repo
                    .find_blob(self.origins[o].blob)?
                    .content()
                    .to_vec(),
            ))
        };
        self.origins[o].text = Some(t.clone());
        Ok(t)
    }

    fn prints(&mut self, o: usize) -> Result<Rc<Vec<Print>>, GitError> {
        if let Some(p) = &self.origins[o].prints {
            return Ok(p.clone());
        }
        let t = self.text(o)?;
        let p = Rc::new((0..t.len()).map(|i| print(t.span(i, i + 1))).collect());
        self.origins[o].prints = Some(Rc::clone(&p));
        Ok(p)
    }

    fn drop_text(&mut self, o: usize) {
        self.origins[o].text = None;
        self.origins[o].prints = None;
    }

    fn push(&mut self, commit: Oid) -> Result<(), GitError> {
        let time = self.info(commit)?.0;
        self.seq += 1;
        let key = if self.reverse { -time } else { time };
        self.queue.push((key, Reverse(self.seq), commit));
        Ok(())
    }

    fn queue_blames(&mut self, o: usize, sorted: Vec<Entry>) {
        if sorted.is_empty() {
            return;
        }
        if !self.origins[o].suspects.is_empty() {
            let cur = std::mem::take(&mut self.origins[o].suspects);
            self.origins[o].suspects = merge(cur, sorted);
            return;
        }
        let commit = self.origins[o].commit;
        let busy = self.by_commit[&commit]
            .iter()
            .any(|&x| !self.origins[x].suspects.is_empty());
        self.origins[o].suspects = sorted;
        if !busy {
            // The commit is known, so its date is too.
            let _ = self.push(commit);
        }
    }

    fn assign(&mut self) -> Result<(), GitError> {
        let mut cur = self.queue.pop().map(|q| q.2);
        while let Some(commit) = cur {
            let suspect = self.by_commit[&commit]
                .iter()
                .copied()
                .find(|&o| !self.origins[o].suspects.is_empty());
            let Some(o) = suspect else {
                cur = self.queue.pop().map(|q| q.2);
                continue;
            };
            if self.reverse || !self.boundary.contains(&commit) {
                self.pass_blame(o)?;
            }
            if self.info(commit)?.1.is_empty() && !self.show_root {
                self.boundary.insert(commit);
            }
            let rest = std::mem::take(&mut self.origins[o].suspects);
            self.found.extend(rest.iter().map(|e| (e.lno + 1, e.num)));
            self.done.extend(rest);
        }
        Ok(())
    }

    fn scapegoats(&mut self, commit: Oid) -> Result<Vec<Oid>, GitError> {
        if self.reverse {
            return Ok(self.children.get(&commit).cloned().unwrap_or_default());
        }
        let mut parents = self.info(commit)?.1;
        if self.first_parent {
            parents.truncate(1);
        }
        Ok(parents)
    }

    /// `origin`'s path in `parent`, unrenamed.
    fn find_origin(&mut self, parent: Oid, o: usize) -> Option<usize> {
        let path = self.origins[o].path.clone();
        if let Some(&p) = self
            .by_commit
            .get(&parent)
            .and_then(|l| l.iter().find(|&&p| self.origins[p].path == path))
        {
            return Some(p);
        }
        let blob = self.blob_at(parent, &path)?;
        let commit = self.origins[o].commit;
        let link = |c: Oid| !c.is_zero() && self.is_link(c, &path);
        if link(parent) != link(commit) {
            return None;
        }
        Some(self.origin(parent, &path, blob))
    }

    /// The tree diff from `parent` to `commit` (the index for the working tree).
    fn tree_diff(
        &self,
        parent: Oid,
        commit: Oid,
        opts: &mut DiffOptions,
    ) -> Result<git2::Diff<'_>, GitError> {
        let old = self.repo.find_commit(parent)?.tree()?;
        Ok(if commit.is_zero() {
            self.repo.diff_tree_to_index(Some(&old), None, Some(opts))?
        } else {
            let new = self.repo.find_commit(commit)?.tree()?;
            self.repo
                .diff_tree_to_tree(Some(&old), Some(&new), Some(opts))?
        })
    }

    /// The path `origin` was renamed from in `parent`.
    fn find_rename(&mut self, parent: Oid, o: usize) -> Result<Option<usize>, GitError> {
        let (commit, path) = (self.origins[o].commit, self.origins[o].path.clone());
        let mut diff = self.tree_diff(parent, commit, &mut DiffOptions::new())?;
        diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
        let found = diff.deltas().find_map(|d| {
            let renamed = matches!(d.status(), git2::Delta::Renamed | git2::Delta::Copied);
            let to = d.new_file().path()?.to_str()?;
            if !renamed || to != path {
                return None;
            }
            let from = d.old_file().path()?.to_str()?.to_owned();
            Some((from, d.old_file().id()))
        });
        drop(diff);
        Ok(found.map(|(from, blob)| self.origin(parent, &from, blob)))
    }

    fn pass_whole_blame(&mut self, o: usize, p: usize) {
        if self.origins[p].text.is_none() {
            self.origins[p].text = self.origins[o].text.clone();
        }
        let mut suspects = std::mem::take(&mut self.origins[o].suspects);
        for e in &mut suspects {
            e.suspect = p;
        }
        self.queue_blames(p, suspects);
    }

    fn pass_blame(&mut self, o: usize) -> Result<(), GitError> {
        let commit = self.origins[o].commit;
        let goats = self.scapegoats(commit)?;
        let mut sg: Vec<Option<usize>> = vec![None; goats.len()];
        let mut blames = Vec::new();
        let mut toosmall = Vec::new();
        'passes: {
            if goats.is_empty() {
                break 'passes;
            }
            for pass in 0..2 {
                for (i, &p) in goats.iter().enumerate() {
                    if sg[i].is_some() {
                        continue;
                    }
                    let found = if pass == 0 {
                        self.find_origin(p, o)
                    } else {
                        self.find_rename(p, o)?
                    };
                    let Some(po) = found else { continue };
                    if self.origins[po].blob == self.origins[o].blob {
                        self.pass_whole_blame(o, po);
                        break 'passes;
                    }
                    let blob = self.origins[po].blob;
                    if !sg[..i]
                        .iter()
                        .flatten()
                        .any(|&x| self.origins[x].blob == blob)
                    {
                        sg[i] = Some(po);
                    }
                }
            }
            self.stats[2] += 1;
            for &po in sg.iter().flatten() {
                if self.origins[o].previous.is_none() {
                    self.origins[o].previous = Some(po);
                }
                self.pass_to_parent(o, po, false)?;
                if self.origins[o].suspects.is_empty() {
                    break 'passes;
                }
            }
            if self.ignore.contains(&commit) {
                for &po in sg.iter().flatten() {
                    self.pass_to_parent(o, po, true)?;
                    self.drop_text(po);
                    if self.origins[o].suspects.is_empty() {
                        break 'passes;
                    }
                }
            }
            if let Some(score) = self.move_score {
                let mut suspects = std::mem::take(&mut self.origins[o].suspects);
                filter_small(&self.final_text, &mut toosmall, 0, &mut suspects, score);
                self.origins[o].suspects = suspects;
                if !self.origins[o].suspects.is_empty() {
                    for &po in sg.iter().flatten() {
                        self.find_move(&mut blames, &mut toosmall, o, po)?;
                        if self.origins[o].suspects.is_empty() {
                            break;
                        }
                    }
                }
            }
            if self.copies > 0 {
                let (copy, moves) = (self.copy_score, self.move_score.unwrap_or(20));
                let mut suspects = std::mem::take(&mut self.origins[o].suspects);
                if copy > moves {
                    filter_small(&self.final_text, &mut toosmall, 0, &mut suspects, copy);
                } else if copy < moves {
                    suspects = merge(suspects, std::mem::take(&mut toosmall));
                    filter_small(&self.final_text, &mut toosmall, 0, &mut suspects, copy);
                }
                self.origins[o].suspects = suspects;
                if self.origins[o].suspects.is_empty() {
                    break 'passes;
                }
                for (i, &p) in goats.iter().enumerate() {
                    self.find_copy(&mut blames, &mut toosmall, o, p, sg[i])?;
                    if self.origins[o].suspects.is_empty() {
                        break 'passes;
                    }
                }
            }
        }
        self.distribute(blames);
        if !toosmall.is_empty() {
            let rest = std::mem::take(&mut self.origins[o].suspects);
            toosmall.extend(rest);
            self.origins[o].suspects = toosmall;
        }
        for &po in sg.iter().flatten() {
            if self.origins[po].suspects.is_empty() {
                self.drop_text(po);
            }
        }
        self.drop_text(o);
        Ok(())
    }

    fn distribute(&mut self, mut blames: Vec<Entry>) {
        blames.sort_by_key(|e| (e.suspect, e.s_lno));
        let mut i = 0;
        while i < blames.len() {
            let s = blames[i].suspect;
            let n = blames[i..].iter().take_while(|e| e.suspect == s).count();
            self.queue_blames(s, blames[i..i + n].to_vec());
            i += n;
        }
    }

    fn diff(&self, old: &[u8], new: &[u8]) -> Result<Vec<[usize; 4]>, GitError> {
        let mut opts = DiffOptions::new();
        opts.context_lines(0)
            .interhunk_lines(0)
            .force_text(true)
            .indent_heuristic(true)
            .ignore_whitespace(self.whitespace);
        let patch = Patch::from_buffers(old, None, new, None, Some(&mut opts))?;
        let start = |start: u32, lines: u32| start as usize - usize::from(lines > 0);
        (0..patch.num_hunks())
            .map(|h| {
                let (h, _) = patch.hunk(h)?;
                Ok([
                    start(h.old_start(), h.old_lines()),
                    h.old_lines() as usize,
                    start(h.new_start(), h.new_lines()),
                    h.new_lines() as usize,
                ])
            })
            .collect()
    }

    fn pass_to_parent(&mut self, t: usize, p: usize, ignore: bool) -> Result<(), GitError> {
        if self.origins[t].suspects.is_empty() {
            return Ok(());
        }
        let (pt, tt) = (self.text(p)?, self.text(t)?);
        let prints = if ignore {
            Some((self.prints(p)?, self.prints(t)?))
        } else {
            None
        };
        self.stats[1] += 1;
        let hunks = self.diff(&pt.buf, &tt.buf)?;
        let mut src: VecDeque<Entry> = std::mem::take(&mut self.origins[t].suspects).into();
        let (mut kept, mut dst) = (Vec::new(), Vec::new());
        let mut chunk = |tlno: usize, offset: isize, same: usize, parent_len: usize| {
            let mut diffp = Vec::new();
            while let Some(mut e) = src.pop_front() {
                if e.s_lno >= tlno {
                    src.push_front(e);
                    break;
                }
                if e.s_lno + e.num > tlno {
                    diffp.push(e.cut(tlno - e.s_lno));
                }
                e.suspect = p;
                e.s_lno = (e.s_lno as isize + offset) as usize;
                dst.push(e);
            }
            for n in diffp.into_iter().rev() {
                src.push_front(n);
            }
            let guesses = match &prints {
                Some((pp, tp)) if same > tlno => guess(
                    pp,
                    tp,
                    (tlno as isize + offset) as usize,
                    parent_len,
                    tlno,
                    same - tlno,
                ),
                _ => Vec::new(),
            };
            let (mut samep, mut diffp, mut ignoredp) = (Vec::new(), Vec::new(), Vec::new());
            while let Some(mut e) = src.pop_front() {
                if e.s_lno >= same {
                    src.push_front(e);
                    break;
                }
                if e.s_lno + e.num > same {
                    samep.push(e.cut(same - e.s_lno));
                }
                if prints.is_some() && e.s_lno >= tlno {
                    ignore_entry(e, p, &mut diffp, &mut ignoredp, &guesses[e.s_lno - tlno..]);
                } else {
                    diffp.push(e);
                }
            }
            for n in samep.into_iter().rev() {
                src.push_front(n);
            }
            kept.extend(diffp);
            dst.extend(ignoredp);
        };
        let mut offset = 0isize;
        for [sa, ca, sb, cb] in hunks {
            chunk(sb, sa as isize - sb as isize, sb + cb, ca);
            offset = (sa + ca) as isize - (sb + cb) as isize;
        }
        chunk(usize::MAX, offset, usize::MAX, 0);
        self.origins[t].suspects = kept;
        self.queue_blames(p, dst);
        Ok(())
    }

    fn score(&self, e: &Entry) -> u32 {
        entry_score(&self.final_text, e)
    }

    /// Split `e` by the lines of `parent_text` it matches, the best run
    /// blamed on `parent`.
    fn copy_in_blob(
        &self,
        e: &Entry,
        parent: usize,
        parent_text: &Text,
    ) -> Result<Split, GitError> {
        let mut split: Split = [None; 3];
        let piece = self.final_text.span(e.lno, e.lno + e.num);
        let (mut tlno, mut plno) = (0, 0);
        for [sa, ca, sb, cb] in self.diff(&parent_text.buf, piece)? {
            self.handle_split(e, tlno, plno, sb, parent, &mut split);
            plno = sa + ca;
            tlno = sb + cb;
        }
        self.handle_split(e, tlno, plno, e.num, parent, &mut split);
        Ok(split)
    }

    fn handle_split(
        &self,
        e: &Entry,
        tlno: usize,
        plno: usize,
        same: usize,
        parent: usize,
        split: &mut Split,
    ) {
        if e.num <= tlno || tlno >= same {
            return;
        }
        let potential = split_overlap(e, tlno + e.s_lno, plno, same + e.s_lno, parent);
        self.copy_split_if_better(split, potential);
    }

    fn copy_split_if_better(&self, best: &mut Split, potential: Split) {
        let Some(p) = &potential[1] else { return };
        if let Some(b) = &best[1]
            && self.score(p) < self.score(b)
        {
            return;
        }
        *best = potential;
    }

    fn find_move(
        &mut self,
        blamed: &mut Vec<Entry>,
        toosmall: &mut Vec<Entry>,
        t: usize,
        p: usize,
    ) -> Result<(), GitError> {
        let mut unblamed = std::mem::take(&mut self.origins[t].suspects);
        if unblamed.is_empty() {
            return Ok(());
        }
        let text = self.text(p)?;
        let score = self.move_score.unwrap_or(20);
        let mut leftover = Vec::new();
        let mut at = 0;
        while !unblamed.is_empty() {
            let mut next = Vec::new();
            for e in unblamed {
                let split = self.copy_in_blob(&e, p, &text)?;
                match &split[1] {
                    Some(s) if score < self.score(s) => split_blame(blamed, &mut next, split),
                    _ => leftover.push(e),
                }
            }
            at = filter_small(&self.final_text, toosmall, at, &mut next, score);
            unblamed = next;
        }
        self.origins[t].suspects = leftover;
        Ok(())
    }

    fn find_copy(
        &mut self,
        blamed: &mut Vec<Entry>,
        toosmall: &mut Vec<Entry>,
        t: usize,
        parent: Oid,
        porigin: Option<usize>,
    ) -> Result<(), GitError> {
        let mut unblamed = std::mem::take(&mut self.origins[t].suspects);
        if unblamed.is_empty() {
            return Ok(());
        }
        let (commit, path) = (self.origins[t].commit, self.origins[t].path.clone());
        let skip = porigin.map(|p| self.origins[p].path.clone());
        let harder = self.copies >= COPY_HARDEST
            || self.copies >= COPY_HARDER && skip.as_deref() != Some(path.as_str());
        let mut files: Vec<(String, Oid)> = Vec::new();
        if harder {
            let tree = self.repo.find_commit(parent)?.tree()?;
            tree.walk(git2::TreeWalkMode::PreOrder, |dir, e| {
                if e.kind() == Some(ObjectType::Blob)
                    && let Ok(name) = e.name()
                {
                    files.push((format!("{dir}{name}"), e.id()));
                }
                git2::TreeWalkResult::Ok
            })?;
        } else {
            let diff = self.tree_diff(parent, commit, &mut DiffOptions::new())?;
            for d in diff.deltas() {
                let old = d.old_file();
                if old.exists()
                    && old.mode() != git2::FileMode::Commit
                    && let Some(p) = old.path().and_then(|p| p.to_str())
                {
                    files.push((p.to_owned(), old.id()));
                }
            }
        }
        files.retain(|(p, _)| skip.as_deref() != Some(p.as_str()));
        let mut leftover = Vec::new();
        let mut at = 0;
        while !unblamed.is_empty() {
            let mut splits: Vec<Split> = vec![[None; 3]; unblamed.len()];
            for (p, blob) in &files {
                let n = self.origin(parent, p, *blob);
                let text = self.text(n)?;
                for (j, e) in unblamed.iter().enumerate() {
                    let potential = self.copy_in_blob(e, n, &text)?;
                    self.copy_split_if_better(&mut splits[j], potential);
                }
                if self.origins[n].suspects.is_empty() {
                    self.drop_text(n);
                }
            }
            let mut next = Vec::new();
            for (e, split) in unblamed.into_iter().zip(splits) {
                match &split[1] {
                    Some(s) if self.copy_score < self.score(s) => {
                        split_blame(blamed, &mut next, split)
                    }
                    _ => leftover.push(e),
                }
            }
            at = filter_small(&self.final_text, toosmall, at, &mut next, self.copy_score);
            unblamed = next;
        }
        self.origins[t].suspects = leftover;
        Ok(())
    }

    fn lines(&mut self) -> Result<Vec<BlameLine>, GitError> {
        let mut done = std::mem::take(&mut self.done);
        done.sort_by_key(|e| e.lno);
        let mut people: HashMap<Oid, (String, String)> = HashMap::new();
        let mut out = Vec::new();
        for e in done {
            let o = &self.origins[e.suspect];
            let commit = o.commit;
            if let std::collections::hash_map::Entry::Vacant(slot) = people.entry(commit) {
                let who = if commit.is_zero() {
                    (
                        "Not Committed Yet".to_owned(),
                        "not.committed.yet".to_owned(),
                    )
                } else {
                    let c = self.repo.find_commit(commit)?;
                    let a = c.author();
                    (
                        String::from_utf8_lossy(a.name_bytes()).into_owned(),
                        String::from_utf8_lossy(a.email_bytes()).into_owned(),
                    )
                };
                slot.insert(who);
            }
            let (author, email) = &people[&commit];
            let id = if commit.is_zero() {
                String::new()
            } else {
                commit.to_string()
            };
            let previous = o.previous.map(|p| {
                let p = &self.origins[p];
                (p.commit.to_string(), p.path.clone())
            });
            for k in 0..e.num {
                let raw = self.final_text.span(e.lno + k, e.lno + k + 1);
                let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
                out.push(BlameLine {
                    short_id: id.chars().take(7).collect(),
                    author: author.clone(),
                    line: String::from_utf8_lossy(raw).into_owned(),
                    id: id.clone(),
                    email: email.clone(),
                    orig_line: e.s_lno + k + 1,
                    orig_path: o.path.clone(),
                    boundary: self.boundary.contains(&commit),
                    final_line: e.lno + k + 1,
                    ignored: e.ignored,
                    unblamable: e.unblamable,
                    previous: previous.clone(),
                });
            }
        }
        Ok(out)
    }
}

fn entry_score(text: &Text, e: &Entry) -> u32 {
    1 + text
        .span(e.lno, e.lno + e.num)
        .iter()
        .filter(|b| b.is_ascii_alphanumeric())
        .count() as u32
}

/// Two lists sorted by suspect line merged, the first's entries first on ties.
fn merge(a: Vec<Entry>, b: Vec<Entry>) -> Vec<Entry> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut a, mut b) = (a.into_iter().peekable(), b.into_iter().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (Some(x), Some(y)) if x.s_lno <= y.s_lno => out.extend(a.next()),
            (Some(_), Some(_)) => out.extend(b.next()),
            _ => break,
        }
    }
    out.extend(a);
    out.extend(b);
    out
}

/// Move the entries of `source` scoring at most `min` into `small` at `at`;
/// returns where the next ones go.
fn filter_small(
    text: &Text,
    small: &mut Vec<Entry>,
    at: usize,
    source: &mut Vec<Entry>,
    min: u32,
) -> usize {
    let (low, high): (Vec<Entry>, Vec<Entry>) = std::mem::take(source)
        .into_iter()
        .partition(|e| entry_score(text, e) <= min);
    *source = high;
    let n = low.len();
    small.splice(at..at, low);
    at + n
}

/// `e` split around lines `tlno..same` of its suspect, which match the
/// parent's lines from `plno`: before, the part blamed on `parent`, after.
fn split_overlap(e: &Entry, tlno: usize, plno: usize, same: usize, parent: usize) -> Split {
    let part = |lno, s_lno, num, suspect| Entry {
        lno,
        s_lno,
        num,
        suspect,
        ..*e
    };
    let mut split: Split = [None; 3];
    let (lno, s_lno) = if e.s_lno < tlno {
        split[0] = Some(part(e.lno, e.s_lno, tlno - e.s_lno, e.suspect));
        (e.lno + tlno - e.s_lno, plno)
    } else {
        (e.lno, plno + (e.s_lno - tlno))
    };
    let end = if same < e.s_lno + e.num {
        let lno = e.lno + (same - e.s_lno);
        split[2] = Some(part(lno, same, e.s_lno + e.num - same, e.suspect));
        lno
    } else {
        e.lno + e.num
    };
    if end <= lno {
        return [None; 3];
    }
    split[1] = Some(part(lno, s_lno, end - lno, parent));
    split
}

fn split_blame(blamed: &mut Vec<Entry>, unblamed: &mut Vec<Entry>, split: Split) {
    let [before, middle, after] = split;
    unblamed.extend(before);
    unblamed.extend(after);
    blamed.extend(middle);
}

/// Carve `e` into runs its guesses give to the parent (ignored) or leave
/// with the target (unblamable).
fn ignore_entry(
    mut e: Entry,
    parent: usize,
    diffp: &mut Vec<Entry>,
    ignoredp: &mut Vec<Entry>,
    guesses: &[(bool, usize)],
) {
    let n = e.num;
    let mut len = 1;
    for i in 0..n {
        let end =
            i + 1 == n || guesses[i].0 != guesses[i + 1].0 || guesses[i].1 + 1 != guesses[i + 1].1;
        if !end {
            len += 1;
            continue;
        }
        let next = (len < e.num).then(|| {
            let (ignored, unblamable) = (e.ignored, e.unblamable);
            let mut rest = e.cut(len);
            rest.ignored = ignored;
            rest.unblamable = unblamable;
            rest
        });
        if guesses[i].0 {
            e.ignored = true;
            e.suspect = parent;
            e.s_lno = guesses[i + 1 - len].1;
            ignoredp.push(e);
        } else {
            e.unblamable = true;
            diffp.push(e);
        }
        if let Some(rest) = next {
            e = rest;
        }
        len = 1;
    }
}

fn print(line: &[u8]) -> Print {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let mut out = Print::new();
    for w in line.windows(2) {
        let key = u16::from_le_bytes([w[0].to_ascii_lowercase(), w[1].to_ascii_lowercase()]);
        *out.entry(key).or_default() += 1;
    }
    out
}

fn similarity(a: &Print, b: &Print) -> u32 {
    a.iter()
        .map(|(k, n)| u32::from(*n.min(b.get(k).unwrap_or(&0))))
        .sum()
}

/// For each target line `b0..b0+lb` an ignored commit changed, the parent
/// line in `a0..a0+la` it most likely came from, keeping their order:
/// `(true, parent line)`, or `(false, its own line)` when none is alike.
fn guess(
    parent: &[Print],
    target: &[Print],
    a0: usize,
    la: usize,
    b0: usize,
    lb: usize,
) -> Vec<(bool, usize)> {
    let mut found: Vec<Option<usize>> = vec![None; lb];
    if la > 0 {
        let reach = 10.min(la - 1);
        let closest = |b: usize| (2 * b + 1) * la / (2 * lb);
        let mut stack = vec![(0, la, 0, lb)];
        while let Some((alo, ahi, blo, bhi)) = stack.pop() {
            if alo >= ahi || blo >= bhi {
                continue;
            }
            let mut best: Option<(u32, usize, usize)> = None;
            for b in blo..bhi {
                let c = closest(b).clamp(alo, ahi - 1);
                let (lo, hi) = (c.saturating_sub(reach).max(alo), (c + reach + 1).min(ahi));
                let (mut top, mut second, mut at) = (0, 0, lo);
                for a in lo..hi {
                    let s = similarity(&parent[a0 + a], &target[b0 + b]);
                    if s > top {
                        second = top;
                        top = s;
                        at = a;
                    } else if s > second {
                        second = s;
                    }
                }
                if top > 0 && best.is_none_or(|(c, _, _)| top - second > c) {
                    best = Some((top - second, b, at));
                }
            }
            if let Some((_, b, a)) = best {
                found[b] = Some(a);
                stack.push((a + 1, ahi, b + 1, bhi));
                stack.push((alo, a, blo, b));
            }
        }
    }
    found
        .into_iter()
        .enumerate()
        .map(|(i, a)| match a {
            Some(a) => (true, a0 + a),
            None => (false, b0 + i),
        })
        .collect()
}

/// git's `-L` ranges over `text`, 0-based and half-open, sorted and merged;
/// the whole file without any.
fn ranges(
    repo: &Repository,
    path: &str,
    text: &Text,
    specs: &[String],
) -> Result<Vec<(usize, usize)>, GitError> {
    let lines = text.len() as i64;
    if specs.is_empty() {
        return Ok(vec![(0, text.len())]);
    }
    let mut out = Vec::new();
    let mut anchor = 1;
    for spec in specs {
        let (bottom, top) = if spec.starts_with(':') || spec.starts_with("^:") {
            funcname(repo, path, text, spec, anchor)?
        } else {
            let (mut begin, mut end) = (0, 0);
            let mut rest = loc(spec, text, -anchor, &mut begin)?;
            if let Some(r) = rest.strip_prefix(',') {
                rest = loc(r, text, begin + 1, &mut end)?;
            }
            if !rest.is_empty() {
                return Err(err(format!("invalid -L range '{spec}'")));
            }
            if begin != 0 && end != 0 && end < begin {
                (end, begin)
            } else {
                (begin, end)
            }
        };
        if lines == 0 && (top != 0 || bottom != 0) || lines < bottom {
            let s = if lines == 1 { "" } else { "s" };
            return Err(err(format!("file {path} has only {lines} line{s}")));
        }
        let bottom = bottom.max(1);
        let top = if top < 1 || lines < top { lines } else { top };
        out.push((bottom as usize - 1, top as usize));
        anchor = top + 1;
    }
    out.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in out {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged.retain(|(a, b)| a < b);
    Ok(merged)
}

/// A `-L` regex as git compiles it: basic, `^` and `$` at each line.
fn bre(pattern: &str) -> Result<crate::userdiff::Regex, String> {
    crate::userdiff::Regex::new(pattern.as_bytes(), crate::userdiff::NEWLINE)
}

/// One end of a `-L` range; `begin` is where a regex search starts (negative:
/// the previous range's end, which `^` resets to the top) and where `+N`/`-N`
/// count from.
fn loc<'s>(spec: &'s str, text: &Text, mut begin: i64, ret: &mut i64) -> Result<&'s str, GitError> {
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    if begin >= 1 && (spec.starts_with('+') || spec.starts_with('-')) {
        let n = digits(&spec[1..]);
        if n == 0 {
            return Ok(spec);
        }
        let num: i64 = spec[1..=n].parse().unwrap_or(0);
        if num == 0 {
            return Err(err("-L invalid empty range"));
        }
        *ret = if spec.starts_with('+') {
            begin + num - 2
        } else {
            (begin - num).max(1)
        };
        return Ok(&spec[n + 1..]);
    }
    let sign = usize::from(spec.starts_with('-'));
    let n = digits(&spec[sign..]);
    if n > 0 {
        let num: i64 = spec[..sign + n].parse().unwrap_or(0);
        if num <= 0 {
            return Err(err(format!("-L invalid line number: {num}")));
        }
        *ret = num;
        return Ok(&spec[sign + n..]);
    }
    let mut spec = spec;
    if begin < 0 {
        match spec.strip_prefix('^') {
            Some(s) => {
                begin = 1;
                spec = s;
            }
            None => begin = -begin,
        }
    }
    let Some(body) = spec.strip_prefix('/') else {
        return Ok(spec);
    };
    let bytes = body.as_bytes();
    let mut end = 0;
    while end < bytes.len() && bytes[end] != b'/' {
        end += if bytes[end] == b'\\' { 2 } else { 1 };
    }
    if end >= bytes.len() {
        return Ok(spec);
    }
    let pattern = &body[..end];
    let from = text.starts[((begin - 1) as usize).min(text.len())];
    let re = bre(pattern);
    let found = re
        .as_ref()
        .ok()
        .and_then(|re| re.exec(&text.buf[from..])?[0]);
    let Some((so, _)) = found else {
        let why = re.map_or_else(|e| e, |re| re.no_match());
        return Err(err(format!(
            "-L parameter '{pattern}' starting at line {begin}: {why}"
        )));
    };
    let at = from + so;
    *ret = text.starts.partition_point(|&s| s <= at) as i64;
    Ok(&body[end + 1..])
}

/// `-L :funcname`: from the first function line matching the regex to the
/// line before the next function line.
fn funcname(
    repo: &Repository,
    path: &str,
    text: &Text,
    spec: &str,
    mut anchor: i64,
) -> Result<(i64, i64), GitError> {
    let mut spec = spec;
    if let Some(s) = spec.strip_prefix('^') {
        anchor = 1;
        spec = s;
    }
    let pattern = &spec[1..];
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b':' {
        i += if bytes[i] == b'\\' && i + 1 < bytes.len() {
            2
        } else {
            1
        };
    }
    if i == 0 || i < bytes.len() {
        return Err(err(format!("invalid -L range '{spec}'")));
    }
    let driver = crate::userdiff::driver(repo, path, crate::userdiff::Fallback::None)?
        .and_then(|d| d.funcname);
    let is_func = |line: &[u8]| crate::userdiff::is_func(driver.as_ref(), line);
    let re = bre(pattern).map_err(|e| err(format!("-L parameter '{pattern}': {e}")))?;
    let lines = text.len();
    let mut line = ((anchor - 1) as usize).min(lines);
    let begin = loop {
        let from = text.starts[line];
        let Some((so, _)) = re.exec(&text.buf[from..]).and_then(|m| m[0]) else {
            return Err(err(format!(
                "-L parameter '{pattern}' starting at line {anchor}: no match"
            )));
        };
        let at = text.starts.partition_point(|&s| s <= from + so) - 1;
        if is_func(text.span(at, at + 1)) {
            break at;
        }
        line = at + 1;
        if line >= lines {
            return Err(err(format!(
                "-L parameter '{pattern}' starting at line {anchor}: no match"
            )));
        }
    };
    let end = (begin + 1..lines)
        .find(|&l| is_func(text.span(l, l + 1)))
        .unwrap_or(lines);
    Ok((begin as i64 + 1, end as i64))
}
