//! `git range-diff`: pair the commits of two versions of a series and show
//! how each pair's patch changed, in git's layout.

use std::fmt::Write as _;

use git2::{Commit, DiffFindOptions, Oid, Repository};

use crate::GitError;

/// How to compare two ranges.
#[derive(Debug, Clone, Default)]
pub struct RangeDiffOpts {
    pub range1: String,
    pub range2: String,
    /// Percent of a patch's size a change may cost and still pair (git's
    /// default 60).
    pub creation_factor: usize,
    /// Show the diffs of changed pairs.
    pub patches: bool,
    pub left_only: bool,
    pub right_only: bool,
    /// Context lines of the diffs between pairs (default 3).
    pub context: Option<u32>,
    /// The notes refs whose notes are part of each patch; None is log's
    /// default (core.notesRef, then notes.displayRef).
    pub notes: Option<Vec<String>>,
    /// Only commits touching these paths, and only their diffs there.
    pub paths: Vec<String>,
}

struct Entry {
    id: Oid,
    subject: String,
    /// The whole patch as range-diff compares it.
    patch: String,
    /// Where its diff part starts in `patch` (0 without a diff, as in git).
    diff_offset: usize,
    diff_size: usize,
    matching: Option<usize>,
    shown: bool,
}

impl Entry {
    fn diff(&self) -> &str {
        &self.patch[self.diff_offset..]
    }
}

/// The notes refs `git log` shows by default: core.notesRef (or
/// refs/notes/commits), then notes.displayRef's matches.
fn default_notes(repo: &Repository) -> Vec<String> {
    let mut refs = vec![
        std::env::var("GIT_NOTES_REF")
            .ok()
            .or_else(|| repo.note_default_ref().ok())
            .unwrap_or_else(|| "refs/notes/commits".to_owned()),
    ];
    let globs: Vec<String> = repo
        .config()
        .and_then(|c| {
            let mut v = Vec::new();
            c.multivar("notes.displayRef", None)?.for_each(|e| {
                if let Ok(s) = e.value() {
                    v.push(s.to_owned());
                }
            })?;
            Ok(v)
        })
        .unwrap_or_default();
    for g in globs {
        let g = expand_notes_ref(&g);
        if let Ok(names) = repo.references_glob(&g) {
            for r in names.flatten() {
                if let Ok(n) = r.name().map(str::to_owned)
                    && !refs.contains(&n)
                {
                    refs.push(n);
                }
            }
        }
    }
    refs
}

/// A notes ref as git expands `--notes=<ref>`: `foo` is refs/notes/foo.
fn expand_notes_ref(name: &str) -> String {
    if name.starts_with("refs/notes/") {
        name.to_owned()
    } else if let Some(rest) = name.strip_prefix("notes/") {
        format!("refs/notes/{rest}")
    } else {
        format!("refs/notes/{name}")
    }
}

/// The non-merge commits of `range` (`a..b`), oldest first, as entries.
fn read_patches(
    repo: &Repository,
    range: &str,
    notes: &[String],
    paths: &[String],
) -> Result<Vec<Entry>, GitError> {
    let mut walk = repo.revwalk()?;
    walk.push_range(range)?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME | git2::Sort::REVERSE)?;
    let mailmap = repo.mailmap().ok();
    let mut out = Vec::new();
    for id in walk {
        let c = repo.find_commit(id?)?;
        if c.parent_count() > 1 {
            continue;
        }
        let e = entry(repo, &c, mailmap.as_ref(), notes, paths)?;
        // Like `git log -- <paths>`, commits that touch none of them drop out.
        if paths.is_empty() || e.diff_size > 0 {
            out.push(e);
        }
    }
    Ok(out)
}

