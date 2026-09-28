//! A port of git's merge-ort: the three-way merge of trees behind `git
//! merge-tree --write-tree`, with rename and directory rename detection and
//! git's conflict messages, word for word.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use git2::{Commit, Oid, Repository, Tree};

use crate::GitError;
use crate::lowlevel::{MergeFileOpts, merge_file};

const MAX_SCORE: u64 = 60000;
const FMT: u32 = 0o170000;
const DIR: u32 = 0o040000;
const REG: u32 = 0o100000;
const LNK: u32 = 0o120000;
const GITLINK: u32 = 0o160000;
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

// dirs_removed relevance, as in diffcore.h.
const NOT_RELEVANT: u8 = 0;
const FOR_ANCESTOR: u8 = 1;
const FOR_SELF: u8 = 2;
// relevant_sources.
const CONTENT: u8 = 1;
const LOCATION: u8 = 2;

/// merge.directoryRenames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirRenames {
    None,
    #[default]
    Conflict,
    True,
}

/// How to merge: the branch labels and git's merge options.
#[derive(Debug, Clone, Default)]
pub struct OrtOpts {
    pub branch1: String,
    pub branch2: String,
    /// The merge base's label; git's own when unset.
    pub ancestor: Option<String>,
    /// `ours` or `theirs` (-X).
    pub favor: Option<String>,
    pub no_renames: bool,
    /// The rename threshold out of 60000; 0 is git's 50%.
    pub rename_score: u64,
    pub dir_renames: DirRenames,
    /// Style, whitespace and diff options for content merges.
    pub file: MergeFileOpts,
}

/// One of git's messages about a path.
#[derive(Debug, Clone)]
pub struct Msg {
    /// git's short type, as printed by `merge-tree -z`.
    pub kind: &'static str,
    pub paths: Vec<String>,
    pub text: String,
}

/// A conflicted index entry.
#[derive(Debug, Clone)]
pub struct Stage {
    pub path: String,
    pub stage: u8,
    pub mode: u32,
    pub id: Oid,
}

/// The merge result: the tree (conflicts written with markers), whether it
/// is clean, the conflicted stages and the messages by path.
#[derive(Debug, Clone)]
pub struct Merged {
    pub tree: Oid,
    pub clean: bool,
    pub conflicted: Vec<Stage>,
    pub messages: BTreeMap<String, Vec<Msg>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Ver {
    mode: u32,
    id: Oid,
}

fn null() -> Ver {
    Ver {
        mode: 0,
        id: Oid::ZERO_SHA1,
    }
}

fn is_reg(mode: u32) -> bool {
    mode & FMT == REG
}

#[derive(Clone, Debug)]
struct Info {
    clean: bool,
    is_null: bool,
    result: Ver,
    stages: [Ver; 3],
    pathnames: [String; 3],
    filemask: u8,
    dirmask: u8,
    match_mask: u8,
    df_conflict: bool,
    path_conflict: bool,
}

impl Info {
    fn resolved(v: Ver) -> Info {
        Info {
            clean: true,
            is_null: false,
            result: v,
            stages: [null(); 3],
            pathnames: Default::default(),
            filemask: 0,
            dirmask: 0,
            match_mask: 0,
            df_conflict: false,
            path_conflict: false,
        }
    }
}

#[derive(Clone, Debug)]
struct Pair {
    one: String,
    one_ver: Option<Ver>,
    two: String,
    two_ver: Option<Ver>,
    status: u8,
    side: usize,
}

#[derive(Default)]
struct Collision {
    sources: BTreeSet<String>,
    reported: bool,
}

type Collisions = BTreeMap<String, Collision>;

struct Ort<'a> {
    repo: &'a Repository,
    o: &'a OrtOpts,
    branch1: String,
    branch2: String,
    ancestor: String,
    depth: usize,
    paths: BTreeMap<String, Info>,
    conflicted: BTreeMap<String, Info>,
    msgs: BTreeMap<String, Vec<Msg>>,
    pairs: [Vec<Pair>; 3],
    relevant: [HashMap<String, u8>; 3],
    dirs_removed: [HashMap<String, u8>; 3],
    counts: [BTreeMap<String, BTreeMap<String, u32>>; 3],
    dir_renames: [BTreeMap<String, String>; 3],
    spans: HashMap<Oid, (u64, HashMap<u32, u64>)>,
    results: BTreeMap<String, Ver>,
}

