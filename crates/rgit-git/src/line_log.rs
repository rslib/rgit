//! git's line-level log (`log -L`), ported from line-log.c: follow line
//! ranges of files back through history and show how each commit changed
//! them.

use crate::rev::RevParse;
use std::collections::HashMap;

use git2::{Delta, DiffFindOptions, DiffOptions, Oid, Repository};

use crate::{DiffLine, FileDiff, GitError, Hunk, LineOrigin, StatusCode};

/// Sorted, disjoint 0-based `[start, end)` line ranges.
type Ranges = Vec<(usize, usize)>;

/// The ranges tracked in each file, sorted by path.
type Tracked = Vec<(String, Ranges)>;

/// A diff as matching parent and target ranges.
#[derive(Default)]
struct DiffRanges {
    parent: Ranges,
    target: Ranges,
}

fn lines(data: &[u8]) -> Vec<&[u8]> {
    data.split_inclusive(|&b| b == b'\n').collect()
}

fn blob(repo: &Repository, oid: Oid) -> Result<Vec<u8>, GitError> {
    Ok(repo.find_blob(oid)?.content().to_vec())
}

fn bad(spec: &str) -> GitError {
    GitError::Other(format!(
        "-L argument not 'start,end:file' or ':funcname:file': {spec}"
    ))
}

/// A `-L` regex as git compiles it: basic, `^` and `$` at each line.
fn regex(re: &str) -> Result<crate::userdiff::Regex, GitError> {
    crate::userdiff::Regex::new(re.as_bytes(), crate::userdiff::NEWLINE)
        .map_err(|e| GitError::Other(format!("-L parameter '{re}': {e}")))
}

/// Split `/re/rest` into `re` and `rest`, honouring `\/`.
fn slashed(s: &str) -> Option<(&str, &str)> {
    let body = s.strip_prefix('/')?;
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '/' if !escaped => return Some((&body[..i], &body[i + 1..])),
            _ => escaped = false,
        }
    }
    None
}

/// Parse one `-L` argument against the file as it is at `tip`: the path and
/// its range.
fn parse_spec(
    repo: &Repository,
    tip: &git2::Commit,
    spec: &str,
) -> Result<(String, (usize, usize)), GitError> {
    let tree = tip.tree()?;
    let data_of = |path: &str| -> Result<Vec<u8>, GitError> {
        let entry = tree
            .get_path(std::path::Path::new(path))
            .map_err(|_| GitError::Other(format!("There is no path {path} in the commit")))?;
        blob(repo, entry.id())
    };
    if let Some(rest) = spec.strip_prefix(':') {
        let (func, path) = rest.split_once(':').ok_or_else(|| bad(spec))?;
        let data = data_of(path)?;
        let ls = lines(&data);
        let re = regex(func)?;
        let driver = crate::userdiff::driver(repo, path, crate::userdiff::Fallback::None)?
            .and_then(|d| d.funcname);
        let is_funcname = |l: &[u8]| crate::userdiff::is_func(driver.as_ref(), l);
        let begin = ls
            .iter()
            .position(|l| re.is_match(l) && is_funcname(l))
            .ok_or_else(|| {
                GitError::Other(format!(
                    "-L parameter '{func}' starting at line 1: no match"
                ))
            })?;
        let end = (begin + 1..ls.len())
            .find(|&i| is_funcname(ls[i]))
            .unwrap_or(ls.len());
        return Ok((path.to_owned(), (begin, end)));
    }
    let (start, rest) = parse_loc(spec)?;
    let (end, rest) = match rest.strip_prefix(',') {
        Some(r) => parse_loc(r)?,
        None => (Loc::None, rest),
    };
    let path = rest
        .strip_prefix(':')
        .filter(|p| !p.is_empty())
        .ok_or_else(|| bad(spec))?;
    let data = data_of(path)?;
    let ls = lines(&data);
    let find = |pattern: &str, from: usize| -> Result<usize, GitError> {
        let re = regex(pattern)?;
        (from..ls.len())
            .find(|&i| re.is_match(ls[i]))
            .ok_or_else(|| {
                GitError::Other(format!(
                    "-L parameter '{pattern}' starting at line {}: {}",
                    from + 1,
                    re.no_match()
                ))
            })
    };
    // 1-based inclusive lines, as git's parse_range_arg works them out.
    let begin = match start {
        Loc::None => 1,
        Loc::Line(n) => n,
        Loc::Regex(re) => find(re, 0)? + 1,
        Loc::Plus(_) | Loc::Minus(_) => return Err(bad(spec)),
    };
    let (mut lo, mut hi) = match end {
        Loc::None => (begin, ls.len()),
        Loc::Line(n) => (begin, n),
        Loc::Plus(n) => (begin, (begin + n).saturating_sub(1).max(begin)),
        Loc::Minus(n) => ((begin + 1).saturating_sub(n).max(1), begin),
        Loc::Regex(re) => (begin, find(re, begin)? + 1),
    };
    if lo > hi {
        std::mem::swap(&mut lo, &mut hi);
    }
    if lo == 0 || lo > ls.len().max(1) {
        return Err(GitError::Other(format!(
            "file {path} has only {} lines",
            ls.len()
        )));
    }
    Ok((path.to_owned(), (lo - 1, hi.min(ls.len()))))
}