/// `line` with tabs expanded to 8 columns, as `git log` prints messages.
fn expand_tabs(line: &str) -> String {
    let mut out = String::new();
    let mut col = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let n = 8 - col % 8;
            out.push_str(&" ".repeat(n));
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// A commit as range-diff's text: metadata, message, notes, then each file's
/// hunks under ` ## <file> ##`, without line numbers.
fn entry(
    repo: &Repository,
    c: &Commit,
    mailmap: Option<&git2::Mailmap>,
    notes: &[String],
    paths: &[String],
) -> Result<Entry, GitError> {
    let author = match mailmap {
        Some(m) => c.author_with_mailmap(m)?,
        None => c.author(),
    };
    let mut patch = format!(
        " ## Metadata ##\nAuthor: {} <{}>\n\n ## Commit message ##\n",
        String::from_utf8_lossy(author.name_bytes()),
        String::from_utf8_lossy(author.email_bytes())
    );
    let msg = String::from_utf8_lossy(c.message_bytes()).into_owned();
    let msg = msg.strip_suffix('\n').unwrap_or(&msg);
    for line in msg
        .split('\n')
        .skip_while(|l| l.trim().is_empty())
        .map(expand_tabs)
    {
        let _ = writeln!(patch, "{}", format!("    {line}").trim_end());
    }
    for r in notes {
        let Ok(note) = repo.find_note(Some(r), c.id()) else {
            continue;
        };
        let text = String::from_utf8_lossy(note.message_bytes()).into_owned();
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let name = match r.as_str() {
            "refs/notes/commits" => "Notes".to_owned(),
            r => format!("Notes ({})", r.strip_prefix("refs/notes/").unwrap_or(r)),
        };
        let _ = write!(patch, "\n\n ## {name} ##\n");
        for line in text.split('\n') {
            let _ = writeln!(patch, "{}", format!("    {line}").trim_end());
        }
    }
    let mut diff_offset = None;
    let parent = match c.parent_count() {
        0 => None,
        _ => Some(c.parent(0)?.tree()?),
    };
    let mut limit = git2::DiffOptions::new();
    for p in paths {
        limit.pathspec(p);
    }
    let mut diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&c.tree()?), Some(&mut limit))?;
    diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
    let mut diff_size = 0;
    for i in 0..diff.deltas().len() {
        let Some(p) = git2::Patch::from_diff(&diff, i)? else {
            continue;
        };
        let d = p.delta();
        let path = |f: git2::DiffFile| f.path().map_or(String::new(), |p| p.display().to_string());
        let (old, new) = (path(d.old_file()), path(d.new_file()));
        let mut name = match d.status() {
            git2::Delta::Added => format!("{new} (new)"),
            git2::Delta::Deleted => format!("{old} (deleted)"),
            git2::Delta::Renamed => format!("{old} => {new}"),
            _ => new.clone(),
        };
        let (om, nm) = (
            u32::from(d.old_file().mode()),
            u32::from(d.new_file().mode()),
        );
        if om != 0 && nm != 0 && om != nm {
            let _ = write!(name, " (mode change {om:06o} => {nm:06o})");
        }
        patch.push('\n');
        diff_offset.get_or_insert(patch.len());
        let _ = writeln!(patch, " ## {name} ##");
        diff_size += 1;
        if d.flags().is_binary() {
            let a = if old.is_empty() || d.status() == git2::Delta::Added {
                "/dev/null".to_owned()
            } else {
                old.clone()
            };
            let b = if new.is_empty() || d.status() == git2::Delta::Deleted {
                "/dev/null".to_owned()
            } else {
                new.clone()
            };
            let _ = writeln!(patch, " Binary files {a} and {b} differ");
            diff_size += 1;
            continue;
        }
        for h in 0..p.num_hunks() {
            let (hunk, lines) = p.hunk(h)?;
            let header = String::from_utf8_lossy(hunk.header()).into_owned();
            let func = header
                .trim_end()
                .splitn(3, "@@")
                .nth(2)
                .unwrap_or("")
                .to_owned();
            if func.is_empty() {
                patch.push_str("@@\n");
            } else {
                let file = if d.status() == git2::Delta::Deleted {
                    &old
                } else {
                    &new
                };
                let _ = writeln!(patch, "@@ {file}:{func}");
            }
            diff_size += 1;
            for l in 0..lines {
                let line = p.line_in_hunk(h, l)?;
                let content = String::from_utf8_lossy(line.content());
                let content = content.trim_end_matches('\n');
                match line.origin() {
                    o @ ('+' | '-' | ' ') => {
                        let _ = writeln!(patch, "{o}{content}");
                    }
                    _ => {
                        let _ = writeln!(patch, " {}", content.trim_start_matches('\n'));
                    }
                }
                diff_size += 1;
            }
        }
    }
    Ok(Entry {
        id: c.id(),
        subject: c.summary().ok().flatten().unwrap_or("").to_owned(),
        diff_offset: diff_offset.unwrap_or(0),
        patch,
        diff_size,
        matching: None,
        shown: false,
    })
}

