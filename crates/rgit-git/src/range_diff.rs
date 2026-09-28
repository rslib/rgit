//! `git range-diff`: pair the commits of two versions of a series and show
//! how each pair's patch changed, in git's layout.

use std::fmt::Write as _;

use git2::{Commit, DiffFindOptions, Oid, Repository};

use crate::GitError;

/// How to compare two ranges.
#[derive(Debug, Clone)]
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
}

struct Entry {
    id: Oid,
    subject: String,
    /// The whole patch as range-diff compares it.
    patch: String,
    /// Where its diff part starts in `patch`.
    diff_offset: usize,
    diff_size: usize,
    matching: Option<usize>,
    shown: bool,
}

/// The non-merge commits of `range` (`a..b`), oldest first, as entries.
fn read_patches(repo: &Repository, range: &str) -> Result<Vec<Entry>, GitError> {
    let mut walk = repo.revwalk()?;
    walk.push_range(range)?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
    let mut out = Vec::new();
    for id in walk {
        let c = repo.find_commit(id?)?;
        if c.parent_count() > 1 {
            continue;
        }
        out.push(entry(repo, &c)?);
    }
    Ok(out)
}

/// A commit as range-diff's text: metadata, message, then each file's hunks
/// under ` ## <file> ##`, without line numbers.
fn entry(repo: &Repository, c: &Commit) -> Result<Entry, GitError> {
    let author = c.author();
    let mut patch = format!(
        " ## Metadata ##\nAuthor: {} <{}>\n\n ## Commit message ##\n",
        String::from_utf8_lossy(author.name_bytes()),
        String::from_utf8_lossy(author.email_bytes())
    );
    let msg = String::from_utf8_lossy(c.message_bytes()).into_owned();
    for line in msg.trim_end().lines() {
        let _ = writeln!(patch, "{}", format!("    {line}").trim_end());
    }
    let mut diff_offset = None;
    let parent = match c.parent_count() {
        0 => None,
        _ => Some(c.parent(0)?.tree()?),
    };
    let mut diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&c.tree()?), None)?;
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
            let a = if old.is_empty() {
                "/dev/null".to_owned()
            } else {
                old.clone()
            };
            let b = if new.is_empty() {
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
            let _ = writeln!(patch, "@@{func}");
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
        diff_offset: diff_offset.unwrap_or(patch.len()),
        patch,
        diff_size,
        matching: None,
        shown: false,
    })
}

/// How many lines a context-free diff between `a` and `b` has, hunk headers
/// included, as git's range-diff counts its cost.
fn diff_size(a: &str, b: &str) -> Result<usize, GitError> {
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(0);
    let p = git2::Patch::from_buffers(a.as_bytes(), None, b.as_bytes(), None, Some(&mut opts))?;
    let (_, add, del) = p.line_stats()?;
    Ok(add + del + p.num_hunks())
}

/// `git range-diff` of two ranges as git prints it.
pub(crate) fn range_diff(repo: &Repository, o: &RangeDiffOpts) -> Result<String, GitError> {
    let mut a = read_patches(repo, &o.range1)?;
    let mut b = read_patches(repo, &o.range2)?;
    // Identical diffs pair first.
    for (i, ea) in a.iter_mut().enumerate() {
        let diff = &ea.patch[ea.diff_offset..];
        if let Some(eb) = b
            .iter_mut()
            .find(|eb| eb.matching.is_none() && eb.patch[eb.diff_offset..] == *diff)
        {
            eb.matching = Some(i);
        }
    }
    for (j, eb) in b.iter().enumerate() {
        if let Some(i) = eb.matching {
            a[i].matching = Some(j);
        }
    }
    // ponytail: greedy cheapest-first pairing instead of git's Hungarian
    // assignment; the two differ only when several near-equal pairings compete.
    let mut costs = Vec::new();
    for (i, ea) in a.iter().enumerate().filter(|(_, e)| e.matching.is_none()) {
        for (j, eb) in b.iter().enumerate().filter(|(_, e)| e.matching.is_none()) {
            let cost = diff_size(&ea.patch[ea.diff_offset..], &eb.patch[eb.diff_offset..])?;
            let limit =
                ea.diff_size * o.creation_factor / 100 + eb.diff_size * o.creation_factor / 100;
            if cost < limit {
                costs.push((cost, i, j));
            }
        }
    }
    costs.sort();
    for (_, i, j) in costs {
        if a[i].matching.is_none() && b[j].matching.is_none() {
            a[i].matching = Some(j);
            b[j].matching = Some(i);
        }
    }
    let width = a.len().max(b.len()).to_string().len();
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
                out.push_str(&inner_diff(&a[ai].patch, &b[j].patch)?);
            }
            a[ai].shown = true;
            j += 1;
        }
    }
    Ok(out)
}

/// The diff of two patches, indented, with each hunk named by its section.
fn inner_diff(a: &str, b: &str) -> Result<String, GitError> {
    let p = git2::Patch::from_buffers(a.as_bytes(), None, b.as_bytes(), None, None)?;
    let old: Vec<&str> = a.lines().collect();
    let mut out = String::new();
    for h in 0..p.num_hunks() {
        let (hunk, lines) = p.hunk(h)?;
        let start = hunk.old_start() as usize;
        let section = old[..start.saturating_sub(1).min(old.len())]
            .iter()
            .rev()
            .find_map(|l| l.strip_prefix(" ## ").and_then(|l| l.strip_suffix(" ##")));
        match section {
            Some(s) => {
                let _ = writeln!(out, "    @@ {s}");
            }
            None => out.push_str("    @@\n"),
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
