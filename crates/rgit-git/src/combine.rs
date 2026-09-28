//! git's combined diff of a merge against all its parents (`-c`, `--cc`),
//! ported from combine-diff.c.

use crate::rev::RevParse;
use git2::{Delta, DiffOptions, Oid, Repository};

use crate::GitError;

/// A path a merge changed against every one of its parents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombinedFile {
    pub path: String,
    /// git's status letter against each parent.
    pub status: Vec<char>,
    /// The file's mode and id in each parent, then in the merge (0 and the
    /// zero id where it is missing).
    pub modes: Vec<u32>,
    pub ids: Vec<Oid>,
    /// The `diff --cc` (or `--combined`) header and hunks, each line
    /// newline-terminated; empty when `--cc` finds nothing worth showing.
    pub patch: String,
}

const CONTEXT: usize = 3;

/// `(path, note)` pairs.
type Notes = Vec<(String, String)>;

struct Lost {
    text: Vec<u8>,
    map: u64,
}

#[derive(Default)]
struct Sline<'a> {
    text: Option<&'a [u8]>,
    flag: u64,
    lost: Vec<Lost>,
    plost: Vec<Vec<u8>>,
    p_lno: Vec<usize>,
}

struct Side {
    oid: Oid,
    mode: u32,
    status: char,
}

/// The combined diff of `commit` (a merge) limited to `paths`; `dense` is
/// `--cc`, which drops hunks where the result matches one parent.
pub(crate) fn combined(
    repo: &Repository,
    commit: &str,
    paths: &[String],
    dense: bool,
) -> Result<Vec<CombinedFile>, GitError> {
    let commit = repo.rev_single(commit)?.peel_to_commit()?;
    let tree = commit.tree()?;
    let mut per_parent: Vec<Vec<(String, Side, Oid, u32)>> = Vec::new();
    for parent in commit.parents() {
        let mut opts = DiffOptions::new();
        crate::pathspec::limit_diff(&mut opts, paths)?;
        let diff = repo.diff_tree_to_tree(Some(&parent.tree()?), Some(&tree), Some(&mut opts))?;
        let mut list = Vec::new();
        for d in diff.deltas() {
            let file = if d.status() == Delta::Deleted {
                d.old_file()
            } else {
                d.new_file()
            };
            let path = String::from_utf8_lossy(file.path_bytes().unwrap_or_default()).into_owned();
            let status = match d.status() {
                Delta::Added => 'A',
                Delta::Deleted => 'D',
                Delta::Typechange => 'T',
                _ => 'M',
            };
            let old = d.old_file();
            let new = d.new_file();
            list.push((
                path,
                Side {
                    oid: old.id(),
                    mode: if old.id().is_zero() {
                        0
                    } else {
                        u32::from(old.mode())
                    },
                    status,
                },
                new.id(),
                if new.id().is_zero() {
                    0
                } else {
                    u32::from(new.mode())
                },
            ));
        }
        per_parent.push(list);
    }
    let Some((first, rest)) = per_parent.split_first() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (path, side, oid, mode) in first {
        let mut sides = vec![side];
        for other in rest {
            match other.iter().find(|(p, ..)| p == path) {
                Some((_, s, ..)) => sides.push(s),
                None => break,
            }
        }
        if sides.len() < per_parent.len() {
            continue;
        }
        let patch = file_patch(repo, path, &sides, *oid, *mode, dense)?;
        out.push(CombinedFile {
            path: path.clone(),
            status: sides.iter().map(|s| s.status).collect(),
            modes: sides.iter().map(|s| s.mode).chain([*mode]).collect(),
            ids: sides.iter().map(|s| s.oid).chain([*oid]).collect(),
            patch,
        });
    }
    Ok(out)
}

fn blob(repo: &Repository, oid: Oid, mode: u32) -> Result<Vec<u8>, GitError> {
    Ok(if oid.is_zero() {
        Vec::new()
    } else if mode == 0o160000 {
        format!("Subproject commit {oid}\n").into_bytes()
    } else {
        repo.find_blob(oid)?.content().to_vec()
    })
}

fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(8000)].contains(&0)
}

