//! `git apply`: patches are parsed here (so `-p`, `--directory`, `--include`,
//! `-R` and whitespace fixes work on any patch, binary ones included), then
//! applied through libgit2, with git's three-way and reject fallbacks.

use std::fmt::Write as _;
use std::path::Path;

use git2::{ApplyLocation, Diff, Repository};

use crate::GitError;

/// How `git apply` treats a patch.
#[derive(Debug, Clone, Default)]
pub struct ApplyOpts {
    /// Apply to the index only.
    pub cached: bool,
    /// Apply to the index and the working tree.
    pub index: bool,
    /// Only check that it applies.
    pub check: bool,
    pub reverse: bool,
    /// Fall back to a three-way merge with the preimage blob.
    pub three_way: bool,
    /// Apply the hunks that fit and leave the rest in `<file>.rej`.
    pub reject: bool,
    /// Report each file on stderr.
    pub verbose: bool,
    /// Leading path components to strip (git's `-p`, default 1).
    pub strip: Option<usize>,
    /// Folder to prepend to every path.
    pub directory: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// nowarn, warn, fix (strip), error or error-all.
    pub whitespace: Option<String>,
    /// A patch with no file changes is not an error.
    pub allow_empty: bool,
    /// Run from this folder of the working tree: only paths under it apply.
    pub prefix: Option<String>,
    /// Resolve three-way conflicts to ours, theirs or union.
    pub favor: Option<String>,
    /// Count hunk lines instead of trusting the `@@` header.
    pub recount: bool,
    /// No progress lines.
    pub quiet: bool,
}

/// One file's part of a patch.
#[derive(Debug, Clone, Default)]
pub struct FilePatch {
    pub old: Option<String>,
    pub new: Option<String>,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    /// `new file mode` / `deleted file mode`.
    pub created: Option<String>,
    pub deleted: Option<String>,
    /// `rename` or `copy`, with the similarity score.
    pub moved: Option<(&'static str, u32)>,
    /// `index <old>..<new>[ <mode>]`.
    pub index: Option<(String, String, String)>,
    pub hunks: Vec<Hunk>,
    /// The `GIT binary patch` blocks (forward, then reverse), or `Binary
    /// files ... differ` when there is no data.
    pub binary: Option<Vec<String>>,
}

/// One `@@` hunk.
#[derive(Debug, Clone)]
pub struct Hunk {
    pub old: (u32, u32),
    pub new: (u32, u32),
    /// Text after the second `@@`, newline included.
    pub tail: String,
    /// Lines with their `+`/`-`/` `/`\` prefix and newline.
    pub lines: Vec<String>,
}

impl FilePatch {
    pub fn is_binary(&self) -> bool {
        self.binary.is_some()
    }

    /// Lines added and deleted.
    pub fn counts(&self) -> (usize, usize) {
        let lines = self.hunks.iter().flat_map(|h| &h.lines);
        lines.fold((0, 0), |(a, d), l| match l.as_bytes().first() {
            Some(b'+') => (a + 1, d),
            Some(b'-') => (a, d + 1),
            _ => (a, d),
        })
    }

    /// The name git reports: the new path, or the old one for a deletion.
    pub fn name(&self) -> &str {
        self.new
            .as_deref()
            .or(self.old.as_deref())
            .unwrap_or_default()
    }

    /// `old => new` for a rename or copy, else the name.
    fn label(&self) -> String {
        match (&self.moved, &self.old, &self.new) {
            (Some(_), Some(o), Some(n)) => format!("{o} => {n}"),
            _ => self.name().to_owned(),
        }
    }