enum Loc<'a> {
    None,
    Line(usize),
    Plus(usize),
    Minus(usize),
    Regex(&'a str),
}

/// One end of a `start,end` range and what follows it.
fn parse_loc(s: &str) -> Result<(Loc<'_>, &str), GitError> {
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    let number = |s: &str| -> Result<(usize, usize), GitError> {
        let n = digits(s);
        let v = s[..n].parse().map_err(|_| bad(s))?;
        Ok((v, n))
    };
    if let Some(r) = s.strip_prefix('+') {
        let (v, n) = number(r)?;
        return Ok((Loc::Plus(v), &r[n..]));
    }
    if let Some(r) = s.strip_prefix('-') {
        let (v, n) = number(r)?;
        return Ok((Loc::Minus(v), &r[n..]));
    }
    // `^/re/` searches from the start of the file, as `/re/` does in log.
    let r = s
        .strip_prefix('^')
        .filter(|r| r.starts_with('/'))
        .unwrap_or(s);
    if let Some((re, rest)) = slashed(r) {
        return Ok((Loc::Regex(re), rest));
    }
    if digits(s) > 0 {
        let (v, n) = number(s)?;
        return Ok((Loc::Line(v), &s[n..]));
    }
    Ok((Loc::None, s))
}

fn union(a: &Ranges, b: &Ranges) -> Ranges {
    let mut all: Vec<(usize, usize)> = a.iter().chain(b).copied().collect();
    all.sort();
    let mut out: Ranges = Vec::new();
    for (s, e) in all {
        if s == e {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.1 >= s => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

fn difference(a: &Ranges, b: &Ranges) -> Ranges {
    let mut out = Vec::new();
    let mut j = 0;
    for &(mut start, end) in a {
        while start < end {
            while j < b.len() && start >= b[j].1 {
                j += 1;
            }
            if j >= b.len() || end <= b[j].0 {
                out.push((start, end));
                break;
            }
            if start < b[j].0 {
                out.push((start, b[j].0));
            }
            start = b[j].1;
        }
    }
    out
}

/// Shift `rs` by the lines the diff adds or removes before each range.
fn shift(rs: &Ranges, diff: &DiffRanges) -> Ranges {
    let mut out = Vec::new();
    let (mut j, mut offset) = (0, 0isize);
    for &(s, e) in rs {
        while j < diff.target.len() && s >= diff.target[j].0 {
            let p = diff.parent[j];
            let t = diff.target[j];
            offset += (p.1 - p.0) as isize - (t.1 - t.0) as isize;
            j += 1;
        }
        out.push((
            (s as isize + offset) as usize,
            (e as isize + offset) as usize,
        ));
    }
    out
}

fn overlap(a: (usize, usize), b: (usize, usize)) -> bool {
    !(a.1 <= b.0 || b.1 <= a.0)
}

/// The hunks of `diff` that touch `rs`.
fn touched(diff: &DiffRanges, rs: &Ranges) -> DiffRanges {
    let mut out = DiffRanges::default();
    let mut j = 0;
    for (i, &t) in diff.target.iter().enumerate() {
        while t.0 >= rs[j].1 {
            j += 1;
            if j == rs.len() {
                return out;
            }
        }
        if overlap(t, rs[j]) {
            out.parent.push(diff.parent[i]);
            out.target.push(t);
        }
    }
    out
}

/// The zero-context line diff of `a` to `b` as parent and target ranges.
fn collect_diff(a: &[u8], b: &[u8]) -> Result<DiffRanges, GitError> {
    let mut opts = DiffOptions::new();
    opts.context_lines(0).interhunk_lines(0);
    let patch = git2::Patch::from_buffers(a, None, b, None, Some(&mut opts))?;
    let mut out = DiffRanges::default();
    for h in 0..patch.num_hunks() {
        let (hunk, _) = patch.hunk(h)?;
        let start = |at: u32, n: u32| if n == 0 { at } else { at - 1 } as usize;
        let ps = start(hunk.old_start(), hunk.old_lines());
        let ts = start(hunk.new_start(), hunk.new_lines());
        out.parent.push((ps, ps + hunk.old_lines() as usize));
        out.target.push((ts, ts + hunk.new_lines() as usize));
    }
    Ok(out)
}

/// A changed file of one commit.
struct Pair {
    old_path: String,
    new_path: String,
    old: Option<Oid>,
    new: Oid,
}

fn pairs(
    repo: &Repository,
    commit: &git2::Commit,
    parent: Option<&git2::Commit>,
    tracked: &Tracked,
) -> Result<Vec<Pair>, GitError> {
    let old_tree = parent.map(|p| p.tree()).transpose()?;
    let tree = commit.tree()?;
    let mut opts = DiffOptions::new();
    for (p, _) in tracked {
        opts.pathspec(p);
    }
    opts.disable_pathspec_match(true);
    let mut diff = repo.diff_tree_to_tree(old_tree.as_ref(), Some(&tree), Some(&mut opts))?;
    if parent.is_some() && diff.deltas().any(|d| d.status() == Delta::Added) {
        diff = repo.diff_tree_to_tree(old_tree.as_ref(), Some(&tree), None)?;
        diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
    }
    let path = |f: git2::DiffFile| {
        String::from_utf8_lossy(f.path_bytes().unwrap_or_default()).into_owned()
    };
    let mut out = Vec::new();
    for d in diff.deltas() {
        let new_path = path(d.new_file());
        if d.status() == Delta::Deleted || !tracked.iter().any(|(p, _)| *p == new_path) {
            continue;
        }
        out.push(Pair {
            old_path: path(d.old_file()),
            new_path,
            old: (d.status() != Delta::Added).then(|| d.old_file().id()),
            new: d.new_file().id(),
        });
    }
    Ok(out)
}

/// Map `tracked` from `commit` to `parent`: the parent's ranges, whether the
/// commit changed any tracked line, and the diffs to show for it.
fn process(
    repo: &Repository,
    commit: &git2::Commit,
    parent: Option<&git2::Commit>,
    tracked: &Tracked,
) -> Result<(bool, Tracked, Vec<FileDiff>), GitError> {
    let mut out = tracked.clone();
    let mut shown = Vec::new();
    for pair in pairs(repo, commit, parent, tracked)? {
        let Some(i) = tracked.iter().position(|(p, _)| *p == pair.new_path) else {
            continue;
        };
        let rs = &tracked[i].1;
        if rs.is_empty() {
            continue;
        }
        let target = blob(repo, pair.new)?;
        let old = pair.old.map(|o| blob(repo, o)).transpose()?;
        let diff = collect_diff(old.as_deref().unwrap_or_default(), &target)?;
        let hit = touched(&diff, rs);
        out[i] = (
            pair.old_path.clone(),
            union(&shift(&difference(rs, &hit.target), &diff), &hit.parent),
        );
        if !hit.target.is_empty() {
            shown.push(dump(&pair, rs, &hit, old.as_deref(), &target));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((!shown.is_empty(), out, shown))
}

/// The file's diff limited to the ranges, as git's dump_diff_hacky_one
/// prints it.
fn dump(pair: &Pair, rs: &Ranges, diff: &DiffRanges, old: Option<&[u8]>, new: &[u8]) -> FileDiff {
    let (p_lines, t_lines) = (lines(old.unwrap_or_default()), lines(new));
    let mut header = format!("diff --git a/{} b/{}\n", pair.old_path, pair.new_path);
    header.push_str(&match old {
        Some(_) => format!("--- a/{}\n", pair.old_path),
        None => "--- /dev/null\n".to_owned(),
    });
    header.push_str(&format!("+++ b/{}\n", pair.new_path));
    let mut hunks = Vec::new();
    let push = |lines: &mut Vec<DiffLine>, origin, line: &[u8]| {
        let text = String::from_utf8_lossy(line);
        lines.push(DiffLine {
            origin,
            text: text.strip_suffix('\n').unwrap_or(&text).to_owned(),
        });
        if !line.ends_with(b"\n") {
            lines.push(DiffLine {
                origin: LineOrigin::Meta,
                text: "\\ No newline at end of file".to_owned(),
            });
        }
    };
    let (target, parent) = (&diff.target, &diff.parent);
    let mut j = 0;
    for &(t_start, t_end) in rs {
        while j < target.len() && target[j].1 < t_start {
            j += 1;
        }
        if j == target.len() || target[j].0 >= t_end {
            continue;
        }
        let mut j_last = j;
        while j_last < target.len() && target[j_last].0 < t_end {
            j_last += 1;
        }
        if j_last > j {
            j_last -= 1;
        }
        let p_start = if t_start < target[j].0 {
            parent[j].0 as isize - (target[j].0 - t_start) as isize
        } else {
            parent[j].0 as isize
        };
        let p_end = if t_end > target[j_last].1 {
            (parent[j_last].1 + (t_end - target[j_last].1)) as isize
        } else {
            parent[j_last].1 as isize
        };
        let (p_start, p_end) = if p_start == 0 && p_end == 0 {
            (-1, -1)
        } else {
            (p_start, p_end)
        };
        let mut lines = Vec::new();
        let mut t_cur = t_start;
        while j < target.len() && target[j].0 < t_end {
            while t_cur < target[j].0 {
                push(&mut lines, LineOrigin::Context, t_lines[t_cur]);
                t_cur += 1;
            }
            for line in &p_lines[parent[j].0..parent[j].1] {
                push(&mut lines, LineOrigin::Removed, line);
            }
            while t_cur < target[j].1 && t_cur < t_end {
                push(&mut lines, LineOrigin::Added, t_lines[t_cur]);
                t_cur += 1;
            }
            j += 1;
        }
        while t_cur < t_end {
            push(&mut lines, LineOrigin::Context, t_lines[t_cur]);
            t_cur += 1;
        }
        hunks.push(Hunk {
            header: format!(
                "@@ -{},{} +{},{} @@",
                p_start + 1,
                p_end - p_start,
                t_start + 1,
                t_end - t_start
            ),
            new_start: t_start as u32 + 1,
            lines,
        });
    }
    FileDiff {
        path: pair.new_path.clone(),
        old_path: None,
        status: StatusCode::Modified,
        hunks,
        binary: false,
        header,
        similarity: 0,
        sizes: (0, 0),
        modes: (0, 0),
        ids: Default::default(),
    }
}

/// `git log -L`: for each commit of `order` (git's topological order, `tip`
/// first), the diffs to show, empty for a merge that changed the ranges
/// against every parent, or None when it did not touch them.
pub(crate) fn line_log(
    repo: &Repository,
    tip: &str,
    order: &[String],
    specs: &[String],
    first_parent: bool,
) -> Result<Vec<Option<Vec<FileDiff>>>, GitError> {
    let tip = repo.rev_single(tip)?.peel_to_commit()?;
    let mut start: Tracked = Vec::new();
    for spec in specs {
        let (path, range) = parse_spec(repo, &tip, spec)?;
        match start.iter_mut().find(|(p, _)| *p == path) {
            Some((_, rs)) => *rs = union(rs, &vec![range]),
            None => start.push((path, vec![range])),
        }
    }
    start.sort_by(|a, b| a.0.cmp(&b.0));
    let mut pending: HashMap<Oid, Tracked> = HashMap::new();
    pending.insert(tip.id(), start);
    let add = |pending: &mut HashMap<Oid, Tracked>, oid: Oid, t: Tracked| {
        let entry = pending.entry(oid).or_default();
        for (path, rs) in t {
            match entry.iter_mut().find(|(p, _)| *p == path) {
                Some((_, have)) => *have = union(have, &rs),
                None => entry.push((path, rs)),
            }
        }
        entry.sort_by(|a, b| a.0.cmp(&b.0));
    };
    let mut out = Vec::new();
    for id in order {
        let oid = Oid::from_str(id)?;
        let Some(tracked) = pending.remove(&oid) else {
            out.push(None);
            continue;
        };
        let commit = repo.find_commit(oid)?;
        let mut parents: Vec<git2::Commit> = commit.parents().collect();
        if first_parent {
            parents.truncate(1);
        }
        if parents.len() < 2 {
            let parent = parents.first();
            let (changed, mapped, shown) = process(repo, &commit, parent, &tracked)?;
            if let Some(p) = parent {
                add(&mut pending, p.id(), mapped);
            }
            out.push(changed.then_some(shown));
            continue;
        }
        let mut cands = Vec::new();
        let mut blamed = None;
        for p in &parents {
            let (changed, mapped, _) = process(repo, &commit, Some(p), &tracked)?;
            if !changed {
                // This parent takes all the blame; the others are not followed.
                blamed = Some((p.id(), mapped));
                break;
            }
            cands.push((p.id(), mapped));
        }
        match blamed {
            Some((p, mapped)) => {
                add(&mut pending, p, mapped);
                out.push(None);
            }
            None => {
                for (p, mapped) in cands {
                    add(&mut pending, p, mapped);
                }
                out.push(Some(Vec::new()));
            }
        }
    }
    Ok(out)
}