fn abbrev(repo: &Repository, oid: Oid) -> Result<String, GitError> {
    crate::plumbing::abbrev(repo, &oid.to_string(), 0)
}

fn file_patch(
    repo: &Repository,
    path: &str,
    parents: &[&Side],
    oid: Oid,
    mode: u32,
    dense: bool,
) -> Result<String, GitError> {
    let n = parents.len();
    let result = blob(repo, oid, mode)?;
    let mode_differs = parents.iter().any(|p| p.mode != mode);
    let mut head = format!(
        "diff --{} {path}\nindex ",
        if dense { "cc" } else { "combined" }
    );
    for (i, p) in parents.iter().enumerate() {
        if i > 0 {
            head.push(',');
        }
        head.push_str(&abbrev(repo, p.oid)?);
    }
    head.push_str(&format!("..{}\n", abbrev(repo, oid)?));
    let deleted = mode == 0;
    let added = !deleted && parents.iter().all(|p| p.status == 'A');
    if mode_differs {
        if added {
            head.push_str(&format!("new file mode {mode:06o}\n"));
        } else {
            if deleted {
                head.push_str("deleted file ");
            }
            head.push_str("mode ");
            let modes: Vec<String> = parents.iter().map(|p| format!("{:06o}", p.mode)).collect();
            head.push_str(&modes.join(","));
            if !deleted {
                head.push_str(&format!("..{mode:06o}"));
            }
            head.push('\n');
        }
    }
    let mut blobs = Vec::new();
    for p in parents {
        blobs.push(blob(repo, p.oid, p.mode)?);
    }
    if is_binary(&result) || blobs.iter().any(|b| is_binary(b)) {
        return Ok(head + "Binary files differ\n");
    }
    let file_head = format!(
        "{}{}",
        if added {
            "--- /dev/null\n".to_owned()
        } else {
            format!("--- a/{path}\n")
        },
        if deleted {
            "+++ /dev/null\n".to_owned()
        } else {
            format!("+++ b/{path}\n")
        }
    );
    let mut lines: Vec<&[u8]> = result.split(|&b| b == b'\n').collect();
    if result.ends_with(b"\n") || result.is_empty() {
        lines.pop();
    }
    let cnt = lines.len();
    let mut sline: Vec<Sline> = (0..cnt + 2)
        .map(|i| Sline {
            text: lines.get(i).copied(),
            p_lno: vec![0; n],
            ..Default::default()
        })
        .collect();
    for i in 0..n {
        match (0..i).find(|&j| parents[j].oid == parents[i].oid) {
            Some(j) => reuse(&mut sline, cnt, i, j),
            None => diff_parent(&mut sline, cnt, i, &blobs[i], &result)?,
        }
    }
    let show = make_hunks(&mut sline, cnt, n, dense);
    if !show && !mode_differs {
        return Ok(String::new());
    }
    let mut out = head + &file_head;
    dump(&sline, cnt, n, &mut out);
    Ok(out)
}

fn diff_parent(
    sline: &mut [Sline],
    cnt: usize,
    n: usize,
    parent: &[u8],
    result: &[u8],
) -> Result<(), GitError> {
    let nmask = 1u64 << n;
    let mut opts = DiffOptions::new();
    opts.context_lines(0).indent_heuristic(true);
    let patch = git2::Patch::from_buffers(parent, None, result, None, Some(&mut opts))?;
    for h in 0..patch.num_hunks() {
        let (hunk, count) = patch.hunk(h)?;
        let mut lno = hunk.new_start() as usize;
        // Lines a hunk removes hang before its first new line, or after
        // line N when it adds none.
        let bucket = if hunk.new_lines() == 0 { lno } else { lno - 1 };
        for l in 0..count {
            let line = patch.line_in_hunk(h, l)?;
            match line.origin() {
                '-' => {
                    let text = line.content();
                    let text = text.strip_suffix(b"\n").unwrap_or(text);
                    sline[bucket].plost.push(text.to_vec());
                }
                '+' => {
                    sline[lno - 1].flag |= nmask;
                    lno += 1;
                }
                _ => {}
            }
        }
    }
    let mut p_lno = 1;
    for (lno, s) in sline.iter_mut().enumerate().take(cnt + 1) {
        s.p_lno[n] = p_lno;
        let plost = std::mem::take(&mut s.plost);
        if !plost.is_empty() {
            s.lost = coalesce(std::mem::take(&mut s.lost), plost, n);
        }
        p_lno += s.lost.iter().filter(|l| l.map & nmask != 0).count();
        if lno < cnt && s.flag & nmask == 0 {
            p_lno += 1;
        }
    }
    sline[cnt + 1].p_lno[n] = p_lno;
    Ok(())
}