    /// The patch text in canonical `a/`/`b/` git form, for libgit2.
    fn render(&self) -> String {
        let old = self.old.as_deref();
        let new = self.new.as_deref();
        let mut out = format!(
            "diff --git a/{} b/{}\n",
            old.or(new).unwrap_or_default(),
            new.or(old).unwrap_or_default()
        );
        let mut line = |s: String| {
            out.push_str(&s);
            out.push('\n');
        };
        if let Some(m) = &self.old_mode {
            line(format!("old mode {m}"));
        }
        if let Some(m) = &self.new_mode {
            line(format!("new mode {m}"));
        }
        if let Some(m) = &self.deleted {
            line(format!("deleted file mode {m}"));
        }
        if let Some(m) = &self.created {
            line(format!("new file mode {m}"));
        }
        if let (Some((kind, score)), Some(o), Some(n)) = (&self.moved, old, new) {
            line(format!("similarity index {score}%"));
            line(format!("{kind} from {o}"));
            line(format!("{kind} to {n}"));
        }
        if let Some((a, b, mode)) = &self.index {
            line(format!("index {a}..{b}{mode}"));
        }
        if !self.hunks.is_empty() {
            line(format!(
                "--- {}",
                old.map_or("/dev/null".to_owned(), |p| format!("a/{p}"))
            ));
            line(format!(
                "+++ {}",
                new.map_or("/dev/null".to_owned(), |p| format!("b/{p}"))
            ));
        }
        for h in &self.hunks {
            out.push_str(&hunk_header(h));
            for l in &h.lines {
                out.push_str(l);
            }
        }
        if let Some(blocks) = &self.binary {
            if blocks.len() == 1 && blocks[0].starts_with("Binary files") {
                out.push_str(&blocks[0]);
            } else {
                out.push_str("GIT binary patch\n");
                for b in blocks {
                    out.push_str(b);
                    out.push('\n');
                }
            }
        }
        out
    }