fn parent(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn basename(path: &str) -> &str {
    parent(path).1
}

impl<'a> Ort<'a> {
    fn new(repo: &'a Repository, o: &'a OrtOpts, b1: &str, b2: &str, depth: usize) -> Self {
        Ort {
            repo,
            o,
            branch1: b1.to_owned(),
            branch2: b2.to_owned(),
            ancestor: String::new(),
            depth,
            paths: BTreeMap::new(),
            conflicted: BTreeMap::new(),
            msgs: BTreeMap::new(),
            pairs: Default::default(),
            relevant: Default::default(),
            dirs_removed: Default::default(),
            counts: Default::default(),
            dir_renames: Default::default(),
            spans: HashMap::new(),
            results: BTreeMap::new(),
        }
    }

    fn msg(&mut self, kind: &'static str, primary: &str, others: &[&str], text: String) {
        if self.depth > 0 {
            return;
        }
        let mut paths = vec![primary.to_owned()];
        paths.extend(others.iter().map(|p| (*p).to_owned()));
        self.msgs
            .entry(primary.to_owned())
            .or_default()
            .push(Msg { kind, paths, text });
    }

    fn branch(&self, side: usize) -> String {
        if side == 1 {
            self.branch1.clone()
        } else {
            self.branch2.clone()
        }
    }

    // collect_merge_info
    fn collect(
        &mut self,
        dir: &str,
        trees: [Option<Tree>; 3],
        mut drm: u8,
    ) -> Result<(), GitError> {
        let mut by_name: BTreeMap<String, [Option<Ver>; 3]> = BTreeMap::new();
        for (i, t) in trees.iter().enumerate() {
            let Some(t) = t else { continue };
            for e in t.iter() {
                let name = String::from_utf8_lossy(e.name_bytes()).into_owned();
                by_name.entry(name).or_default()[i] = Some(Ver {
                    mode: e.filemode() as u32,
                    id: e.id(),
                });
            }
        }
        let mut entries: Vec<(String, [Option<Ver>; 3])> = by_name.into_iter().collect();
        entries.sort_by_cached_key(|(n, v)| {
            let mut k = n.clone().into_bytes();
            if v.iter().flatten().any(|v| v.mode == DIR) {
                k.push(b'/');
            }
            k
        });
        if (drm == 2 || drm == 4) && entries.iter().any(|(_, v)| masks(v).2 == drm) {
            drm = 7;
        }
        for (name, n) in entries {
            self.entry(dir, &name, n, drm)?;
        }
        Ok(())
    }

    fn entry(
        &mut self,
        dir: &str,
        name: &str,
        n: [Option<Ver>; 3],
        drm: u8,
    ) -> Result<(), GitError> {
        let (_, dirmask, filemask) = masks(&n);
        let eq = |a: usize, b: usize| n[a].is_some() && n[a] == n[b];
        let (s1m, s2m, sm) = (eq(1, 0), eq(2, 0), eq(1, 2));
        let match_mask = if s1m {
            if s2m { 7 } else { 3 }
        } else if s2m {
            5
        } else if sm {
            6
        } else {
            0
        };
        let full = join(dir, name);
        let pick = |i: usize| n[i].unwrap_or_else(null);
        if s1m && s2m {
            self.paths.insert(full, Info::resolved(pick(0)));
            return Ok(());
        }
        if filemask == 7 && (sm || s1m || s2m) {
            let side = if !sm && s1m { 2 } else { 1 };
            self.paths.insert(full, Info::resolved(pick(side)));
            return Ok(());
        }
        let sub = self.rename_info(&n, dir, &full, filemask, dirmask, match_mask, drm);
        let mut ci = Info {
            clean: false,
            is_null: dirmask != 0,
            result: null(),
            stages: [pick(0), pick(1), pick(2)],
            pathnames: [full.clone(), full.clone(), full.clone()],
            filemask,
            dirmask,
            match_mask,
            df_conflict: filemask != 0 && dirmask != 0,
            path_conflict: false,
        };
        if dirmask != 0 {
            ci.match_mask &= filemask;
        }
        self.paths.insert(full.clone(), ci);
        if dirmask != 0 {
            let mut trees: [Option<Tree>; 3] = [None, None, None];
            for (i, t) in trees.iter_mut().enumerate() {
                if dirmask & (1 << i) != 0 {
                    *t = Some(self.repo.find_tree(pick(i).id)?);
                }
            }
            self.collect(&full, trees, sub)?;
        }
        Ok(())
    }

    // collect_rename_info and add_pair; returns the dir_rename_mask to use
    // below this path.
    #[allow(clippy::too_many_arguments)]
    fn rename_info(
        &mut self,
        n: &[Option<Ver>; 3],
        dirname: &str,
        full: &str,
        filemask: u8,
        dirmask: u8,
        match_mask: u8,
        mut drm: u8,
    ) -> u8 {
        if drm != 7 && (dirmask == 3 || dirmask == 5) {
            drm = dirmask & !1;
        }
        if matches!(dirmask, 1 | 3 | 5) {
            let sides = (7 - dirmask) / 2;
            let rel = if drm == 7 { FOR_ANCESTOR } else { NOT_RELEVANT };
            for side in 1..=2 {
                if sides & side as u8 != 0 {
                    self.dirs_removed[side].insert(full.to_owned(), rel);
                }
            }
        }
        if drm == 7 && (filemask == 2 || filemask == 4) {
            let side = 3 - (filemask >> 1) as usize;
            self.dirs_removed[side].insert(dirname.to_owned(), FOR_SELF);
        }
        if filemask == 0 || filemask == 7 {
            return drm;
        }
        for side in 1..=2usize {
            let bit = 1u8 << side;
            if filemask & 1 != 0 && filemask & bit == 0 {
                let content = match_mask & filemask == 0;
                if content || drm == 7 {
                    let r = if content { CONTENT } else { LOCATION };
                    self.relevant[side].insert(full.to_owned(), r);
                }
                self.pairs[side].push(Pair {
                    one: full.to_owned(),
                    one_ver: n[0],
                    two: full.to_owned(),
                    two_ver: None,
                    status: 0,
                    side,
                });
            }
            if filemask & 1 == 0 && filemask & bit != 0 {
                self.pairs[side].push(Pair {
                    one: full.to_owned(),
                    one_ver: None,
                    two: full.to_owned(),
                    two_ver: n[side],
                    status: 0,
                    side,
                });
            }
        }
        drm
    }

    fn span(&mut self, id: Oid) -> Result<(u64, HashMap<u32, u64>), GitError> {
        if let Some(s) = self.spans.get(&id) {
            return Ok(s.clone());
        }
        let blob = self.repo.find_blob(id)?;
        let s = (
            blob.content().len() as u64,
            crate::git_repo::span_hashes(blob.content()),
        );
        self.spans.insert(id, s.clone());
        Ok(s)
    }

    fn size(&mut self, id: Oid) -> Result<u64, GitError> {
        if let Some(s) = self.spans.get(&id) {
            return Ok(s.0);
        }
        Ok(self.repo.find_blob(id)?.content().len() as u64)
    }

    // diffcore-rename's estimate_similarity
    fn similarity(&mut self, src: Ver, dst: Ver, min: u64) -> Result<u64, GitError> {
        if !is_reg(src.mode) || !is_reg(dst.mode) {
            return Ok(0);
        }
        let (a, b) = (self.size(src.id)?, self.size(dst.id)?);
        let (max, base) = (a.max(b), a.min(b));
        if max * (MAX_SCORE - min) < (max - base) * MAX_SCORE {
            return Ok(0);
        }
        let (_, sa) = self.span(src.id)?;
        let (bsize, sb) = self.span(dst.id)?;
        if bsize == 0 {
            return Ok(0);
        }
        let copied: u64 = sa
            .iter()
            .map(|(h, n)| sb.get(h).map_or(0, |m| (*n).min(*m)))
            .sum();
        Ok(copied * MAX_SCORE / max)
    }

    fn count_rename(&mut self, side: usize, old: &str, new: &str) {
        let (mut old_dir, mut new_dir) = (old, new);
        let mut first = true;
        loop {
            let (od, osub) = parent(old_dir);
            if !self.dirs_removed[side].contains_key(od) {
                break;
            }
            let (nd, nsub) = parent(new_dir);
            if !first && osub != nsub {
                break;
            }
            let flag = self.dirs_removed[side]
                .get(od)
                .copied()
                .unwrap_or(NOT_RELEVANT);
            if flag == FOR_SELF || first {
                *self.counts[side]
                    .entry(od.to_owned())
                    .or_default()
                    .entry(nd.to_owned())
                    .or_default() += 1;
            }
            first = false;
            (old_dir, new_dir) = (od, nd);
            if flag == NOT_RELEVANT || od.is_empty() || nd.is_empty() {
                break;
            }
        }
    }

    // diffcore_rename_extended for one side's pairs.
    fn detect(&mut self, side: usize) -> Result<(), GitError> {
        let mut pairs = std::mem::take(&mut self.pairs[side]);
        let empty = Oid::from_str(EMPTY_BLOB)?;
        let srcs: Vec<usize> = (0..pairs.len())
            .filter(|&i| pairs[i].one_ver.is_some_and(|v| v.id != empty))
            .collect();
        let dsts: Vec<usize> = (0..pairs.len())
            .filter(|&i| pairs[i].two_ver.is_some_and(|v| v.id != empty))
            .collect();
        let run = !pairs.is_empty() && !self.relevant[side].is_empty();
        let mut used = vec![false; srcs.len()];
        let mut renamed: Vec<Option<usize>> = vec![None; dsts.len()];
        if run && !srcs.is_empty() && !dsts.is_empty() {
            self.find_renames(side, &pairs, &srcs, &dsts, &mut used, &mut renamed)?;
        }
        let gone: BTreeSet<usize> = srcs
            .iter()
            .zip(&used)
            .filter(|(_, u)| **u)
            .map(|(s, _)| *s)
            .collect();
        let by_dst: HashMap<usize, usize> = dsts
            .iter()
            .zip(&renamed)
            .filter_map(|(d, s)| s.map(|s| (*d, srcs[s])))
            .collect();
        let mut out = Vec::new();
        for (i, p) in pairs.iter().enumerate() {
            if gone.contains(&i) {
                continue;
            }
            let mut p = p.clone();
            if let Some(&s) = by_dst.get(&i) {
                p.one = pairs[s].one.clone();
                p.one_ver = pairs[s].one_ver;
                p.status = b'R';
            } else {
                p.status = if p.one_ver.is_none() { b'A' } else { b'D' };
            }
            out.push(p);
        }
        pairs.clear();
        self.pairs[side] = out;
        let removed = &self.dirs_removed[side];
        self.counts[side].retain(|dir, _| removed.get(dir).is_some_and(|r| *r != NOT_RELEVANT));
        Ok(())
    }

    #[allow(clippy::needless_range_loop)]
    fn find_renames(
        &mut self,
        side: usize,
        pairs: &[Pair],
        srcs: &[usize],
        dsts: &[usize],
        used: &mut [bool],
        renamed: &mut [Option<usize>],
    ) -> Result<(), GitError> {
        let min = if self.o.rename_score == 0 {
            30000
        } else {
            self.o.rename_score
        };
        let src = |s: usize| (pairs[srcs[s]].one.clone(), pairs[srcs[s]].one_ver.unwrap());
        let dst = |d: usize| (pairs[dsts[d]].two.clone(), pairs[dsts[d]].two_ver.unwrap());
        // Exact renames.
        for d in 0..dsts.len() {
            let (dpath, dv) = dst(d);
            let mut best: Option<(usize, u8)> = None;
            for s in 0..srcs.len() {
                let (spath, sv) = src(s);
                if sv.id != dv.id || used[s] {
                    continue;
                }
                if (!is_reg(sv.mode) || !is_reg(dv.mode)) && sv.mode != dv.mode {
                    continue;
                }
                let score = 1 + u8::from(basename(&spath) == basename(&dpath));
                if best.is_none_or(|(_, b)| score > b) {
                    best = Some((s, score));
                    if score == 2 {
                        break;
                    }
                }
            }
            if let Some((s, _)) = best {
                used[s] = true;
                renamed[d] = Some(s);
            }
        }
        if min == MAX_SCORE {
            return Ok(());
        }
        let mut idx_map: HashMap<String, usize> = HashMap::new();
        for d in 0..dsts.len() {
            match renamed[d] {
                None => {
                    idx_map.insert(dst(d).0, d);
                }
                Some(s) => self.count_rename(side, &src(s).0, &dst(d).0),
            }
        }
        let guess: HashMap<String, String> = self.counts[side]
            .iter()
            .filter_map(|(old, c)| {
                let mut best: Option<(&String, u32)> = None;
                for (new, n) in c {
                    if best.is_none_or(|(_, b)| *n > b) {
                        best = Some((new, *n));
                    }
                }
                best.map(|(new, _)| (old.clone(), new.clone()))
            })
            .collect();
        // Basename-guided matches.
        let min_base = min + (MAX_SCORE - min) / 2;
        let mut sources: HashMap<&str, Option<usize>> = HashMap::new();
        let src_paths: Vec<String> = (0..srcs.len()).map(|s| src(s).0).collect();
        for (s, path) in src_paths.iter().enumerate() {
            if used[s] {
                continue;
            }
            let b = basename(path);
            let e = sources.entry(b).or_insert(Some(s));
            if *e != Some(s) {
                *e = None;
            }
        }
        let dst_paths: Vec<String> = (0..dsts.len()).map(|d| dst(d).0).collect();
        let mut dests: HashMap<&str, Option<usize>> = HashMap::new();
        for (d, path) in dst_paths.iter().enumerate() {
            if renamed[d].is_some() {
                continue;
            }
            let b = basename(path);
            let e = dests.entry(b).or_insert(Some(d));
            if *e != Some(d) {
                *e = None;
            }
        }
        for s in 0..srcs.len() {
            let (spath, sv) = src(s);
            if used[s] || !self.relevant[side].contains_key(&spath) {
                continue;
            }
            let b = basename(&spath);
            let Some(&dst_index) = dests.get(b) else {
                continue;
            };
            let d = match (sources.get(b).copied().flatten(), dst_index) {
                (Some(_), Some(d)) => Some(d),
                _ => guess
                    .get(parent(&spath).0)
                    .and_then(|dir| idx_map.get(&format!("{dir}/{b}")).copied()),
            };
            let Some(d) = d else { continue };
            if renamed[d].is_some() {
                continue;
            }
            let (dpath, dv) = dst(d);
            if self.similarity(sv, dv, min_base)? < min_base {
                continue;
            }
            used[s] = true;
            renamed[d] = Some(s);
            self.count_rename(side, &spath, &dpath);
        }
        // Inexact renames among the relevant sources left.
        let left: Vec<usize> = (0..srcs.len())
            .filter(|&s| !used[s] && self.relevant[side].contains_key(&src(s).0))
            .collect();
        if left.is_empty() {
            return Ok(());
        }
        #[derive(Clone, Copy)]
        struct Score {
            score: i64,
            name: i64,
            dst: isize,
            src: usize,
        }
        let cmp = |a: &Score, b: &Score| -> i64 {
            if a.dst < 0 {
                return i64::from(b.dst >= 0);
            } else if b.dst < 0 {
                return -1;
            }
            if a.score == b.score {
                return b.name - a.name;
            }
            b.score - a.score
        };
        let mut mx: Vec<Score> = Vec::new();
        for d in 0..dsts.len() {
            if renamed[d].is_some() {
                continue;
            }
            let (dpath, dv) = dst(d);
            let mut m = [Score {
                score: 0,
                name: 0,
                dst: -1,
                src: 0,
            }; 4];
            for &s in &left {
                let (spath, sv) = src(s);
                let this = Score {
                    score: self.similarity(sv, dv, min)? as i64,
                    name: i64::from(basename(&spath) == basename(&dpath)),
                    dst: d as isize,
                    src: s,
                };
                let mut worst = 0;
                for i in 1..4 {
                    if cmp(&m[i], &m[worst]) > 0 {
                        worst = i;
                    }
                }
                if cmp(&m[worst], &this) > 0 {
                    m[worst] = this;
                }
            }
            mx.extend(m);
        }
        mx.sort_by(|a, b| cmp(a, b).cmp(&0));
        for m in mx {
            if m.dst < 0 || (m.score as u64) < min {
                break;
            }
            let d = m.dst as usize;
            if renamed[d].is_some() || used[m.src] {
                continue;
            }
            used[m.src] = true;
            renamed[d] = Some(m.src);
            self.count_rename(side, &src(m.src).0, &dst(d).0);
        }
        Ok(())
    }

    fn renames(&mut self) -> Result<bool, GitError> {
        let possible = |o: &Self, s: usize| !o.pairs[s].is_empty() && !o.relevant[s].is_empty();
        if !(possible(self, 1) || possible(self, 2)) || self.o.no_renames {
            return Ok(true);
        }
        self.detect(1)?;
        self.detect(2)?;
        let mut clean = true;
        if self.depth == 0 && self.o.dir_renames != DirRenames::None {
            for side in 1..=2 {
                clean &= self.provisional_dir_renames(side);
            }
            let both: Vec<String> = self.dir_renames[1]
                .keys()
                .filter(|k| self.dir_renames[2].contains_key(*k))
                .cloned()
                .collect();
            for k in both {
                self.dir_renames[1].remove(&k);
                self.dir_renames[2].remove(&k);
            }
        }
        let mut collisions: [Collisions; 3] = Default::default();
        for (side, c) in collisions.iter_mut().enumerate().skip(1) {
            *c = self.collisions(&self.dir_renames[3 - side], &self.pairs[side]);
        }
        let mut combined = Vec::new();
        for side in 1..=2 {
            clean &= self.collect_renames(side, &mut combined, &mut collisions)?;
        }
        combined.sort_by(|a, b| a.one.cmp(&b.one));
        clean &= self.process_renames(&combined)?;
        Ok(clean)
    }

    fn provisional_dir_renames(&mut self, side: usize) -> bool {
        let mut clean = true;
        let counts = std::mem::take(&mut self.counts[side]);
        for (dir, c) in &counts {
            let (mut max, mut bad_max, mut best) = (0, 0, None);
            for (target, n) in c {
                if *n == max {
                    bad_max = max;
                } else if *n > max {
                    max = *n;
                    best = Some(target);
                }
            }
            if max == 0 {
                continue;
            }
            if bad_max == max {
                self.msg(
                    "CONFLICT(directory rename unclear split)",
                    dir,
                    &[],
                    format!(
                        "CONFLICT (directory rename split): Unclear where to rename {dir} to; it was renamed to multiple other directories, with no destination getting a majority of the files."
                    ),
                );
                clean = false;
            } else if let Some(best) = best {
                self.dir_renames[side].insert(dir.clone(), best.clone());
            }
        }
        self.counts[side] = counts;
        clean
    }

    fn dir_renamed<'m>(
        path: &str,
        renames: &'m BTreeMap<String, String>,
    ) -> Option<(&'m str, &'m str)> {
        let mut p = path;
        while let Some((dir, _)) = p.rsplit_once('/') {
            if let Some((k, v)) = renames.get_key_value(dir) {
                return Some((k, v));
            }
            p = dir;
        }
        None
    }

    fn apply_dir_rename(old_dir: &str, new_dir: &str, path: &str) -> String {
        let rest = &path[old_dir.len()..];
        if new_dir.is_empty() {
            rest[1..].to_owned()
        } else {
            format!("{new_dir}{rest}")
        }
    }

    fn collisions(&self, renames: &BTreeMap<String, String>, pairs: &[Pair]) -> Collisions {
        let mut out = Collisions::new();
        if renames.is_empty() {
            return out;
        }
        for p in pairs {
            if p.status != b'A' && p.status != b'R' {
                continue;
            }
            if let Some((old, new)) = Self::dir_renamed(&p.two, renames) {
                out.entry(Self::apply_dir_rename(old, new, &p.two))
                    .or_default()
                    .sources
                    .insert(p.two.clone());
            }
        }
        out
    }

    fn collect_renames(
        &mut self,
        side: usize,
        combined: &mut Vec<Pair>,
        collisions: &mut [Collisions; 3],
    ) -> Result<bool, GitError> {
        let mut clean = true;
        let pairs = std::mem::take(&mut self.pairs[side]);
        for mut p in pairs {
            if p.status != b'A' && p.status != b'R' {
                continue;
            }
            if self.o.dir_renames == DirRenames::None && p.status == b'R' {
                combined.push(p);
                continue;
            }
            let new_path = self.check_dir_rename(&p.two, side, collisions, &mut clean);
            if p.status != b'R' && new_path.is_none() {
                continue;
            }
            if let Some(new_path) = new_path {
                self.apply_dir_rename_mods(&mut p, new_path);
            }
            combined.push(p);
        }
        Ok(clean)
    }

    fn check_dir_rename(
        &mut self,
        path: &str,
        side: usize,
        collisions: &mut [Collisions; 3],
        clean: &mut bool,
    ) -> Option<String> {
        let other = 3 - side;
        if self.dir_renames[other].is_empty() || collisions[other].contains_key(path) {
            return None;
        }
        let (old_dir, new_dir) = Self::dir_renamed(path, &self.dir_renames[other])
            .map(|(a, b)| (a.to_owned(), b.to_owned()))?;
        if self.dir_renames[side].contains_key(&new_dir) {
            self.msg(
                "Directory rename skipped since directory was renamed on both sides",
                &old_dir,
                &[path, &new_dir],
                format!(
                    "WARNING: Avoiding applying {old_dir} -> {new_dir} rename to {path}, because {new_dir} itself was renamed."
                ),
            );
            return None;
        }
        let new_path = Self::apply_dir_rename(&old_dir, &new_dir, path);
        let c = collisions[side].get_mut(&new_path)?;
        let sources: Vec<String> = c.sources.iter().cloned().collect();
        let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
        let list = sources.join(", ");
        let ok = if c.reported {
            false
        } else if self.path_in_way(&new_path, 1 << side) {
            c.reported = true;
            self.msg(
                "CONFLICT (file in way of directory rename)",
                &new_path,
                &refs,
                format!(
                    "CONFLICT (implicit dir rename): Existing file/dir at {new_path} in the way of implicit directory rename(s) putting the following path(s) there: {list}."
                ),
            );
            false
        } else if sources.len() > 1 {
            c.reported = true;
            self.msg(
                "CONFLICT(directory rename collision)",
                &new_path,
                &refs,
                format!(
                    "CONFLICT (implicit dir rename): Cannot map more than one path to {new_path}; implicit directory renames tried to put these paths there: {list}"
                ),
            );
            false
        } else {
            true
        };
        *clean &= ok;
        ok.then_some(new_path)
    }

    fn path_in_way(&self, path: &str, side_mask: u8) -> bool {
        self.paths
            .get(path)
            .is_some_and(|mi| mi.clean || side_mask & (mi.filemask | mi.dirmask) != 0)
    }

    fn apply_dir_rename_mods(&mut self, pair: &mut Pair, new_path: String) {
        let old_path = pair.two.clone();
        let Some(mut ci) = self.paths.get(&old_path).cloned() else {
            return;
        };
        let mut dirs = Vec::new();
        let mut cur = new_path.as_str();
        while let Some((dir, _)) = cur.rsplit_once('/') {
            if self.paths.contains_key(dir) {
                break;
            }
            dirs.push(dir.to_owned());
            cur = dir;
        }
        for d in dirs.into_iter().rev() {
            let mut dir_ci = Info::resolved(null());
            dir_ci.clean = false;
            dir_ci.is_null = true;
            dir_ci.dirmask = ci.filemask;
            self.paths.insert(d, dir_ci);
        }
        if ci.dirmask == 0 {
            self.paths.remove(&old_path);
        } else {
            let mut new_ci = ci.clone();
            new_ci.dirmask = 0;
            new_ci.stages[1] = null();
            let old = self.paths.get_mut(&old_path).unwrap();
            old.filemask = 0;
            old.clean = true;
            for i in 0..3 {
                if old.dirmask & (1 << i) == 0 {
                    old.stages[i] = null();
                }
            }
            ci = new_ci;
        }
        let (with_path, with_rename) = if ci.filemask == 2 {
            (self.branch1.clone(), self.branch2.clone())
        } else {
            (self.branch2.clone(), self.branch1.clone())
        };
        let target = match self.paths.get(&new_path).cloned() {
            None => ci,
            Some(mut t) => {
                t.filemask |= ci.filemask;
                if t.dirmask != 0 {
                    t.df_conflict = true;
                }
                let i = (ci.filemask >> 1) as usize;
                t.pathnames[i] = ci.pathnames[i].clone();
                t.stages[i] = ci.stages[i];
                t
            }
        };
        let mut target = target;
        if self.o.dir_renames == DirRenames::True {
            let text = if pair.status == b'A' {
                format!(
                    "Path updated: {old_path} added in {with_path} inside a directory that was renamed in {with_rename}; moving it to {new_path}."
                )
            } else {
                format!(
                    "Path updated: {} renamed to {old_path} in {with_path}, inside a directory that was renamed in {with_rename}; moving it to {new_path}.",
                    pair.one
                )
            };
            self.msg(
                "Path updated due to directory rename",
                &new_path,
                &[&old_path],
                text,
            );
        } else {
            target.path_conflict = true;
            let text = if pair.status == b'A' {
                format!(
                    "CONFLICT (file location): {old_path} added in {with_path} inside a directory that was renamed in {with_rename}, suggesting it should perhaps be moved to {new_path}."
                )
            } else {
                format!(
                    "CONFLICT (file location): {} renamed to {old_path} in {with_path}, inside a directory that was renamed in {with_rename}, suggesting it should perhaps be moved to {new_path}.",
                    pair.one
                )
            };
            self.msg(
                "CONFLICT (directory rename suggested)",
                &new_path,
                &[&old_path],
                text,
            );
        }
        self.paths.insert(new_path.clone(), target);
        pair.two = new_path;
    }

    fn process_renames(&mut self, renames: &[Pair]) -> Result<bool, GitError> {
        let mut clean_merge = true;
        let mut i = 0;
        while i < renames.len() {
            let pair = &renames[i];
            i += 1;
            let oldpath = pair.one.clone();
            let newpath = pair.two.clone();
            match self.paths.get(&oldpath) {
                Some(o) if !o.clean => {}
                _ => continue,
            }
            if let Some(next) = renames.get(i)
                && next.one == oldpath
            {
                i += 1;
                let names = [oldpath.clone(), newpath.clone(), next.two.clone()];
                if names[1] == names[2] {
                    let base0 = self.paths[&names[0]].stages[0];
                    let s1 = self.paths.get_mut(&names[1]).unwrap();
                    s1.stages[0] = base0;
                    s1.filemask |= 1;
                    let base = self.paths.get_mut(&names[0]).unwrap();
                    base.is_null = true;
                    base.clean = true;
                    continue;
                }
                let o = self.paths[&names[0]].stages[0];
                let a = self.paths[&names[1]].stages[1];
                let b = self.paths[&names[2]].stages[2];
                let (clean, mut merged) =
                    self.content_merge(&oldpath, o, a, b, &names, 1 + 2 * self.depth)?;
                clean_merge = clean;
                let was_binary = !clean && merged == a;
                self.paths.get_mut(&names[1]).unwrap().stages[1] = merged;
                if was_binary {
                    merged = b;
                }
                self.paths.get_mut(&names[2]).unwrap().stages[2] = merged;
                for n in &names {
                    self.paths.get_mut(n).unwrap().path_conflict = true;
                }
                let (b1, b2) = (self.branch1.clone(), self.branch2.clone());
                self.msg(
                    "CONFLICT (rename/rename)",
                    &names[0],
                    &[&names[1], &names[2]],
                    format!(
                        "CONFLICT (rename/rename): {} renamed to {} in {b1} and to {} in {b2}.",
                        names[0], names[1], names[2]
                    ),
                );
                continue;
            }
            let (Some(oldinfo), Some(newinfo)) = (
                self.paths.get(&oldpath).cloned(),
                self.paths.get(&newpath).cloned(),
            ) else {
                continue;
            };
            let target = pair.side;
            let other = 3 - target;
            let old_sidemask = 1u8 << other;
            let source_deleted = oldinfo.filemask == 1;
            let mut collision = newinfo.filemask & old_sidemask != 0;
            let type_changed = !source_deleted
                && is_reg(oldinfo.stages[other].mode) != is_reg(newinfo.stages[target].mode);
            if type_changed && collision {
                collision = false;
            }
            let (rename_branch, delete_branch) = (self.branch(target), self.branch(other));
            let rename_delete = |o: &mut Self| {
                o.paths.get_mut(&newpath).unwrap().path_conflict = true;
                o.msg(
                    "CONFLICT (rename/delete)",
                    &newpath,
                    &[&oldpath],
                    format!(
                        "CONFLICT (rename/delete): {oldpath} renamed to {newpath} in {rename_branch}, but deleted in {delete_branch}."
                    ),
                );
            };
            if collision && !source_deleted {
                let mut names = [oldpath.clone(), oldpath.clone(), oldpath.clone()];
                names[target] = newpath.clone();
                let o = self.paths[&names[0]].stages[0];
                let a = self.paths[&names[1]].stages[1];
                let b = self.paths[&names[2]].stages[2];
                let (clean, merged) =
                    self.content_merge(&oldpath, o, a, b, &names, 1 + 2 * self.depth)?;
                self.paths.get_mut(&newpath).unwrap().stages[target] = merged;
                if !clean {
                    self.msg(
                        "CONFLICT (rename involved in collision)",
                        &newpath,
                        &[&oldpath],
                        format!(
                            "CONFLICT (rename involved in collision): rename of {oldpath} -> {newpath} has content conflicts AND collides with another path; this may result in nested conflict markers."
                        ),
                    );
                }
            } else if collision && source_deleted {
                rename_delete(self);
            } else {
                {
                    let n = self.paths.get_mut(&newpath).unwrap();
                    n.stages[0] = oldinfo.stages[0];
                    n.filemask |= 1;
                    n.pathnames[0] = oldpath.clone();
                }
                if type_changed {
                    let o = self.paths.get_mut(&oldpath).unwrap();
                    o.stages[0] = null();
                    o.filemask &= 6;
                } else if source_deleted {
                    rename_delete(self);
                } else {
                    let n = self.paths.get_mut(&newpath).unwrap();
                    n.stages[other] = oldinfo.stages[other];
                    n.filemask |= old_sidemask;
                    n.pathnames[other] = oldpath.clone();
                }
            }
            if !type_changed {
                let o = self.paths.get_mut(&oldpath).unwrap();
                o.is_null = true;
                o.clean = true;
            }
        }
        Ok(clean_merge)
    }

    fn blob(&self, v: Ver) -> Result<Vec<u8>, GitError> {
        if v.mode == 0 || v.id.is_zero() {
            return Ok(Vec::new());
        }
        Ok(self.repo.find_blob(v.id)?.content().to_vec())
    }

    // merge_3way: the status (0 clean, 1 conflicts, 2 binary) and content.
    fn merge_3way(
        &mut self,
        path: &str,
        o: Ver,
        a: Ver,
        b: Ver,
        names: &[String; 3],
        extra: usize,
    ) -> Result<(u8, Vec<u8>), GitError> {
        let labels = if names[0] == names[1] && names[1] == names[2] {
            [
                self.ancestor.clone(),
                self.branch1.clone(),
                self.branch2.clone(),
            ]
        } else {
            [
                format!("{}:{}", self.ancestor, names[0]),
                format!("{}:{}", self.branch1, names[1]),
                format!("{}:{}", self.branch2, names[2]),
            ]
        };
        let (orig, ours, theirs) = (self.blob(o)?, self.blob(a)?, self.blob(b)?);
        let binary = |d: &[u8]| d[..d.len().min(8000)].contains(&0);
        if binary(&orig) || binary(&ours) || binary(&theirs) {
            if self.depth > 0 {
                return Ok((0, orig));
            }
            return Ok(match self.o.favor.as_deref() {
                Some("ours") => (0, ours),
                Some("theirs") => (0, theirs),
                _ => {
                    self.msg(
                        "CONFLICT (binary)",
                        path,
                        &[],
                        format!(
                            "warning: Cannot merge binary files: {path} ({} vs. {})",
                            labels[1], labels[2]
                        ),
                    );
                    (2, ours)
                }
            });
        }
        let mut opts = self.o.file.clone();
        opts.labels = [labels[1].clone(), labels[0].clone(), labels[2].clone()];
        opts.favor = if self.depth > 0 {
            None
        } else {
            self.o.favor.clone()
        };
        opts.marker_size = Some(7 + extra as u16);
        let (content, conflicts) = merge_file(&ours, &orig, &theirs, &opts)?;
        Ok((u8::from(conflicts > 0), content))
    }

    // handle_content_merge
    fn content_merge(
        &mut self,
        path: &str,
        o: Ver,
        a: Ver,
        b: Ver,
        names: &[String; 3],
        extra: usize,
    ) -> Result<(bool, Ver), GitError> {
        let mut clean = true;
        let mut result = null();
        if a.mode == b.mode || a.mode == o.mode {
            result.mode = b.mode;
        } else {
            result.mode = a.mode;
            clean = b.mode == o.mode;
        }
        if a.id == b.id || a.id == o.id {
            result.id = b.id;
        } else if b.id == o.id {
            result.id = a.id;
        } else if is_reg(a.mode) {
            let two_way = o.mode & FMT != a.mode & FMT;
            let base = if two_way { null() } else { o };
            let (status, content) = self.merge_3way(path, base, a, b, names, extra)?;
            result.id = self.repo.blob(&content)?;
            if status > 0 {
                clean = false;
            }
            self.msg("Auto-merging", path, &[], format!("Auto-merging {path}"));
        } else if a.mode & FMT == GITLINK {
            let two_way = o.mode & FMT != a.mode & FMT;
            let base = if two_way { Oid::ZERO_SHA1 } else { o.id };
            let (ok, id) = self.merge_submodule(&names[0], base, a.id, b.id)?;
            clean = ok;
            result.id = id;
            if self.depth > 0 && two_way && !clean {
                result = o;
            }
        } else if a.mode & FMT == LNK {
            if self.depth > 0 {
                clean = false;
                result = o;
            } else {
                match self.o.favor.as_deref() {
                    Some("ours") => result.id = a.id,
                    Some("theirs") => result.id = b.id,
                    _ => {
                        clean = false;
                        result.id = a.id;
                    }
                }
            }
        }
        Ok((clean, result))
    }

    fn merge_submodule(
        &mut self,
        path: &str,
        o: Oid,
        a: Oid,
        b: Oid,
    ) -> Result<(bool, Oid), GitError> {
        let fallback = if self.depth > 0 { o } else { a };
        let sub = self.repo.workdir().and_then(|w| {
            Repository::open_ext(
                w.join(path),
                git2::RepositoryOpenFlags::NO_SEARCH,
                std::iter::empty::<&std::ffi::OsStr>(),
            )
            .ok()
        });
        let Some(sub) = sub else {
            self.msg(
                "CONFLICT (submodule not initialized)",
                path,
                &[],
                format!("Failed to merge submodule {path} (not checked out)"),
            );
            return Ok((false, fallback));
        };
        if o.is_zero() {
            self.msg(
                "CONFLICT (submodule lacks merge base)",
                path,
                &[],
                format!("Failed to merge submodule {path} (no merge base)"),
            );
            return Ok((false, fallback));
        }
        if [o, a, b].iter().any(|id| sub.find_commit(*id).is_err()) {
            self.msg(
                "CONFLICT (submodule history not available)",
                path,
                &[],
                format!("Failed to merge submodule {path} (commits not present)"),
            );
            return Ok((false, fallback));
        }
        let contains =
            |anc: Oid, of: Oid| anc == of || sub.graph_descendant_of(of, anc).unwrap_or(false);
        if !contains(o, a) || !contains(o, b) {
            self.msg(
                "CONFLICT (submodule may have rewinds)",
                path,
                &[],
                format!("Failed to merge submodule {path} (commits don't follow merge-base)"),
            );
            return Ok((false, fallback));
        }
        for (from, to) in [(a, b), (b, a)] {
            if contains(from, to) {
                self.msg(
                    "Fast forwarding submodule",
                    path,
                    &[],
                    format!("Note: Fast-forwarding submodule {path} to {to}"),
                );
                return Ok((true, to));
            }
        }
        if self.depth == 0 {
            self.msg(
                "CONFLICT (submodule)",
                path,
                &[],
                format!("Failed to merge submodule {path}"),
            );
        }
        Ok((false, fallback))
    }

    fn unique_path(&self, path: &str, branch: &str) -> String {
        let base = format!("{path}~{}", branch.replace('/', "_"));
        let mut p = base.clone();
        let mut n = 0;
        while self.paths.contains_key(&p) {
            p = format!("{base}_{n}");
            n += 1;
        }
        p
    }

    fn has_children(&self, path: &str) -> bool {
        let dir = format!("{path}/");
        self.results
            .range(dir.clone()..)
            .next()
            .is_some_and(|(k, _)| k.starts_with(&dir))
    }

    fn record(&mut self, path: &str, ci: &Info) {
        if !ci.is_null && ci.result.mode != 0 {
            self.results.insert(path.to_owned(), ci.result);
        }
    }

    fn process_entries(&mut self) -> Result<(), GitError> {
        let mut order: Vec<String> = self.paths.keys().cloned().collect();
        order.sort_by_cached_key(|p| {
            let mut k = p.clone().into_bytes();
            k.push(b'/');
            k
        });
        for path in order.into_iter().rev() {
            let Some(ci) = self.paths.get(&path).cloned() else {
                continue;
            };
            if ci.clean {
                self.record(&path, &ci);
            } else {
                self.process_entry(path, ci)?;
            }
        }
        Ok(())
    }

    fn process_entry(&mut self, mut path: String, mut ci: Info) -> Result<(), GitError> {
        if ci.dirmask != 0 && ci.filemask == 0 {
            return Ok(());
        }
        let mut df_file_index = 0;
        if ci.df_conflict && !self.has_children(&path) {
            ci.df_conflict = false;
            ci.clean = false;
            ci.is_null = false;
            ci.match_mask &= !ci.dirmask;
            ci.dirmask = 0;
            for i in 0..3 {
                if ci.filemask & (1 << i) == 0 {
                    ci.stages[i] = null();
                }
            }
        } else if ci.df_conflict {
            if ci.filemask == 1 {
                return Ok(());
            }
            let mut new_ci = ci.clone();
            new_ci.is_null = false;
            new_ci.match_mask &= !new_ci.dirmask;
            new_ci.dirmask = 0;
            for i in 0..3 {
                if new_ci.filemask & (1 << i) == 0 {
                    new_ci.stages[i] = null();
                }
            }
            df_file_index = if ci.dirmask & 2 != 0 { 2 } else { 1 };
            let branch = self.branch(df_file_index);
            let new_path = self.unique_path(&path, &branch);
            self.paths.insert(new_path.clone(), new_ci.clone());
            self.msg(
                "CONFLICT (file/directory)",
                &new_path,
                &[&path],
                format!(
                    "CONFLICT (file/directory): directory in the way of {path} from {branch}; moving it to {new_path} instead."
                ),
            );
            path = new_path;
            ci = new_ci;
        }
        if ci.match_mask != 0 {
            ci.clean = !ci.df_conflict && !ci.path_conflict;
            if ci.match_mask == 6 {
                ci.result = ci.stages[1];
            } else {
                let othermask = 7 & !ci.match_mask;
                let side = if othermask == 4 { 2 } else { 1 };
                ci.result = ci.stages[side];
                ci.is_null = ci.result.mode == 0;
                if ci.is_null {
                    ci.clean = true;
                }
            }
        } else if ci.filemask >= 6 && ci.stages[1].mode & FMT != ci.stages[2].mode & FMT {
            if self.depth > 0 {
                ci.clean = false;
                ci.result = ci.stages[0];
                ci.is_null = ci.result.mode == 0;
            } else {
                let [o_mode, a_mode, b_mode] = ci.stages.map(|s| s.mode);
                let (rename_a, rename_b) = if is_reg(a_mode) {
                    (true, false)
                } else if is_reg(b_mode) {
                    (false, true)
                } else {
                    (true, true)
                };
                let a_path = rename_a.then(|| self.unique_path(&path, &self.branch1));
                let b_path = rename_b.then(|| self.unique_path(&path, &self.branch2));
                if let (Some(a), Some(b)) = (&a_path, &b_path) {
                    self.msg(
                        "CONFLICT (distinct modes)",
                        &path,
                        &[a, b],
                        format!(
                            "CONFLICT (distinct types): {path} had different types on each side; renamed both of them so each can be recorded somewhere."
                        ),
                    );
                } else {
                    let moved = a_path.as_ref().or(b_path.as_ref()).unwrap();
                    self.msg(
                        "CONFLICT (distinct modes)",
                        &path,
                        &[moved],
                        format!(
                            "CONFLICT (distinct types): {path} had different types on each side; renamed one of them so each can be recorded somewhere."
                        ),
                    );
                }
                ci.clean = false;
                let mut new_ci = ci.clone();
                new_ci.result = ci.stages[2];
                new_ci.stages[1] = null();
                new_ci.filemask = 5;
                if b_mode & FMT != o_mode & FMT {
                    new_ci.stages[0] = null();
                    new_ci.filemask = 4;
                }
                ci.result = ci.stages[1];
                ci.stages[2] = null();
                ci.filemask = 3;
                if a_mode & FMT != o_mode & FMT {
                    ci.stages[0] = null();
                    ci.filemask = 2;
                }
                if let Some(a) = &a_path {
                    self.paths.insert(a.clone(), ci.clone());
                }
                let b_path = b_path.unwrap_or_else(|| path.clone());
                self.paths.insert(b_path.clone(), new_ci.clone());
                if rename_a && rename_b {
                    self.paths.remove(&path);
                }
                self.conflicted.insert(b_path.clone(), new_ci.clone());
                self.record(&b_path, &new_ci);
                if let Some(a) = a_path {
                    path = a;
                }
            }
        } else if ci.filemask >= 6 {
            let [o, a, b] = ci.stages;
            let names = ci.pathnames.clone();
            let (clean, merged) = self.content_merge(&path, o, a, b, &names, self.depth * 2)?;
            ci.clean = clean && !ci.df_conflict && !ci.path_conflict;
            ci.result = merged;
            ci.is_null = merged.mode == 0;
            if clean && ci.df_conflict {
                ci.filemask = 1 << df_file_index;
                ci.stages[df_file_index] = merged;
            }
            if !clean {
                let reason = if merged.mode & FMT == GITLINK {
                    "submodule"
                } else if ci.filemask == 6 {
                    "add/add"
                } else {
                    "content"
                };
                self.msg(
                    "CONFLICT (contents)",
                    &path,
                    &[],
                    format!("CONFLICT ({reason}): Merge conflict in {path}"),
                );
            }
        } else if ci.filemask == 3 || ci.filemask == 5 {
            let side = if ci.filemask == 5 { 2 } else { 1 };
            let index = if self.depth > 0 { 0 } else { side };
            ci.result = ci.stages[index];
            ci.clean = false;
            let (modify, delete) = (self.branch(side), self.branch(3 - side));
            if !(ci.path_conflict && ci.stages[0].id == ci.stages[side].id) {
                self.msg(
                    "CONFLICT (modify/delete)",
                    &path,
                    &[],
                    format!(
                        "CONFLICT (modify/delete): {path} deleted in {delete} and modified in {modify}.  Version {modify} of {path} left in tree."
                    ),
                );
            }
        } else if ci.filemask == 2 || ci.filemask == 4 {
            let side = if ci.filemask == 4 { 2 } else { 1 };
            ci.result = ci.stages[side];
            ci.clean = !ci.df_conflict && !ci.path_conflict;
        } else if ci.filemask == 1 {
            ci.is_null = true;
            ci.result = null();
            ci.clean = !ci.path_conflict;
        }
        if !ci.clean {
            self.conflicted.insert(path.clone(), ci.clone());
        }
        self.record(&path, &ci);
        Ok(())
    }

    fn write_tree(&self) -> Result<Oid, GitError> {
        #[derive(Default)]
        struct Node {
            files: BTreeMap<String, Ver>,
            dirs: BTreeMap<String, Node>,
        }
        let mut root = Node::default();
        for (path, v) in &self.results {
            let mut node = &mut root;
            let mut parts: Vec<&str> = path.split('/').collect();
            let name = parts.pop().unwrap();
            for p in parts {
                node = node.dirs.entry(p.to_owned()).or_default();
            }
            node.files.insert(name.to_owned(), *v);
        }
        fn write(repo: &Repository, n: &Node) -> Result<Oid, GitError> {
            let mut b = repo.treebuilder(None)?;
            for (name, v) in &n.files {
                if !n.dirs.contains_key(name) {
                    b.insert(name, v.id, v.mode as i32)?;
                }
            }
            for (name, d) in &n.dirs {
                b.insert(name, write(repo, d)?, DIR as i32)?;
            }
            Ok(b.write()?)
        }
        write(self.repo, &root)
    }

    fn run(mut self, base: &Tree, side1: &Tree, side2: &Tree) -> Result<Merged, GitError> {
        self.collect(
            "",
            [Some(base.clone()), Some(side1.clone()), Some(side2.clone())],
            0,
        )?;
        let renames_clean = self.renames()?;
        self.process_entries()?;
        let tree = self.write_tree()?;
        let mut conflicted = Vec::new();
        for (path, ci) in &self.conflicted {
            for (i, s) in ci.stages.iter().enumerate() {
                if ci.filemask & (1 << i) != 0 {
                    conflicted.push(Stage {
                        path: path.clone(),
                        stage: i as u8 + 1,
                        mode: s.mode,
                        id: s.id,
                    });
                }
            }
        }
        Ok(Merged {
            tree,
            clean: renames_clean && self.conflicted.is_empty(),
            conflicted,
            messages: self.msgs,
        })
    }
}