/// git's range-diff cost of turning `a` into `b`: the lines and hunk headers
/// of their diff with 3 lines of context.
fn diff_size(a: &str, b: &str) -> Result<i32, GitError> {
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(3);
    let p = git2::Patch::from_buffers(a.as_bytes(), None, b.as_bytes(), None, Some(&mut opts))?;
    let (ctx, add, del) = p.line_stats()?;
    Ok((ctx + add + del + p.num_hunks()) as i32)
}

const COST_MAX: i32 = 1 << 16;

/// The cheapest assignment of columns to rows over `cost` (column-major:
/// `cost[column + n * row]`), a port of git's linear-assignment.c
/// (Jonker-Volgenant) so ties break exactly as git's do.
#[allow(
    clippy::many_single_char_names,
    clippy::needless_range_loop,
    clippy::mut_range_bound
)]
fn compute_assignment(n: usize, cost: &[i32]) -> Vec<i32> {
    let c = |col: usize, row: usize| cost[col + n * row];
    let mut column2row = vec![-1i32; n];
    let mut row2column = vec![-1i32; n];
    if n < 2 {
        return vec![0; n];
    }
    let mut v = vec![0i32; n];
    // Column reduction.
    for j in (0..n).rev() {
        let mut i1 = 0;
        for i in 1..n {
            if c(j, i1) > c(j, i) {
                i1 = i;
            }
        }
        v[j] = c(j, i1);
        if row2column[i1] == -1 {
            row2column[i1] = j as i32;
            column2row[j] = i1 as i32;
        } else {
            if row2column[i1] >= 0 {
                row2column[i1] = -2 - row2column[i1];
            }
            column2row[j] = -1;
        }
    }
    // Reduction transfer.
    let mut free_row = vec![0usize; n];
    let mut free_count = 0;
    for i in 0..n {
        let j1 = row2column[i];
        if j1 == -1 {
            free_row[free_count] = i;
            free_count += 1;
        } else if j1 < -1 {
            row2column[i] = -2 - j1;
        } else {
            let j1 = j1 as usize;
            let not = usize::from(j1 == 0);
            let mut min = c(not, i) - v[not];
            for j in 1..n {
                if j != j1 && min > c(j, i) - v[j] {
                    min = c(j, i) - v[j];
                }
            }
            v[j1] -= min;
        }
    }
    if free_count == 0 {
        return column2row;
    }
    // Augmenting row reduction.
    for _ in 0..2 {
        let mut k = 0;
        let saved = free_count;
        free_count = 0;
        while k < saved {
            let i = free_row[k];
            k += 1;
            let mut j1 = 0usize;
            let mut u1 = c(j1, i) - v[j1];
            let mut j2: i32 = -1;
            let mut u2 = i32::MAX;
            for j in 1..n {
                let cc = c(j, i) - v[j];
                if u2 > cc {
                    if u1 < cc {
                        u2 = cc;
                        j2 = j as i32;
                    } else {
                        u2 = u1;
                        u1 = cc;
                        j2 = j1 as i32;
                        j1 = j;
                    }
                }
            }
            if j2 < 0 {
                j2 = j1 as i32;
                u2 = u1;
            }
            let mut i0 = column2row[j1];
            if u1 < u2 {
                v[j1] -= u2 - u1;
            } else if i0 >= 0 {
                j1 = j2 as usize;
                i0 = column2row[j1];
            }
            if i0 >= 0 {
                if u1 < u2 {
                    k -= 1;
                    free_row[k] = i0 as usize;
                } else {
                    free_row[free_count] = i0 as usize;
                    free_count += 1;
                }
            }
            row2column[i] = j1 as i32;
            column2row[j1] = i as i32;
        }
    }
    // Augmentation.
    let saved = free_count;
    let mut d = vec![0i32; n];
    let mut pred = vec![0usize; n];
    let mut col = vec![0usize; n];
    for f in 0..saved {
        let i1 = free_row[f];
        let (mut low, mut up) = (0usize, 0usize);
        for j in 0..n {
            d[j] = c(j, i1) - v[j];
            pred[j] = i1;
            col[j] = j;
        }
        // Like git's, a free column found among the minima leaves `j` at the
        // last column scanned rather than that free one.
        let mut j: Option<usize> = None;
        let last;
        let min;
        'search: loop {
            let this_last = low;
            let mut this_min = d[col[up]];
            up += 1;
            for k in up..n {
                let cj = col[k];
                j = Some(cj);
                let cc = d[cj];
                if cc <= this_min {
                    if cc < this_min {
                        up = low;
                        this_min = cc;
                    }
                    col[k] = col[up];
                    col[up] = cj;
                    up += 1;
                }
            }
            if col[low..up].iter().any(|&cj| column2row[cj] == -1) {
                last = this_last;
                min = this_min;
                break 'search;
            }
            // Scan a row.
            loop {
                let j1 = col[low];
                low += 1;
                let i = column2row[j1] as usize;
                let u1 = c(j1, i) - v[j1] - this_min;
                for k in up..n {
                    let cj = col[k];
                    j = Some(cj);
                    let cc = c(cj, i) - v[cj] - u1;
                    if cc < d[cj] {
                        d[cj] = cc;
                        pred[cj] = i;
                        if cc == this_min {
                            if column2row[cj] == -1 {
                                last = this_last;
                                min = this_min;
                                break 'search;
                            }
                            col[k] = col[up];
                            col[up] = cj;
                            up += 1;
                        }
                    }
                }
                if low == up {
                    break;
                }
            }
        }
        for &j1 in &col[..last] {
            v[j1] += d[j1] - min;
        }
        let Some(mut j) = j else {
            continue;
        };
        loop {
            let i = pred[j];
            column2row[j] = i as i32;
            let next = row2column[i];
            row2column[i] = j as i32;
            if i == i1 {
                break;
            }
            j = next as usize;
        }
    }
    column2row
}