    fn reverse(&mut self) {
        std::mem::swap(&mut self.old, &mut self.new);
        std::mem::swap(&mut self.old_mode, &mut self.new_mode);
        std::mem::swap(&mut self.created, &mut self.deleted);
        if let Some((a, b, _)) = &mut self.index {
            std::mem::swap(a, b);
        }
        for h in &mut self.hunks {
            std::mem::swap(&mut h.old, &mut h.new);
            for l in &mut h.lines {
                let flipped = match l.as_bytes().first() {
                    Some(b'+') => Some('-'),
                    Some(b'-') => Some('+'),
                    _ => None,
                };
                if let Some(c) = flipped {
                    l.replace_range(..1, &c.to_string());
                }
            }
            // A `\ No newline` note belongs to the line before it, so each
            // -/+ run is swapped as a block to keep it in place.
            let mut out: Vec<String> = Vec::with_capacity(h.lines.len());
            let mut i = 0;
            while i < h.lines.len() {
                if !h.lines[i].starts_with(['+', '-']) {
                    out.push(h.lines[i].clone());
                    i += 1;
                    continue;
                }
                let start = i;
                while i < h.lines.len() && !h.lines[i].starts_with(' ') {
                    i += 1;
                }
                let run = &h.lines[start..i];
                let side = |c: char| {
                    let mut v = Vec::new();
                    for (j, l) in run.iter().enumerate() {
                        if l.starts_with(c) {
                            v.push(l.clone());
                            if let Some(n) = run.get(j + 1).filter(|n| n.starts_with('\\')) {
                                v.push(n.clone());
                            }
                        }
                    }
                    v
                };
                out.extend(side('-'));
                out.extend(side('+'));
            }
            h.lines = out;
        }
        if let Some(b) = &mut self.binary
            && b.len() == 2
        {
            b.swap(0, 1);
        }
    }
}

fn hunk_header(h: &Hunk) -> String {
    let range = |(start, len): (u32, u32)| {
        if len == 1 {
            start.to_string()
        } else {
            format!("{start},{len}")
        }
    };
    format!("@@ -{} +{} @@{}", range(h.old), range(h.new), h.tail)
}

/// Strip `n` leading components from `path`.
fn strip(path: &str, n: usize) -> Option<String> {
    let mut rest = path;
    for _ in 0..n {
        rest = &rest[rest.find('/')? + 1..];
    }
    Some(rest.to_owned())
}

/// The path of a `---`/`+++` line, without any tab-separated timestamp.
fn header_path(s: &str, n: usize) -> Option<Option<String>> {
    let s = s.trim_end_matches(['\n', '\r']);
    let s = s.split('\t').next().unwrap_or(s);
    let s = unquote(s);
    if s == "/dev/null" {
        return Some(None);
    }
    Some(Some(strip(&s, n)?))
}

/// A C-quoted path (`"a\tb"`) unquoted; other paths as they are.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return s.to_owned();
    };
    let mut bytes = Vec::new();
    let mut chars = inner.bytes().peekable();
    while let Some(b) = chars.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match chars.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(d @ b'0'..=b'7') => {
                let mut v = u32::from(d - b'0');
                for _ in 0..2 {
                    if let Some(&d @ b'0'..=b'7') = chars.peek() {
                        v = v * 8 + u32::from(d - b'0');
                        chars.next();
                    }
                }
                bytes.push(v as u8);
            }
            Some(c) => bytes.push(c),
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// Parse `text` into file patches, applying `-p`, `--directory`,
/// `--include`/`--exclude`, `-R` and `--whitespace=fix`.
pub fn parse_patch(text: &[u8], opts: &ApplyOpts) -> Result<Vec<FilePatch>, GitError> {
    let text = String::from_utf8_lossy(text);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let p = opts.strip.unwrap_or(1);
    let bad = |what: &str| GitError::Other(format!("corrupt patch: {what}"));
    let mut out = Vec::new();
    let mut seen = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let git = line.starts_with("diff --git ");
        let plain =
            line.starts_with("--- ") && lines.get(i + 1).is_some_and(|l| l.starts_with("+++ "));
        if !git && !plain {
            i += 1;
            continue;
        }
        let mut f = FilePatch::default();
        let mut git_names = None;
        if git {
            git_names = Some(line["diff --git ".len()..].trim_end_matches(['\n', '\r']));
            i += 1;
        }
        // Extended headers.
        let (mut minus, mut plus) = (None, None);
        while i < lines.len() {
            let l = lines[i].trim_end_matches(['\n', '\r']);
            let value = |prefix: &str| l.strip_prefix(prefix).map(str::to_owned);
            if let Some(v) = value("old mode ") {
                f.old_mode = Some(v);
            } else if let Some(v) = value("new mode ") {
                f.new_mode = Some(v);
            } else if let Some(v) = value("deleted file mode ") {
                f.deleted = Some(v);
            } else if let Some(v) = value("new file mode ") {
                f.created = Some(v);
            } else if let Some(v) = value("similarity index ") {
                let score = v.trim_end_matches('%').parse().unwrap_or(100);
                f.moved = Some((f.moved.map_or("rename", |m| m.0), score));
            } else if l.starts_with("dissimilarity index ") {
            } else if let Some(v) = value("rename from ").or_else(|| value("copy from ")) {
                let kind = if l.starts_with("copy") {
                    "copy"
                } else {
                    "rename"
                };
                f.moved = Some((kind, f.moved.map_or(100, |m| m.1)));
                f.old = Some(strip(&unquote(&v), p.saturating_sub(1)).ok_or_else(|| bad(l))?);
            } else if let Some(v) = value("rename to ").or_else(|| value("copy to ")) {
                f.new = Some(strip(&unquote(&v), p.saturating_sub(1)).ok_or_else(|| bad(l))?);
            } else if let Some(v) = value("index ") {
                let (ids, mode) = v.split_once(' ').map_or((v.as_str(), ""), |(a, m)| (a, m));
                let (a, b) = ids.split_once("..").ok_or_else(|| bad(l))?;
                let mode = if mode.is_empty() {
                    String::new()
                } else {
                    format!(" {mode}")
                };
                f.index = Some((a.to_owned(), b.to_owned(), mode));
            } else if let Some(v) = value("--- ") {
                minus = Some(header_path(&v, p).ok_or_else(|| bad(l))?);
            } else if let Some(v) = value("+++ ") {
                plus = Some(header_path(&v, p).ok_or_else(|| bad(l))?);
            } else if l.starts_with("Binary files ") {
                f.binary = Some(vec![lines[i].to_owned()]);
            } else if l == "GIT binary patch" {
                let mut blocks = Vec::new();
                i += 1;
                while blocks.len() < 2
                    && lines
                        .get(i)
                        .is_some_and(|l| l.starts_with("literal ") || l.starts_with("delta "))
                {
                    let mut block = String::new();
                    while let Some(l) = lines.get(i).filter(|l| !l.trim_end().is_empty()) {
                        block.push_str(l);
                        i += 1;
                    }
                    i += 1;
                    blocks.push(block);
                }
                f.binary = Some(blocks);
                break;
            } else {
                break;
            }
            i += 1;
        }
        // Hunks.
        while let Some(l) = lines.get(i).filter(|l| l.starts_with("@@ -")) {
            let rest = &l["@@ -".len()..];
            let (ranges, tail) = rest.split_once(" @@").ok_or_else(|| bad(l))?;
            let (o, n) = ranges.split_once(" +").ok_or_else(|| bad(l))?;
            let mut h = Hunk {
                old: parse_range(o).ok_or_else(|| bad(l))?,
                new: parse_range(n).ok_or_else(|| bad(l))?,
                tail: tail.to_owned(),
                lines: Vec::new(),
            };
            i += 1;
            if opts.recount {
                // The hunk runs until a line that cannot be part of one.
                let starts_file = |j: usize| {
                    lines[j].starts_with("--- ")
                        && lines.get(j + 1).is_some_and(|n| n.starts_with("+++ "))
                };
                let (mut old, mut new) = (0, 0);
                let mut j = i;
                while j < lines.len()
                    && lines[j].starts_with([' ', '+', '-', '\\'])
                    && !starts_file(j)
                {
                    match lines[j].as_bytes()[0] {
                        b'+' => new += 1,
                        b'-' => old += 1,
                        b' ' => {
                            old += 1;
                            new += 1;
                        }
                        _ => {}
                    }
                    j += 1;
                }
                h.old.1 = old;
                h.new.1 = new;
            }
            let (mut old_left, mut new_left) = (h.old.1, h.new.1);
            while old_left > 0 || new_left > 0 || lines.get(i).is_some_and(|l| l.starts_with('\\'))
            {
                let Some(l) = lines.get(i) else {
                    return Err(bad("truncated hunk"));
                };
                match l.as_bytes().first() {
                    Some(b'+') => new_left = new_left.saturating_sub(1),
                    Some(b'-') => old_left = old_left.saturating_sub(1),
                    Some(b'\\') => {}
                    // A blank line is an empty context line mangled by a mailer.
                    Some(b' ') | Some(b'\n') | Some(b'\r') => {
                        old_left = old_left.saturating_sub(1);
                        new_left = new_left.saturating_sub(1);
                    }
                    _ => return Err(bad(&format!("unexpected line in hunk: {}", l.trim_end()))),
                }
                h.lines.push(if l.starts_with(['\n', '\r']) {
                    format!(" {l}")
                } else {
                    (*l).to_owned()
                });
                i += 1;
            }
            f.hunks.push(h);
        }
        // Names: ---/+++ and rename/copy win over the `diff --git` line.
        if let Some(m) = minus
            && (f.moved.is_none() || f.old.is_none())
        {
            f.old = m;
        }
        if let Some(pl) = plus
            && (f.moved.is_none() || f.new.is_none())
        {
            f.new = pl;
        }
        if f.old.is_none() && f.new.is_none() {
            let names = git_names.ok_or_else(|| bad("no file names"))?;
            let name = git_diff_name(names, p).ok_or_else(|| bad(names))?;
            f.old = Some(name.clone());
            f.new = Some(name);
        }
        if f.created.is_some() || (plain && f.old.is_none()) {
            f.old = None;
            f.created.get_or_insert_with(|| "100644".to_owned());
        } else if f.old.is_none() && git {
            f.old = f.new.clone();
        }
        if f.deleted.is_some() || (plain && f.new.is_none()) {
            f.new = None;
            f.deleted.get_or_insert_with(|| "100644".to_owned());
        } else if f.new.is_none() && git {
            f.new = f.old.clone();
        }
        if let Some(dir) = opts.directory.as_deref().filter(|d| !d.is_empty()) {
            let dir = dir.trim_end_matches('/');
            for p in [&mut f.old, &mut f.new].into_iter().flatten() {
                *p = format!("{dir}/{p}");
            }
        }
        seen = true;
        if !used(opts, f.name()) {
            continue;
        }
        if opts.reverse {
            f.reverse();
        }
        out.push(f);
    }
    if !seen && !opts.allow_empty {
        return Err(GitError::Other(
            "No valid patches in input (allow with \"--allow-empty\")".to_owned(),
        ));
    }
    Ok(out)
}