fn masks(n: &[Option<Ver>; 3]) -> (u8, u8, u8) {
    let (mut mask, mut dirmask) = (0, 0);
    for (i, v) in n.iter().enumerate() {
        if let Some(v) = v {
            mask |= 1 << i;
            if v.mode == DIR {
                dirmask |= 1 << i;
            }
        }
    }
    (mask, dirmask, mask & !dirmask)
}

/// merge_incore_nonrecursive: merge `side1` and `side2` from `base`.
pub fn merge_trees(
    repo: &Repository,
    base: &Tree,
    side1: &Tree,
    side2: &Tree,
    o: &OrtOpts,
) -> Result<Merged, GitError> {
    let mut ort = Ort::new(repo, o, &o.branch1, &o.branch2, 0);
    ort.ancestor = o.ancestor.clone().unwrap_or_default();
    ort.run(base, side1, side2)
}

/// merge_incore_recursive: merge two commits, first merging their merge
/// bases into a virtual one when there are several.
pub fn merge_commits(
    repo: &Repository,
    bases: Vec<Commit>,
    h1: &Commit,
    h2: &Commit,
    o: &OrtOpts,
) -> Result<Merged, GitError> {
    internal(repo, o, 0, &o.branch1, &o.branch2, Some(bases), h1, h2)
}