/// Merge parent `n`'s lost lines into those of the parents before it along
/// their longest common subsequence, as git's coalesce_lines does.
fn coalesce(base: Vec<Lost>, new: Vec<Vec<u8>>, n: usize) -> Vec<Lost> {
    let bit = 1u64 << n;
    if base.is_empty() {
        return new
            .into_iter()
            .map(|text| Lost { text, map: bit })
            .collect();
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Dir {
        Match,
        Base,
        New,
    }
    let (b, m) = (base.len(), new.len());
    let mut lcs = vec![vec![0usize; m + 1]; b + 1];
    let mut dir = vec![vec![Dir::Base; m + 1]; b + 1];
    for d in dir[0].iter_mut().skip(1) {
        *d = Dir::New;
    }
    for i in 1..=b {
        for j in 1..=m {
            if base[i - 1].text == new[j - 1] {
                lcs[i][j] = lcs[i - 1][j - 1] + 1;
                dir[i][j] = Dir::Match;
            } else if lcs[i][j - 1] >= lcs[i - 1][j] {
                lcs[i][j] = lcs[i][j - 1];
                dir[i][j] = Dir::New;
            } else {
                lcs[i][j] = lcs[i - 1][j];
                dir[i][j] = Dir::Base;
            }
        }
    }
    let mut base: Vec<Option<Lost>> = base.into_iter().map(Some).collect();
    let mut new: Vec<Option<Vec<u8>>> = new.into_iter().map(Some).collect();
    let mut out = Vec::new();
    let (mut i, mut j) = (b, m);
    while i != 0 || j != 0 {
        match dir[i][j] {
            Dir::Match => {
                let mut l = base[i - 1].take().expect("line");
                l.map |= bit;
                out.push(l);
                i -= 1;
                j -= 1;
            }
            Dir::New => {
                let text = new[j - 1].take().expect("line");
                out.push(Lost { text, map: bit });
                j -= 1;
            }
            Dir::Base => {
                out.push(base[i - 1].take().expect("line"));
                i -= 1;
            }
        }
    }
    out.reverse();
    out
}

fn reuse(sline: &mut [Sline], cnt: usize, i: usize, j: usize) {
    let (imask, jmask) = (1u64 << i, 1u64 << j);
    for s in sline.iter_mut().take(cnt + 1) {
        s.p_lno[i] = s.p_lno[j];
        for l in &mut s.lost {
            if l.map & jmask != 0 {
                l.map |= imask;
            }
        }
        if s.flag & jmask != 0 {
            s.flag |= imask;
        }
    }
    sline[cnt + 1].p_lno[i] = sline[cnt + 1].p_lno[j];
}

fn find_next(sline: &[Sline], mark: u64, mut i: usize, cnt: usize, uninteresting: bool) -> usize {
    while i <= cnt {
        if (sline[i].flag & mark == 0) == uninteresting {
            return i;
        }
        i += 1;
    }
    i
}

fn adjust_hunk_tail(sline: &[Sline], all_mask: u64, hunk_begin: usize, i: usize) -> usize {
    if hunk_begin < i && sline[i - 1].flag & all_mask == 0 {
        i - 1
    } else {
        i
    }
}

fn give_context(sline: &mut [Sline], cnt: usize, n: usize) -> bool {
    let all_mask = (1u64 << n) - 1;
    let mark = 1u64 << n;
    let no_pre_delete = 2u64 << n;
    let mut i = find_next(sline, mark, 0, cnt, false);
    if cnt < i {
        return false;
    }
    while i <= cnt {
        let mut j = i.saturating_sub(CONTEXT);
        while j < i {
            if sline[j].flag & mark == 0 {
                sline[j].flag |= no_pre_delete;
            }
            sline[j].flag |= mark;
            j += 1;
        }
        loop {
            let j = find_next(sline, mark, i, cnt, true);
            if cnt < j {
                return true;
            }
            let k = find_next(sline, mark, j, cnt, false);
            let mut j = adjust_hunk_tail(sline, all_mask, i, j);
            if k < j + CONTEXT {
                while j < k {
                    sline[j].flag |= mark;
                    j += 1;
                }
                i = k;
                continue;
            }
            i = k;
            let end = (j + CONTEXT).min(cnt + 1);
            while j < end {
                sline[j].flag |= mark;
                j += 1;
            }
            break;
        }
    }
    true
}

fn make_hunks(sline: &mut [Sline], cnt: usize, n: usize, dense: bool) -> bool {
    let all_mask = (1u64 << n) - 1;
    let mark = 1u64 << n;
    for s in sline.iter_mut().take(cnt + 1) {
        if s.flag & all_mask != 0 || !s.lost.is_empty() {
            s.flag |= mark;
        } else {
            s.flag &= !mark;
        }
    }
    if !dense {
        return give_context(sline, cnt, n);
    }
    let mut i = 0;
    while i < cnt {
        while i < cnt && sline[i].flag & mark == 0 {
            i += 1;
        }
        if cnt <= i {
            break;
        }
        let hunk_begin = i;
        let mut j = i + 1;
        while j < cnt {
            if sline[j].flag & mark == 0 {
                let la = adjust_hunk_tail(sline, all_mask, hunk_begin, j);
                let mut la = if la + CONTEXT < cnt {
                    la + CONTEXT
                } else {
                    cnt
                };
                let mut contin = false;
                while la > 0 {
                    la -= 1;
                    if j > la {
                        break;
                    }
                    if sline[la].flag & mark != 0 {
                        contin = true;
                        break;
                    }
                }
                if !contin {
                    break;
                }
                j = la;
            }
            j += 1;
        }
        let hunk_end = j;
        let mut same_diff = 0u64;
        let mut interesting = false;
        'scan: for s in &sline[i..hunk_end] {
            let this = s.flag & all_mask;
            if this != 0 {
                if same_diff == 0 {
                    same_diff = this;
                } else if same_diff != this {
                    interesting = true;
                    break;
                }
            }
            for l in &s.lost {
                if same_diff == 0 {
                    same_diff = l.map;
                } else if same_diff != l.map {
                    interesting = true;
                    break 'scan;
                }
            }
        }
        if !interesting && same_diff != all_mask {
            for s in &mut sline[hunk_begin..hunk_end] {
                s.flag &= !mark;
            }
        }
        i = hunk_end;
    }
    give_context(sline, cnt, n)
}