/// The name in `diff --git a/<name> b/<name>`: both halves must agree.
fn git_diff_name(names: &str, p: usize) -> Option<String> {
    let bytes = names.as_bytes();
    (0..bytes.len())
        .filter(|&i| bytes[i] == b' ')
        .find_map(|i| {
            let a = strip(&unquote(&names[..i]), p)?;
            let b = strip(&unquote(&names[i + 1..]), p)?;
            (a == b).then_some(a)
        })
}

/// Whether `--include`/`--exclude` and the current folder keep `path`.
fn used(opts: &ApplyOpts, path: &str) -> bool {
    let matches = |pats: &[String]| pats.iter().any(|g| wildmatch(g, path));
    let outside = opts.prefix.as_deref().is_some_and(|p| {
        !p.is_empty() && !path.starts_with(&format!("{}/", p.trim_end_matches('/')))
    });
    if outside || matches(&opts.exclude) {
        return false;
    }
    opts.include.is_empty() || matches(&opts.include)
}

/// git's `wildmatch` without flags: `*` also matches `/`.
fn wildmatch(glob: &str, path: &str) -> bool {
    let mut re = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '[' => {
                re.push('[');
                if chars.peek() == Some(&'!') {
                    chars.next();
                    re.push('^');
                }
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    if c == '\\' {
                        re.push('\\');
                    }
                    re.push(c);
                }
                re.push(']');
            }
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).is_ok_and(|r| r.is_match(path))
}