/// Pair the entries of `a` and `b` as git's get_correspondences does.
fn correspondences(a: &mut [Entry], b: &mut [Entry], factor: usize) -> Result<(), GitError> {
    let n = a.len() + b.len();
    let mut cost = vec![0i32; n * n];
    let creation = |e: &Entry| {
        if e.matching.is_none() {
            (e.diff_size * factor / 100) as i32
        } else {
            COST_MAX
        }
    };
    for (i, ea) in a.iter().enumerate() {
        for (j, eb) in b.iter().enumerate() {
            cost[i + n * j] = if ea.matching == Some(j) {
                0
            } else if ea.matching.is_none() && eb.matching.is_none() {
                diff_size(ea.diff(), eb.diff())?
            } else {
                COST_MAX
            };
        }
        let c = creation(ea);
        for j in b.len()..n {
            cost[i + n * j] = c;
        }
    }
    for (j, eb) in b.iter().enumerate() {
        let c = creation(eb);
        for i in a.len()..n {
            cost[i + n * j] = c;
        }
    }
    let a2b = compute_assignment(n, &cost);
    for i in 0..a.len() {
        if let Ok(j) = usize::try_from(a2b[i])
            && j < b.len()
        {
            a[i].matching = Some(j);
            b[j].matching = Some(i);
        }
    }
    Ok(())
}

/// `git range-diff` of two ranges as git prints it.
pub(crate) fn range_diff(repo: &Repository, o: &RangeDiffOpts) -> Result<String, GitError> {
    let notes: Vec<String> = match &o.notes {
        None => default_notes(repo),
        Some(refs) => refs
            .iter()
            .map(|r| match r.as_str() {
                "" => default_notes(repo).swap_remove(0),
                r => expand_notes_ref(r),
            })
            .collect(),
    };
    let mut a = read_patches(repo, &o.range1, &notes, &o.paths)?;
    let mut b = read_patches(repo, &o.range2, &notes, &o.paths)?;
    // Identical diffs pair first; like git's hash map, each patch of the new
    // range takes the last unpaired equal one of the old.
    for (j, eb) in b.iter_mut().enumerate() {
        if let Some(i) = (0..a.len())
            .rev()
            .find(|&i| a[i].matching.is_none() && a[i].diff() == eb.diff())
        {
            a[i].matching = Some(j);
            eb.matching = Some(i);
        }
    }
    correspondences(&mut a, &mut b, o.creation_factor)?;
    let width = (1 + a.len().max(b.len())).to_string().len();
    let abbrev = |id: Oid| {
        repo.find_object(id, None)
            .and_then(|o| o.short_id())
            .ok()
            .and_then(|s| s.as_str().ok().map(str::to_owned))
            .unwrap_or_else(|| id.to_string()[..7].to_owned())
    };
    let dashes = "-".repeat(abbrev(a.first().or(b.first()).map_or(Oid::ZERO_SHA1, |e| e.id)).len());
    let mut out = String::new();
    let header =
        |ai: Option<usize>, bi: Option<usize>, a: &[Entry], b: &[Entry], out: &mut String| {
            let left = ai.map_or(format!("{:>width$}:  {dashes}", "-"), |i| {
                format!("{:>width$}:  {}", i + 1, abbrev(a[i].id))
            });
            let right = bi.map_or(format!("{:>width$}:  {dashes}", "-"), |j| {
                format!("{:>width$}:  {}", j + 1, abbrev(b[j].id))
            });
            let status = match (ai, bi) {
                (Some(_), None) => '<',
                (None, Some(_)) => '>',
                (Some(i), Some(j)) if a[i].patch == b[j].patch => '=',
                _ => '!',
            };
            // git names the pair by its old commit when there is one.
            let subject = ai.map_or(&b[bi.unwrap_or(0)].subject, |i| &a[i].subject);
            let _ = writeln!(out, "{left} {status} {right} {subject}");
        };
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        while i < a.len() && a[i].shown {
            i += 1;
        }
        if i < a.len() && a[i].matching.is_none() {
            if !o.right_only {
                header(Some(i), None, &a, &b, &mut out);
            }
            i += 1;
            continue;
        }
        while j < b.len() && b[j].matching.is_none() {
            if !o.left_only {
                header(None, Some(j), &a, &b, &mut out);
            }
            j += 1;
        }
        if j < b.len() {
            let ai = b[j].matching.expect("matched");
            header(Some(ai), Some(j), &a, &b, &mut out);
            if o.patches && a[ai].patch != b[j].patch {
                out.push_str(&inner_diff(
                    &a[ai].patch,
                    &b[j].patch,
                    o.context.unwrap_or(3),
                )?);
            }
            a[ai].shown = true;
            j += 1;
        }
    }
    Ok(out)
}