#[allow(clippy::too_many_arguments)]
fn internal(
    repo: &Repository,
    o: &OrtOpts,
    depth: usize,
    b1: &str,
    b2: &str,
    bases: Option<Vec<Commit>>,
    h1: &Commit,
    h2: &Commit,
) -> Result<Merged, GitError> {
    let mut bases = match bases {
        Some(b) => b,
        None => merge_bases(repo, h1.id(), h2.id())?,
    }
    .into_iter();
    let first = bases.next();
    let rest: Vec<Commit> = bases.collect();
    let (mut merged, ancestor) = match first {
        None => (None, "empty tree".to_owned()),
        Some(c) if !rest.is_empty() => (Some(c), "merged common ancestors".to_owned()),
        Some(c) => {
            let name = match &o.ancestor {
                Some(a) if depth == 0 => a.clone(),
                _ => c
                    .as_object()
                    .short_id()?
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            };
            (Some(c), name)
        }
    };
    for next in rest {
        let prev = merged.take().unwrap();
        let inner = internal(
            repo,
            o,
            depth + 1,
            "Temporary merge branch 1",
            "Temporary merge branch 2",
            None,
            &prev,
            &next,
        )?;
        let sig = git2::Signature::new("rgit", "rgit", &git2::Time::new(0, 0))?;
        let tree = repo.find_tree(inner.tree)?;
        let id = repo.commit(None, &sig, &sig, "merged tree", &tree, &[&prev, &next])?;
        merged = Some(repo.find_commit(id)?);
    }
    let base = match &merged {
        Some(c) => c.tree()?,
        None => repo.find_tree(repo.treebuilder(None)?.write()?)?,
    };
    let mut ort = Ort::new(repo, o, b1, b2, depth);
    ort.ancestor = ancestor;
    ort.run(&base, &h1.tree()?, &h2.tree()?)
}

/// The merge bases of two commits, reversed from git's order as
/// merge-ort takes them.
pub fn merge_bases<'r>(repo: &'r Repository, a: Oid, b: Oid) -> Result<Vec<Commit<'r>>, GitError> {
    let ids: Vec<Oid> = match repo.merge_bases(a, b) {
        Ok(ids) => ids.iter().copied().collect(),
        Err(e) if e.code() == git2::ErrorCode::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    ids.into_iter()
        .rev()
        .map(|id| Ok(repo.find_commit(id)?))
        .collect()
}