/// Check added lines for trailing whitespace; fix them (`fix`/`strip`), or
/// report them as git does. Returns the warning for stderr, if any.
pub fn check_whitespace(files: &mut [FilePatch], action: &str) -> Result<String, GitError> {
    let mut bad = 0;
    for f in files.iter_mut() {
        for h in &mut f.hunks {
            for l in &mut h.lines {
                if !l.starts_with('+') {
                    continue;
                }
                let body = l.trim_end_matches(['\n', '\r']);
                let trimmed = body.trim_end_matches([' ', '\t']);
                if trimmed.len() != body.len() {
                    bad += 1;
                    if matches!(action, "fix" | "strip") {
                        *l = format!("{trimmed}{}", &l[body.len()..]);
                    }
                }
            }
        }
    }
    if bad == 0 {
        return Ok(String::new());
    }
    let what = if bad == 1 {
        "1 line adds whitespace errors.".to_owned()
    } else {
        format!("{bad} lines add whitespace errors.")
    };
    match action {
        "error" | "error-all" => Err(GitError::Other(what)),
        "fix" | "strip" => Ok(if bad == 1 {
            "warning: 1 line applied after fixing whitespace errors.".to_owned()
        } else {
            format!("warning: {bad} lines applied after fixing whitespace errors.")
        }),
        "warn" => Ok(format!("warning: {what}")),
        _ => Ok(String::new()),
    }
}

/// `git apply --stat`.
pub fn patch_stat(files: &[FilePatch]) -> String {
    let names: Vec<String> = files.iter().map(|f| f.name().to_owned()).collect();
    let max_len = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    let max_change = files
        .iter()
        .map(|f| f.counts().0 + f.counts().1)
        .max()
        .unwrap_or(0);
    let width = max_len.min(50);
    let mut out = String::new();
    let (mut adds, mut dels) = (0, 0);
    for (f, name) in files.iter().zip(&names) {
        let mut name = name.clone();
        let len = name.chars().count();
        if len > width {
            let skip = len + 3 - width;
            let cut: String = name.chars().skip(skip).collect();
            let cut = match cut.find('/') {
                Some(i) => cut[i..].to_owned(),
                None => cut,
            };
            name = format!("...{cut}");
        }
        if f.is_binary() {
            let _ = writeln!(out, " {name:<width$} |  Bin");
            continue;
        }
        let (a, d) = f.counts();
        adds += a;
        dels += d;
        let scale = if width + max_change > 70 {
            70 - width
        } else {
            max_change
        };
        let half = max_change / 2;
        let total = ((a + d) * scale + half)
            .checked_div(max_change)
            .unwrap_or(a + d);
        let pa = (a * scale + half).checked_div(max_change).unwrap_or(a);
        let pd = total - pa;
        let _ = writeln!(
            out,
            " {name:<width$} |{:>5} {}{}",
            a + d,
            "+".repeat(pa),
            "-".repeat(pd)
        );
    }
    out.push_str(&stat_summary(files.len(), adds, dels));
    out.push('\n');
    out
}

/// git's ` N files changed, X insertions(+), Y deletions(-)`.
pub fn stat_summary(files: usize, adds: usize, dels: usize) -> String {
    if files == 0 {
        return " 0 files changed".to_owned();
    }
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut s = format!(" {}", plural(files, "file changed", "files changed"));
    if adds > 0 || dels == 0 {
        s.push_str(&format!(
            ", {}",
            plural(adds, "insertion(+)", "insertions(+)")
        ));
    }
    if dels > 0 || adds == 0 {
        s.push_str(&format!(
            ", {}",
            plural(dels, "deletion(-)", "deletions(-)")
        ));
    }
    s
}