fn dump(sline: &[Sline], cnt: usize, n: usize, out: &mut String) {
    let mark = 1u64 << n;
    let no_pre_delete = 2u64 << n;
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let mut lno = 0;
    loop {
        let mut comment: Option<&[u8]> = None;
        while lno <= cnt && sline[lno].flag & mark == 0 {
            if let Some(t) = sline[lno].text
                && t.first()
                    .is_some_and(|&c| c.is_ascii_alphabetic() || c == b'_' || c == b'$')
            {
                comment = Some(t);
            }
            lno += 1;
        }
        if cnt < lno {
            break;
        }
        let mut hunk_end = lno + 1;
        while hunk_end <= cnt && sline[hunk_end].flag & mark != 0 {
            hunk_end += 1;
        }
        let mut rlines = hunk_end - lno;
        if cnt < hunk_end {
            rlines -= 1;
        }
        let at = "@".repeat(n + 1);
        out.push_str(&at);
        for i in 0..n {
            let (l0, l1) = (sline[lno].p_lno[i], sline[hunk_end].p_lno[i]);
            out.push_str(&format!(" -{l0},{}", l1 - l0));
        }
        out.push_str(&format!(" +{},{rlines} {at}", lno + 1));
        if let Some(c) = comment {
            // git scans 40 bytes and prints up to (not including) the last
            // non-space one it saw.
            let mut end = 0;
            for (i, &ch) in c.iter().take(40).enumerate() {
                if ch == b'\n' {
                    break;
                }
                if !ch.is_ascii_whitespace() {
                    end = i;
                }
            }
            if end > 0 {
                out.push(' ');
                out.push_str(&text(&c[..end]));
            }
        }
        out.push('\n');
        while lno < hunk_end {
            let sl = &sline[lno];
            lno += 1;
            if sl.flag & no_pre_delete == 0 {
                for l in &sl.lost {
                    for j in 0..n {
                        out.push(if l.map & (1 << j) != 0 { '-' } else { ' ' });
                    }
                    out.push_str(&text(&l.text));
                    out.push('\n');
                }
            }
            if cnt < lno {
                break;
            }
            for j in 0..n {
                out.push(if sl.flag & (1 << j) != 0 { '+' } else { ' ' });
            }
            out.push_str(&text(sl.text.unwrap_or_default()));
            out.push('\n');
        }
    }
}

