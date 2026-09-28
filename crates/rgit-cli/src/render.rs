//! Rendering of backend results, shared by the CLI and the MCP server. Output
//! is compact and one item per line so it stays easy to read and to parse. On a
//! real terminal it is colored; piped or under MCP it is plain, so agents and
//! scripts get clean text.

use std::sync::atomic::{AtomicBool, Ordering};

use rgit_git::{
    BlameLine, Blob, CommitDetails, CommitRef, Deco, FileDiff, GrepMatch, LogEntry, RefEntry,
    Remote, RepoStatus, SmartlogEntry, Stash, StatusEntry, TreeEntry, Worktree, group_decorations,
};

static COLOR: AtomicBool = AtomicBool::new(false);

/// Enable or disable ANSI color (set from whether stdout is a terminal).
pub fn set_color(on: bool) {
    COLOR.store(on, Ordering::Relaxed);
}

pub fn color_on() -> bool {
    COLOR.load(Ordering::Relaxed)
}

/// Wrap `s` in an SGR code when color is on, otherwise return it unchanged.
fn paint(s: &str, code: &str) -> String {
    if !s.is_empty() && COLOR.load(Ordering::Relaxed) {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

const GREEN: &str = "32";
const RED: &str = "31";
const YELLOW: &str = "33";
const CYAN: &str = "36";
const DIM: &str = "2";
const BOLD: &str = "1";
const MAGENTA: &str = "35";

/// Working-tree status: a branch line, then changed paths grouped by state, one
/// per line with a colored status code.
pub fn status(s: &RepoStatus) -> String {
    let mut out = paint(&s.head.describe(), BOLD);
    if s.head.ahead > 0 {
        out.push_str(&paint(&format!(" +{}", s.head.ahead), GREEN));
    }
    if s.head.behind > 0 {
        out.push_str(&paint(&format!(" -{}", s.head.behind), RED));
    }
    if let Some(r) = &s.rebase {
        out.push_str(&rebase_progress(r));
    }

    let staged: Vec<&StatusEntry> = s.entries.iter().filter(|e| e.is_staged()).collect();
    let unstaged: Vec<&StatusEntry> = s
        .entries
        .iter()
        .filter(|e| e.is_unstaged() && !e.is_untracked())
        .collect();
    let untracked: Vec<&StatusEntry> = s.entries.iter().filter(|e| e.is_untracked()).collect();

    if staged.is_empty() && unstaged.is_empty() && untracked.is_empty() {
        out.push_str(&paint("\nclean", DIM));
        return out;
    }
    group(&mut out, "Staged", &staged, GREEN, |e| e.index.letter());
    group(&mut out, "Unstaged", &unstaged, YELLOW, |e| {
        e.worktree.letter()
    });
    group(&mut out, "Untracked", &untracked, RED, |_| "?");
    out
}

/// A stopped rebase as `git status` words it: the last commands done, the
/// next ones to do, and what is rebased onto what.
pub fn rebase_progress(r: &rgit_git::RebaseProgress) -> String {
    let plural = |n: usize, one: &'static str, many: &'static str| if n == 1 { one } else { many };
    let mut out = format!("\ninteractive rebase in progress; onto {}", r.onto);
    if !r.done.is_empty() {
        let n = r.done.len();
        out.push_str(&format!(
            "\n{} ({n} {} done):",
            plural(n, "Last command done", "Last commands done"),
            plural(n, "command", "commands")
        ));
        for line in &r.done[n.saturating_sub(2)..] {
            out.push_str(&format!("\n   {line}"));
        }
    }
    if r.todo.is_empty() {
        out.push_str("\nNo commands remaining.");
    } else {
        let n = r.todo.len();
        out.push_str(&format!(
            "\n{} ({n} remaining {}):",
            plural(n, "Next command to do", "Next commands to do"),
            plural(n, "command", "commands")
        ));
        for line in r.todo.iter().take(2) {
            out.push_str(&format!("\n   {line}"));
        }
    }
    match &r.branch {
        Some(b) => out.push_str(&format!(
            "\nYou are currently rebasing branch '{b}' on '{}'.",
            r.onto
        )),
        None => out.push_str("\nYou are currently rebasing."),
    }
    out
}

fn group(
    out: &mut String,
    label: &str,
    entries: &[&StatusEntry],
    color: &str,
    code: impl Fn(&StatusEntry) -> &str,
) {
    if entries.is_empty() {
        return;
    }
    out.push('\n');
    out.push_str(&paint(&format!("{label} ({})", entries.len()), DIM));
    for e in entries {
        let path = match &e.orig_path {
            Some(from) if *from != e.path => format!("{from} -> {}", e.path),
            _ => e.path.clone(),
        };
        out.push_str(&format!("\n  {} {path}", paint(code(e), color)));
    }
}

/// A smartlog: your draft commits and the trunk tip, one per line, with a
/// marker (HEAD `*`, trunk `=`, other `o`), branch labels, and age.
/// Abbreviate a change id (`I` + 40 hex) to `I` + its first 8 hex, matching how
/// short commit ids read, so the smartlog can show it without eating the line.
fn short_change(id: &str) -> String {
    id.chars().take(9).collect()
}

pub fn smartlog(entries: &[SmartlogEntry]) -> String {
    if entries.is_empty() {
        return "no commits".to_owned();
    }
    entries
        .iter()
        .map(|e| {
            let (marker, color) = if e.is_head {
                ("*", GREEN)
            } else if e.is_trunk {
                ("=", CYAN)
            } else {
                ("o", DIM)
            };
            let refs = if e.refs.is_empty() {
                String::new()
            } else {
                format!(" ({})", e.refs.join(", "))
            };
            let change = match e.change_id.as_deref() {
                Some(id) => format!("{}  ", paint(&short_change(id), DIM)),
                None => String::new(),
            };
            format!(
                "{} {}{} {}  {}{}",
                paint(marker, color),
                paint(&e.short_id, YELLOW),
                paint(&refs, CYAN),
                e.summary,
                change,
                paint(&e.when, DIM)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The operation log: `sha label (on branch, age)`, newest first.
pub fn oplog(entries: &[rgit_git::OpLogEntry]) -> String {
    if entries.is_empty() {
        return "op-log is empty".to_owned();
    }
    entries
        .iter()
        .map(|e| {
            format!(
                "{} {} {}",
                paint(&e.short_id, YELLOW),
                e.label,
                paint(&format!("({}, {})", e.head, e.when), DIM)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One `sha subject` line per commit; the sha is dim.
/// git --decorate labels for a log line: local branch, upstream, then tags,
/// each trailed by a space so they slot between the hash and the summary.
fn ref_decor(refs: &[CommitRef]) -> String {
    let mut out = String::new();
    for deco in group_decorations(refs) {
        match deco {
            Deco::Local(name) => out.push_str(&format!("{} ", paint(&name, GREEN))),
            Deco::Remote { remote, branch } => out.push_str(&format!(
                "{} ",
                paint(&format!("{remote}/{branch}"), MAGENTA)
            )),
            Deco::Tag(name) => out.push_str(&format!("{} ", paint(&name, CYAN))),
            Deco::Group {
                branch,
                local,
                remotes,
            } => {
                let mut parts = Vec::new();
                if local {
                    parts.push(paint("local", GREEN));
                }
                for r in &remotes {
                    parts.push(paint(r, MAGENTA));
                }
                let name_color = if local { GREEN } else { MAGENTA };
                out.push_str(&format!(
                    "{}{}{}{} ",
                    paint("{", DIM),
                    parts.join(&paint(",", DIM)),
                    paint("}/", DIM),
                    paint(&branch, name_color)
                ));
            }
        }
    }
    out
}

/// A directory listing: directories (trailing slash) first, then files with
/// their byte size.
pub fn tree(entries: &[TreeEntry]) -> String {
    if entries.is_empty() {
        return "empty tree".to_owned();
    }
    entries
        .iter()
        .map(|e| {
            if e.is_dir {
                format!("{}/", e.name)
            } else {
                format!("{:>9}  {}", e.size, e.name)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Code search results as `path:line: text`, one match per line (grep -n style).
pub fn grep(matches: &[GrepMatch]) -> String {
    if matches.is_empty() {
        return "no matches".to_owned();
    }
    matches
        .iter()
        .map(|m| format!("{}:{}: {}", m.path, m.line, m.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A file's contents, or a note for a binary blob.
pub fn blob(b: &Blob) -> String {
    match &b.text {
        Some(text) => text.clone(),
        None => format!("<binary file, {} bytes>", b.size),
    }
}

pub fn log(entries: &[LogEntry]) -> String {
    if entries.is_empty() {
        return "no commits".to_owned();
    }
    entries
        .iter()
        .map(|e| {
            let mark = if e.unpushed {
                paint(" \u{2191}", GREEN)
            } else {
                String::new()
            };
            format!(
                "{} {}{}{mark}",
                paint(&e.short_id, YELLOW),
                ref_decor(&e.refs),
                e.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Per-file `+add -del path` diffstat, additions green and deletions red.
pub fn diffstat(files: &[FileDiff]) -> String {
    if files.is_empty() {
        return "no changes".to_owned();
    }
    files
        .iter()
        .map(|f| {
            let (mut add, mut del) = (0, 0);
            for h in &f.hunks {
                for l in &h.lines {
                    match l.origin {
                        rgit_git::LineOrigin::Added => add += 1,
                        rgit_git::LineOrigin::Removed => del += 1,
                        _ => {}
                    }
                }
            }
            format!(
                "{} {} {}",
                paint(&format!("+{add}"), GREEN),
                paint(&format!("-{del}"), RED),
                f.path
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Refs grouped by kind, `*` marking HEAD.
pub fn refs(entries: &[RefEntry]) -> String {
    use rgit_git::RefKind;
    if entries.is_empty() {
        return "no refs".to_owned();
    }
    let kind = |k: RefKind| match k {
        RefKind::Local => "local",
        RefKind::Remote => "remote",
        RefKind::Tag => "tag",
    };
    entries
        .iter()
        .map(|r| {
            let head = if r.is_head {
                paint("*", GREEN)
            } else {
                " ".to_owned()
            };
            format!("{head} {} {}", paint(kind(r.kind), DIM), r.name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Stashes as `stash@{i} message`.
pub fn stashes(list: &[Stash]) -> String {
    if list.is_empty() {
        return "no stashes".to_owned();
    }
    list.iter()
        .map(|s| {
            format!(
                "{}: {}",
                paint(&format!("stash@{{{}}}", s.index), YELLOW),
                s.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Remotes as `name url`.
pub fn remotes(list: &[Remote]) -> String {
    if list.is_empty() {
        return "no remotes".to_owned();
    }
    list.iter()
        .map(|r| format!("{} {}", paint(&r.name, CYAN), paint(&r.url, DIM)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Worktrees as `name path`.
pub fn worktrees(list: &[Worktree]) -> String {
    if list.is_empty() {
        return "no worktrees".to_owned();
    }
    list.iter()
        .map(|w| {
            let head = match (&w.branch, &w.head) {
                (Some(b), Some(h)) => format!("{b} {h}"),
                (None, Some(h)) => format!("detached {h}"),
                _ => "(unborn)".to_owned(),
            };
            let mut flags = String::new();
            if w.dirty {
                flags.push_str(" dirty");
            }
            if w.locked {
                flags.push_str(" locked");
            }
            format!(
                "{}  {}{}  {}",
                paint(&w.name, CYAN),
                head,
                paint(&flags, DIM),
                paint(&w.path, DIM)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Local branch names, `*` (green) marking the current one.
pub fn branches(names: &[String], current: Option<&str>) -> String {
    if names.is_empty() {
        return "no branches".to_owned();
    }
    names
        .iter()
        .map(|n| {
            if Some(n.as_str()) == current {
                format!("{} {}", paint("*", GREEN), paint(n, GREEN))
            } else {
                format!("  {n}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A commit's header plus its diffstat (`git show`, compressed).
pub fn commit_details(c: &CommitDetails) -> String {
    let mut out = format!(
        "{} {} {}",
        paint(&c.id, YELLOW),
        c.author,
        paint(&c.when, DIM)
    );
    let subject = c.message.lines().next().unwrap_or("");
    out.push_str(&format!("\n{subject}"));
    if !c.files.is_empty() {
        out.push('\n');
        out.push_str(&diffstat(&c.files));
    }
    out
}

/// Blame as `sha author line` per line, or `sha line` without `author`.
pub fn blame(lines: &[BlameLine], author: bool) -> String {
    if lines.is_empty() {
        return "no lines".to_owned();
    }
    lines
        .iter()
        .map(|b| {
            let sha = paint(&b.short_id, YELLOW);
            if author {
                format!("{sha} {} {}", paint(&b.author, DIM), b.line)
            } else {
                format!("{sha} {}", b.line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A reconstructed unified patch for ref-to-ref diffs (the staged diff has its
/// own patch from the backend).
pub fn patch(files: &[FileDiff]) -> String {
    use rgit_git::LineOrigin;
    let mut out = String::new();
    for f in files {
        if f.header.is_empty() {
            out.push_str(&paint(&format!("--- a/{p}\n+++ b/{p}\n", p = f.path), BOLD));
        } else {
            for line in f.header.lines() {
                out.push_str(&paint(line, BOLD));
                out.push('\n');
            }
        }
        for h in &f.hunks {
            out.push_str(&paint(&h.header, CYAN));
            out.push('\n');
            for l in &h.lines {
                let (prefix, color) = match l.origin {
                    LineOrigin::Added => ("+", GREEN),
                    LineOrigin::Removed => ("-", RED),
                    LineOrigin::Context => (" ", ""),
                    LineOrigin::Meta => ("", DIM),
                };
                let line = format!("{prefix}{}", l.text.trim_matches('\n'));
                out.push_str(&if color.is_empty() {
                    line
                } else {
                    paint(&line, color)
                });
                out.push('\n');
            }
        }
    }
    out
}

/// `old => new` as git's diffstat prints a rename, with the shared leading and
/// trailing folders outside braces: `dir/{a => b}/f`.
pub fn rename_name(a: &str, b: &str) -> String {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let mut pfx = 0;
    for (i, (x, y)) in ab.iter().zip(bb).enumerate() {
        if x != y {
            break;
        }
        if *x == b'/' {
            pfx = i + 1;
        }
    }
    let mut sfx = 0;
    let (mut i, mut j) = (ab.len() as isize, bb.len() as isize);
    let low = pfx as isize - isize::from(pfx > 0);
    // As in git, the walk starts one past the end, where both "match".
    while i >= low && j >= low {
        let x = ab.get(i as usize);
        if x != bb.get(j as usize) {
            break;
        }
        if x == Some(&b'/') {
            sfx = ab.len() - i as usize;
        }
        i -= 1;
        j -= 1;
    }
    let amid = &a[pfx..(ab.len().saturating_sub(sfx)).max(pfx)];
    let bmid = &b[pfx..(bb.len().saturating_sub(sfx)).max(pfx)];
    if pfx + sfx > 0 {
        format!("{}{{{amid} => {bmid}}}{}", &a[..pfx], &a[ab.len() - sfx..])
    } else {
        format!("{amid} => {bmid}")
    }
}

/// git's ` N files changed, X insertions(+), Y deletions(-)` line.
pub fn stat_summary(files: &[FileDiff]) -> String {
    let (mut add, mut del) = (0, 0);
    for f in files {
        let (a, d) = crate::axi::line_counts(f);
        add += a;
        del += d;
    }
    let s = |n: usize| if n == 1 { "" } else { "s" };
    let mut out = format!(" {} file{} changed", files.len(), s(files.len()));
    if files.is_empty() {
        return out;
    }
    if add > 0 || del == 0 {
        out.push_str(&format!(", {add} insertion{}(+)", s(add)));
    }
    if del > 0 || add == 0 {
        out.push_str(&format!(", {del} deletion{}(-)", s(del)));
    }
    out
}

/// git's `--stat`: ` name | count +++--` per file, scaled to the terminal
/// width (80 when piped), then the summary line.
pub fn stat(files: &[FileDiff], indent: usize) -> String {
    use std::io::IsTerminal;
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .or_else(|| {
            std::io::stdout()
                .is_terminal()
                .then(|| crossterm::terminal::size().ok())
                .flatten()
                .map(|(w, _)| usize::from(w))
        })
        .unwrap_or(80)
        .saturating_sub(indent);
    let rows: Vec<(String, usize, usize, bool)> = files
        .iter()
        .map(|f| {
            let name = match &f.old_path {
                Some(old) => rename_name(old, &f.path),
                None => f.path.clone(),
            };
            // A binary file counts bytes, new then old, as git's does.
            let (a, d) = if f.binary {
                (f.sizes.1 as usize, f.sizes.0 as usize)
            } else {
                crate::axi::line_counts(f)
            };
            (name, a, d, f.binary)
        })
        .collect();
    let max_change = rows
        .iter()
        .filter(|r| !r.3)
        .map(|r| r.1 + r.2)
        .max()
        .unwrap_or(0);
    let bin_width = rows
        .iter()
        .filter(|r| r.3)
        .map(|r| 14 + r.1.to_string().len() + r.2.to_string().len())
        .max()
        .unwrap_or(0);
    let max_len = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0);
    let mut number_width = max_change.to_string().len();
    if bin_width > 0 {
        number_width = number_width.max(3);
    }
    let width = width.max(16 + 6 + number_width);
    let mut graph_width = if max_change + 4 > bin_width {
        max_change
    } else {
        bin_width - 4
    };
    let mut name_width = max_len;
    if name_width + number_width + 6 + graph_width > width {
        let cap = (width * 3 / 8).saturating_sub(number_width + 6);
        if graph_width > cap {
            graph_width = cap.max(6);
        }
        if name_width > width.saturating_sub(number_width + 6 + graph_width) {
            name_width = width.saturating_sub(number_width + 6 + graph_width);
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }
    let scale = |n: usize| {
        if n == 0 {
            0
        } else {
            1 + n * (graph_width - 1) / max_change
        }
    };
    let mut out = String::new();
    for (name, a, d, binary) in &rows {
        let (mut name, mut prefix, mut len) = (name.as_str(), "", name_width);
        let chars = name.chars().count();
        if chars > name_width {
            prefix = "...";
            len = len.saturating_sub(3);
            let skip = name
                .char_indices()
                .nth(chars - len)
                .map_or(name.len(), |(i, _)| i);
            name = &name[skip..];
            if let Some(i) = name.find('/') {
                name = &name[i..];
            }
        }
        let pad = len.saturating_sub(name.chars().count());
        if *binary {
            out.push_str(&format!(
                " {prefix}{name}{:pad$} | {:>number_width$}",
                "", "Bin"
            ));
            if a + d > 0 {
                out.push_str(&format!(
                    " {} -> {} bytes",
                    paint(&d.to_string(), RED),
                    paint(&a.to_string(), GREEN)
                ));
            }
            out.push('\n');
            continue;
        }
        let (mut add, mut del) = (*a, *d);
        if graph_width <= max_change {
            let mut total = scale(a + d);
            if total < 2 && *a > 0 && *d > 0 {
                total = 2;
            }
            if a < d {
                add = scale(*a);
                del = total - add;
            } else {
                del = scale(*d);
                add = total - del;
            }
        }
        out.push_str(&format!(
            " {prefix}{name}{:pad$} | {:>number_width$}{}{}{}\n",
            "",
            a + d,
            if a + d > 0 { " " } else { "" },
            paint(&"+".repeat(add), GREEN),
            paint(&"-".repeat(del), RED),
        ));
    }
    out.push_str(&stat_summary(files));
    out
}