/// `git apply --numstat`.
pub fn patch_numstat(files: &[FilePatch]) -> String {
    files
        .iter()
        .map(|f| {
            if f.is_binary() {
                format!("-\t-\t{}\n", f.name())
            } else {
                let (a, d) = f.counts();
                format!("{a}\t{d}\t{}\n", f.name())
            }
        })
        .collect()
}

/// `git apply --summary` (also format-patch's summary).
pub fn patch_summary(files: &[FilePatch]) -> String {
    let mut out = String::new();
    for f in files {
        if let Some(m) = &f.created {
            let _ = writeln!(out, " create mode {m} {}", f.name());
        } else if let Some(m) = &f.deleted {
            let _ = writeln!(out, " delete mode {m} {}", f.name());
        } else if let (Some((kind, score)), Some(old), Some(new)) = (&f.moved, &f.old, &f.new) {
            let _ = writeln!(out, " {kind} {} ({score}%)", rename_label(old, new));
            if let (Some(a), Some(b)) = (&f.old_mode, &f.new_mode) {
                let _ = writeln!(out, " mode change {a} => {b}");
            }
        } else if let (Some(a), Some(b)) = (&f.old_mode, &f.new_mode) {
            let _ = writeln!(out, " mode change {a} => {b} {}", f.name());
        }
    }
    out
}

/// `old => new` with a shared leading folder folded, as `dir/{a => b}`.
pub fn rename_label(old: &str, new: &str) -> String {
    let mut common = 0;
    let (ob, nb) = (old.as_bytes(), new.as_bytes());
    while let (Some(so), Some(sn)) = (old[common..].find('/'), new[common..].find('/')) {
        if so != sn || ob[common..common + so] != nb[common..common + sn] {
            break;
        }
        common += so + 1;
    }
    if common > 0 {
        format!(
            "{}{{{} => {}}}",
            &old[..common],
            &old[common..],
            &new[common..]
        )
    } else {
        format!("{old} => {new}")
    }
}

/// Apply `files` to the repository (or, with `check`, only test them).
/// Returns git's progress lines for stderr; rejected hunks and three-way
/// conflicts are a `Conflict` error carrying them.
pub fn apply(repo: &Repository, files: &[FilePatch], opts: &ApplyOpts) -> Result<String, GitError> {
    let location = if opts.cached {
        ApplyLocation::Index
    } else if opts.index || (opts.three_way && repo.workdir().is_some()) {
        ApplyLocation::Both
    } else {
        ApplyLocation::WorkDir
    };
    let fits = |text: &str| -> Result<(), GitError> {
        let diff = Diff::from_buffer(text.as_bytes())?;
        let mut o = git2::ApplyOptions::new();
        o.check(true);
        repo.apply(&diff, location, Some(&mut o))?;
        Ok(())
    };
    let mut log = String::new();
    // First decide how each file goes, so a patch that cannot apply at all
    // changes nothing.
    enum Plan {
        Direct,
        Merge(Vec<u8>, Vec<u8>, Vec<u8>),
        Reject,
    }
    let mut plans = Vec::new();
    for f in files {
        let text = f.render();
        if opts.verbose || opts.reject {
            let _ = writeln!(log, "Checking patch {}...", f.label());
        }
        let plan = match fits(&text) {
            Ok(()) => Plan::Direct,
            Err(e) if opts.three_way => match three_way_inputs(repo, f, opts.cached) {
                Some((base, ours, theirs)) => Plan::Merge(base, ours, theirs),
                None => return Err(patch_failed(f, e)),
            },
            Err(_) if opts.reject => Plan::Reject,
            Err(e) => return Err(patch_failed(f, e)),
        };
        plans.push(plan);
    }
    if opts.check {
        return Ok(log.trim_end().to_owned());
    }
    let direct: String = files
        .iter()
        .zip(&plans)
        .filter(|(_, p)| matches!(p, Plan::Direct))
        .map(|(f, _)| f.render())
        .collect();
    if !direct.is_empty() {
        repo.apply(&Diff::from_buffer(direct.as_bytes())?, location, None)?;
    }
    let mut failed = false;
    for (f, plan) in files.iter().zip(plans) {
        match plan {
            Plan::Direct => {
                if opts.verbose || opts.reject {
                    let _ = writeln!(log, "Applied patch {} cleanly.", f.label());
                }
            }
            Plan::Merge(base, ours, theirs) => {
                let clean = write_merge(
                    repo,
                    f,
                    &base,
                    &ours,
                    &theirs,
                    location,
                    opts.favor.as_deref(),
                )?;
                let name = f.name();
                if clean {
                    let _ = writeln!(log, "Applied patch to '{name}' cleanly.");
                } else {
                    failed = true;
                    let _ = writeln!(log, "Applied patch to '{name}' with conflicts.");
                    let _ = writeln!(log, "U {name}");
                }
            }
            Plan::Reject => {
                failed = true;
                apply_with_rejects(repo, f, location, &mut log)?;
            }
        }
    }
    let log = if opts.quiet {
        String::new()
    } else {
        log.trim_end().to_owned()
    };
    if failed {
        Err(GitError::Conflict(log))
    } else {
        Ok(log)
    }
}