/// The name range-diff's hunk headers take from `line`, as git's section
/// driver (`^ ## (.*) ##$` or `^.?@@ (.*)$`) finds it.
fn section(line: &str) -> Option<&str> {
    let found = match line
        .strip_prefix(" ## ")
        .and_then(|l| l.strip_suffix(" ##"))
    {
        Some(s) => s,
        None => line.strip_prefix("@@ ").or_else(|| {
            line.get(line.chars().next()?.len_utf8()..)?
                .strip_prefix("@@ ")
        })?,
    };
    Some(found.trim_end())
}

/// The diff of two patches, indented, with each hunk named by its section.
fn inner_diff(a: &str, b: &str, context: u32) -> Result<String, GitError> {
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(context);
    let p = git2::Patch::from_buffers(a.as_bytes(), None, b.as_bytes(), None, Some(&mut opts))?;
    let old: Vec<&str> = a.lines().collect();
    let mut out = String::new();
    for h in 0..p.num_hunks() {
        let (hunk, lines) = p.hunk(h)?;
        let start = hunk.old_start() as usize;
        let start = if hunk.old_lines() == 0 {
            start + 1
        } else {
            start
        };
        match old[..start.saturating_sub(1).min(old.len())]
            .iter()
            .rev()
            .find_map(|l| section(l))
        {
            Some(s) if !s.is_empty() => {
                let _ = writeln!(out, "    @@ {s}");
            }
            _ => out.push_str("    @@\n"),
        }
        for l in 0..lines {
            let line = p.line_in_hunk(h, l)?;
            let content = String::from_utf8_lossy(line.content());
            if let o @ ('+' | '-' | ' ') = line.origin() {
                let _ = writeln!(out, "    {o}{}", content.trim_end_matches('\n'));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_prefers_the_cheapest_pairs() {
        // Two old commits and two new: old 0 is closer to new 1.
        let n = 4;
        let big = COST_MAX;
        let mut cost = vec![0; n * n];
        let set = |cost: &mut Vec<i32>, i: usize, j: usize, c: i32| cost[i + n * j] = c;
        set(&mut cost, 0, 0, 9);
        set(&mut cost, 0, 1, 1);
        set(&mut cost, 1, 0, 2);
        set(&mut cost, 1, 1, 9);
        for (i, j) in [
            (0, 2),
            (0, 3),
            (1, 2),
            (1, 3),
            (2, 0),
            (3, 0),
            (2, 1),
            (3, 1),
        ] {
            set(&mut cost, i, j, big);
        }
        let a2b = compute_assignment(n, &cost);
        assert_eq!(&a2b[..2], &[1, 0]);
    }

    #[test]
    fn sections_follow_git_driver() {
        assert_eq!(section(" ## a.txt ##"), Some("a.txt"));
        assert_eq!(section("@@ fn main()"), Some("fn main()"));
        assert_eq!(section("+@@ x "), Some("x"));
        assert_eq!(section("@@"), None);
    }
}