/// A fresh merge of `commit`'s two parents written as a tree, conflicted
/// files with their markers, and git's `remerge CONFLICT` note per path; None
/// for a commit that is not a two-parent merge.
pub(crate) fn remerge_tree(
    repo: &Repository,
    commit: &git2::Commit,
) -> Result<Option<(Oid, Notes)>, GitError> {
    if commit.parent_count() != 2 {
        return Ok(None);
    }
    let (ours, theirs) = (commit.parent(0)?, commit.parent(1)?);
    let label = |c: &git2::Commit| -> Result<String, GitError> {
        Ok(format!(
            "{} ({})",
            abbrev(repo, c.id())?,
            c.summary().ok().flatten().unwrap_or_default()
        ))
    };
    let (our_label, their_label) = (label(&ours)?, label(&theirs)?);
    let mut index = repo.merge_commits(&ours, &theirs, None)?;
    let conflicts: Vec<_> = index.conflicts()?.collect::<Result<_, _>>()?;
    let content = |e: Option<&git2::IndexEntry>| -> Result<Vec<u8>, GitError> {
        Ok(match e {
            Some(e) => repo.find_blob(e.id)?.content().to_vec(),
            None => Vec::new(),
        })
    };
    let mut notes = Vec::new();
    for c in conflicts {
        let (base, a, b) = (
            content(c.ancestor.as_ref())?,
            content(c.our.as_ref())?,
            content(c.their.as_ref())?,
        );
        let both = c.our.is_some() && c.their.is_some();
        let kind = if c.ancestor.is_some() {
            "content"
        } else {
            "add/add"
        };
        let ours_kept = c.our.is_some();
        let Some(mut entry) = c.our.or(c.their) else {
            continue;
        };
        let path = String::from_utf8_lossy(&entry.path).into_owned();
        index.remove_path(std::path::Path::new(&path))?;
        entry.flags &= !0x3000;
        let note = if both {
            fn input(data: &[u8]) -> git2::MergeFileInput<'_> {
                let mut i = git2::MergeFileInput::new();
                i.content(data);
                i
            }
            let (ib, ia, ic) = (input(&base), input(&a), input(&b));
            let mut o = git2::MergeFileOptions::new();
            o.our_label(&our_label).their_label(&their_label);
            let merged = git2::merge_file(&ib, &ia, &ic, Some(&mut o))?;
            entry.id = repo.blob(merged.content())?;
            entry.file_size = merged.content().len() as u32;
            format!("remerge CONFLICT ({kind}): Merge conflict in {path}")
        } else {
            let (gone, kept) = if ours_kept {
                (&their_label, &our_label)
            } else {
                (&our_label, &their_label)
            };
            format!(
                "remerge CONFLICT (modify/delete): {path} deleted in {gone} and modified in {kept}.  Version {kept} of {path} left in tree."
            )
        };
        index.add(&entry)?;
        notes.push((path, note));
    }
    Ok(Some((index.write_tree_to(repo)?, notes)))
}