fn patch_failed(f: &FilePatch, e: GitError) -> GitError {
    GitError::Other(format!("{}: patch does not apply ({e})", f.name()))
}

/// Apply the hunks of `f` that fit, one by one, and write the rest to
/// `<file>.rej` as git does.
fn apply_with_rejects(
    repo: &Repository,
    f: &FilePatch,
    location: ApplyLocation,
    log: &mut String,
) -> Result<(), GitError> {
    let mut rejected = Vec::new();
    let mut outcomes = Vec::new();
    for (i, h) in f.hunks.iter().enumerate() {
        let one = FilePatch {
            hunks: vec![h.clone()],
            ..f.clone()
        };
        let diff = Diff::from_buffer(one.render().as_bytes())?;
        repo.index()?.read(true)?;
        match repo.apply(&diff, location, None) {
            Ok(()) => outcomes.push(format!("Hunk #{} applied cleanly.", i + 1)),
            Err(_) => {
                outcomes.push(format!("Rejected hunk #{}.", i + 1));
                rejected.push(h);
            }
        }
    }
    let _ = writeln!(
        log,
        "Applying patch {} with {} reject{}...",
        f.label(),
        rejected.len(),
        if rejected.len() == 1 { "" } else { "s" }
    );
    for o in outcomes {
        let _ = writeln!(log, "{o}");
    }
    if f.hunks.is_empty() || matches!(location, ApplyLocation::Index) {
        return Ok(());
    }
    let name = f.name();
    let mut rej = format!("diff a/{name} b/{name}\t(rejected hunks)\n");
    for h in rejected {
        rej.push_str(&hunk_header(h));
        for l in &h.lines {
            rej.push_str(l);
        }
    }
    if let Some(workdir) = repo.workdir() {
        std::fs::write(workdir.join(format!("{name}.rej")), rej)?;
    }
    Ok(())
}

/// The base, ours and theirs contents for a three-way fallback: the preimage
/// named by the `index` line, the current file, and the preimage patched.
fn three_way_inputs(
    repo: &Repository,
    f: &FilePatch,
    cached: bool,
) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    if f.is_binary() || f.created.is_some() || f.deleted.is_some() {
        return None;
    }
    let (pre, ..) = f.index.as_ref()?;
    let base = repo.revparse_single(pre).ok()?.peel_to_blob().ok()?;
    let base = base.content().to_vec();
    let theirs = patch_blob(&base, &f.hunks)?;
    let path = f.old.as_deref()?;
    let ours = if cached {
        let index = repo.index().ok()?;
        let entry = index.get_path(Path::new(path), 0)?;
        repo.find_blob(entry.id).ok()?.content().to_vec()
    } else {
        std::fs::read(repo.workdir()?.join(path)).ok()?
    };
    Some((base, ours, theirs))
}

/// `hunks` applied to `base` exactly where they say, or `None` if they do
/// not match it.
fn patch_blob(base: &[u8], hunks: &[Hunk]) -> Option<Vec<u8>> {
    let lines: Vec<&[u8]> = base.split_inclusive(|b| *b == b'\n').collect();
    let mut out: Vec<u8> = Vec::with_capacity(base.len());
    let mut pos = 0usize;
    for h in hunks {
        let start = if h.old.1 == 0 {
            h.old.0 as usize
        } else {
            (h.old.0 as usize).checked_sub(1)?
        };
        if start < pos || start > lines.len() {
            return None;
        }
        for l in &lines[pos..start] {
            out.extend_from_slice(l);
        }
        pos = start;
        let mut last_kept = false;
        for l in &h.lines {
            let (tag, body) = l.split_at(1);
            match tag {
                " " | "-" => {
                    let have = lines.get(pos)?;
                    if have.strip_suffix(b"\n").unwrap_or(have)
                        != body.trim_end_matches('\n').as_bytes()
                    {
                        return None;
                    }
                    if tag == " " {
                        out.extend_from_slice(have);
                    }
                    last_kept = tag == " ";
                    pos += 1;
                }
                "+" => {
                    out.extend_from_slice(body.as_bytes());
                    last_kept = true;
                }
                // `\ No newline at end of file` after a kept line.
                _ => {
                    if last_kept && out.last() == Some(&b'\n') {
                        out.pop();
                    }
                }
            }
        }
    }
    for l in &lines[pos..] {
        out.extend_from_slice(l);
    }
    Some(out)
}

/// Merge and write one file of a three-way apply; conflicts go to the index
/// as stages 1-3, as git leaves them. Returns whether it merged cleanly.
fn write_merge(
    repo: &Repository,
    f: &FilePatch,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    location: ApplyLocation,
    favor: Option<&str>,
) -> Result<bool, GitError> {
    let path = f.name();
    let input = |data| {
        let mut i = git2::MergeFileInput::new();
        i.content(data).path(path);
        i
    };
    let mut o = git2::MergeFileOptions::new();
    o.our_label("ours").their_label("theirs");
    match favor {
        Some("ours") => {
            o.favor(git2::FileFavor::Ours);
        }
        Some("theirs") => {
            o.favor(git2::FileFavor::Theirs);
        }
        Some("union") => {
            o.favor(git2::FileFavor::Union);
        }
        _ => {}
    }
    let result = git2::merge_file(&input(base), &input(ours), &input(theirs), Some(&mut o))?;
    let content = result.content().to_vec();
    let clean = result.is_automergeable();
    if !matches!(location, ApplyLocation::Index)
        && let Some(workdir) = repo.workdir()
    {
        std::fs::write(workdir.join(path), &content)?;
    }
    if !matches!(location, ApplyLocation::WorkDir) {
        let mut index = repo.index()?;
        let mode = index
            .get_path(Path::new(path), 0)
            .map_or(0o100644, |e| e.mode);
        let entry = |data: &[u8], stage: u16| -> Result<git2::IndexEntry, GitError> {
            Ok(git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode,
                uid: 0,
                gid: 0,
                file_size: data.len() as u32,
                id: repo.blob(data)?,
                flags: (stage << 12) | (path.len().min(0xfff) as u16),
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            })
        };
        if clean {
            index.add(&entry(&content, 0)?)?;
        } else {
            index.remove_path(Path::new(path))?;
            for (data, stage) in [(base, 1), (ours, 2), (theirs, 3)] {
                index.add(&entry(data, stage)?)?;
            }
        }
        index.write()?;
    }
    Ok(clean)
}

/// Apply outside any repository: patch the files under the current folder.
pub fn apply_outside(files: &[FilePatch], opts: &ApplyOpts) -> Result<String, GitError> {
    if opts.cached || opts.index || opts.three_way {
        return Err(GitError::Other(
            "--cached, --index and --3way need a repository".to_owned(),
        ));
    }
    let tmp = std::env::temp_dir().join(format!("rgit-apply-{}", std::process::id()));
    let repo = Repository::init_bare(&tmp)?;
    let result = std::env::current_dir()
        .map_err(GitError::from)
        .and_then(|cwd| Ok(repo.set_workdir(&cwd, false)?))
        .and_then(|()| apply(&repo, files, opts));
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_round_trips_and_strip_rewrites_paths() {
        let patch = "diff --git a/x/f b/x/f\nindex 1..2 100644\n--- a/x/f\n+++ b/x/f\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n\\ No newline at end of file\n";
        let opts = ApplyOpts::default();
        let mut f = parse_patch(patch.as_bytes(), &opts).unwrap();
        assert_eq!(f[0].render(), patch);
        f[0].reverse();
        f[0].reverse();
        assert_eq!(f[0].render(), patch);
        let p2 = ApplyOpts {
            strip: Some(2),
            directory: Some("d".into()),
            ..Default::default()
        };
        assert_eq!(parse_patch(patch.as_bytes(), &p2).unwrap()[0].name(), "d/f");
        assert_eq!(rename_label("s/a/x.rs", "s/b/x.rs"), "s/{a/x.rs => b/x.rs}");
        assert_eq!(
            patch_blob(b"a\nb", &f[0].hunks).as_deref(),
            Some(&b"a\nc"[..])
        );
    }
}
