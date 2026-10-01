//! `git apply`: patches are parsed here (so `-p`, `--directory`, `--include`,
//! `-R` and whitespace fixes work on any patch, binary ones included), then
//! applied through libgit2, with git's three-way and reject fallbacks.

use crate::rev::RevParse;
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
    /// Context lines that must still match (git's `-C<n>`); by default all.
    pub context: Option<usize>,
    /// Trust hunks without context to apply where their header says.
    pub unidiff_zero: bool,
    /// Match context lines ignoring changes in the amount of whitespace.
    pub ignore_whitespace: bool,
    /// The patch may lack newlines at the end of files.
    pub inaccurate_eof: bool,
    /// Let a hunk match lines an earlier hunk wrote.
    pub allow_overlap: bool,
    /// Record new files as intent-to-add in the index (working-tree apply).
    pub intent_to_add: bool,
    /// Write an index of the preimage blobs here instead of applying.
    pub fake_ancestor: Option<std::path::PathBuf>,
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
    /// The patch input it came from, as whitespace reports name it.
    pub source: String,
    /// The whitespace rule (ws.rs bits) `--whitespace=fix` fixes by.
    pub ws_rule: u32,
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
    /// Each line's number in the patch input, for whitespace reports.
    pub linenrs: Vec<usize>,
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
    pub(crate) fn render(&self) -> String {
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
            let mut out: Vec<usize> = Vec::with_capacity(h.lines.len());
            let mut i = 0;
            while i < h.lines.len() {
                if !h.lines[i].starts_with(['+', '-']) {
                    out.push(i);
                    i += 1;
                    continue;
                }
                let start = i;
                while i < h.lines.len() && !h.lines[i].starts_with(' ') {
                    i += 1;
                }
                let side = |c: char| {
                    let mut v = Vec::new();
                    for j in start..i {
                        if h.lines[j].starts_with(c) {
                            v.push(j);
                            if h.lines.get(j + 1).is_some_and(|n| n.starts_with('\\')) && j + 1 < i
                            {
                                v.push(j + 1);
                            }
                        }
                    }
                    v
                };
                out.extend(side('-'));
                out.extend(side('+'));
            }
            h.lines = out.iter().map(|&j| h.lines[j].clone()).collect();
            if h.linenrs.len() == out.len() {
                h.linenrs = out.iter().map(|&j| h.linenrs[j]).collect();
            }
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
pub(crate) fn unquote(s: &str) -> String {
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
            Some(b'a') => bytes.push(7),
            Some(b'b') => bytes.push(8),
            Some(b'v') => bytes.push(11),
            Some(b'f') => bytes.push(12),
            Some(b'r') => bytes.push(b'\r'),
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
                linenrs: Vec::new(),
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
                h.linenrs.push(i + 1);
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
pub(crate) fn wildmatch(glob: &str, path: &str) -> bool {
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

/// What `git apply` says about a patch's whitespace: `report` (each error
/// with its line, as the patch is read), then `summary` (after applying);
/// `fatal` when `--whitespace=error` stops it.
#[derive(Debug, Default)]
pub struct WsCheck {
    pub report: String,
    pub summary: String,
    pub fatal: bool,
}

/// git's whitespace checks of a patch under `action` (nowarn, warn, fix or
/// strip, error, error-all): added lines, and context lines too when
/// fixing, each against its file's rule (core.whitespace and the
/// `whitespace` attribute). Fixing rewrites the added lines; context lines
/// are fixed where the patch lands. `applying` is false for `--check` and
/// the like.
pub fn check_whitespace(
    files: &mut [FilePatch],
    action: &str,
    git_dir: Option<&Path>,
    reverse: bool,
    applying: bool,
) -> WsCheck {
    let mut out = WsCheck::default();
    if action == "nowarn" {
        return out;
    }
    let fixing = matches!(action, "fix" | "strip");
    let squelch = if action == "error-all" { 0 } else { 5 };
    let mut errors = 0;
    for f in files.iter_mut() {
        f.ws_rule =
            crate::ws::rule_for(git_dir, f.new.as_deref().or(f.old.as_deref()).unwrap_or(""));
        let old_side = if reverse { '+' } else { '-' };
        let crlf = f
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| (l.starts_with(' ') || l.starts_with(old_side)) && l.ends_with("\r\n"));
        if crlf {
            f.ws_rule |= crate::ws::CR_AT_EOL;
        }
        let source = if f.source.is_empty() || f.source == "-" {
            "<stdin>"
        } else {
            &f.source
        };
        for h in &f.hunks {
            for (k, l) in h.lines.iter().enumerate() {
                if !(l.starts_with('+') || (fixing && !reverse && l.starts_with(' '))) {
                    continue;
                }
                let bad = crate::ws::check(&l.as_bytes()[1..], f.ws_rule);
                if bad == 0 {
                    continue;
                }
                errors += 1;
                if squelch == 0 || errors <= squelch {
                    let body = l[1..].strip_suffix('\n').unwrap_or(&l[1..]);
                    let _ = writeln!(
                        out.report,
                        "{source}:{}: {}.\n{body}",
                        h.linenrs.get(k).copied().unwrap_or(0),
                        crate::ws::error_string(bad)
                    );
                }
            }
        }
    }
    if errors == 0 {
        return out;
    }
    let mut fixed = 0;
    if fixing {
        for f in files.iter_mut() {
            for h in &mut f.hunks {
                for l in h.lines.iter_mut().filter(|l| l.starts_with('+')) {
                    let (new, changed) = crate::ws::fix(&l.as_bytes()[1..], f.ws_rule);
                    if changed {
                        fixed += 1;
                        *l = format!("+{}", String::from_utf8_lossy(&new));
                    }
                }
            }
        }
    }
    if squelch > 0 && errors > squelch {
        let n = errors - squelch;
        let _ = writeln!(
            out.summary,
            "warning: squelched {n} whitespace error{}",
            if n == 1 { "" } else { "s" }
        );
    }
    let adds = |n: usize| {
        if n == 1 {
            "1 line adds whitespace errors.".to_owned()
        } else {
            format!("{n} lines add whitespace errors.")
        }
    };
    if action.starts_with("error") {
        out.fatal = true;
        let _ = writeln!(out.summary, "error: {}", adds(errors));
    } else if fixed > 0 && applying {
        let _ = writeln!(
            out.summary,
            "warning: {fixed} line{} applied after fixing whitespace errors.",
            if fixed == 1 { "" } else { "s" }
        );
    } else {
        let _ = writeln!(out.summary, "warning: {}", adds(errors));
    }
    out
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
    if let Some(path) = &opts.fake_ancestor {
        fake_ancestor(repo, files, path)?;
        return Ok(String::new());
    }
    let mut log = String::new();
    // First decide how each file goes, so a patch that cannot apply at all
    // changes nothing.
    enum Plan {
        Direct(String),
        Merge(Vec<u8>, Vec<u8>, Vec<u8>),
        Reject(Vec<Option<Hunk>>),
    }
    let mut plans = Vec::new();
    for f in files {
        if opts.verbose || opts.reject {
            let _ = writeln!(log, "Checking patch {}...", f.label());
        }
        // Hunks are placed as git places them; libgit2 then applies them
        // exactly there.
        let fitted = if f.created.is_some() || f.is_binary() || f.hunks.is_empty() {
            None
        } else {
            preimage(repo, f, opts.cached).map(|image| fit(&image, f, opts, &mut log))
        };
        let whole = match &fitted {
            None => Ok(f.clone()),
            Some(hunks) => match hunks.iter().position(Option::is_none) {
                None => Ok(FilePatch {
                    hunks: hunks.iter().flatten().cloned().collect(),
                    ..f.clone()
                }),
                Some(i) => Err(GitError::Other(format!(
                    "patch failed: {}:{}",
                    f.old.as_deref().unwrap_or_else(|| f.name()),
                    f.hunks[i].old.0
                ))),
            },
        };
        let plan = match whole.map(|w| w.render()).and_then(|t| fits(&t).map(|()| t)) {
            Ok(w) => Plan::Direct(w),
            Err(e) if opts.three_way => match three_way_inputs(repo, f, opts.cached) {
                Some((base, ours, theirs)) => Plan::Merge(base, ours, theirs),
                None => return Err(patch_failed(f, e)),
            },
            Err(_) if opts.reject => {
                Plan::Reject(fitted.unwrap_or_else(|| f.hunks.iter().cloned().map(Some).collect()))
            }
            Err(e) => return Err(patch_failed(f, e)),
        };
        plans.push(plan);
    }
    if opts.check {
        return Ok(log.trim_end().to_owned());
    }
    let direct: String = plans
        .iter()
        .filter_map(|p| match p {
            Plan::Direct(w) => Some(w.as_str()),
            _ => None,
        })
        .collect();
    if !direct.is_empty() {
        repo.apply(&Diff::from_buffer(direct.as_bytes())?, location, None)?;
    }
    if opts.intent_to_add && matches!(location, ApplyLocation::WorkDir) {
        intent_to_add(repo, files)?;
    }
    let mut failed = false;
    for (f, plan) in files.iter().zip(plans) {
        match plan {
            Plan::Direct(_) => {
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
            Plan::Reject(fitted) => {
                failed = true;
                apply_with_rejects(repo, f, &fitted, location, &mut log)?;
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
    match e {
        GitError::Other(m) if m.starts_with("patch failed: ") => {
            GitError::Other(format!("{m}\nerror: {}: patch does not apply", f.name()))
        }
        e => GitError::Other(format!("{}: patch does not apply ({e})", f.name())),
    }
}

/// Apply the hunks of `f` that fit (`fitted`, placed), one by one, and
/// write the rest to `<file>.rej` as git does.
fn apply_with_rejects(
    repo: &Repository,
    f: &FilePatch,
    fitted: &[Option<Hunk>],
    location: ApplyLocation,
    log: &mut String,
) -> Result<(), GitError> {
    let mut rejected = Vec::new();
    let mut outcomes = Vec::new();
    for (i, (h, placed)) in f.hunks.iter().zip(fitted).enumerate() {
        let applied = placed.as_ref().is_some_and(|placed| {
            let one = FilePatch {
                hunks: vec![placed.clone()],
                ..f.clone()
            };
            Diff::from_buffer(one.render().as_bytes()).is_ok_and(|diff| {
                repo.index().and_then(|mut i| i.read(true)).is_ok()
                    && repo.apply(&diff, location, None).is_ok()
            })
        });
        if applied {
            outcomes.push(format!("Hunk #{} applied cleanly.", i + 1));
        } else {
            outcomes.push(format!("Rejected hunk #{}.", i + 1));
            rejected.push(h);
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

/// The content `f` patches: its index blob (`--cached`) or its file.
fn preimage(repo: &Repository, f: &FilePatch, cached: bool) -> Option<Vec<u8>> {
    let path = f.old.as_deref()?;
    if cached {
        let entry = repo.index().ok()?.get_path(Path::new(path), 0)?;
        Some(repo.find_blob(entry.id).ok()?.content().to_vec())
    } else {
        std::fs::read(repo.workdir()?.join(path)).ok()
    }
}

/// One hunk line: in the preimage, the postimage, or both (context).
struct Part {
    pre: Option<Vec<u8>>,
    post: Option<Vec<u8>>,
}

/// Place `f`'s hunks in `image` as git's apply does: at the line the header
/// names or the nearest one around it that matches (never over lines an
/// earlier hunk wrote, unless `--allow-overlap`), dropping context down to
/// `-C` lines when needed. Each placed hunk is rewritten to match the image
/// exactly where it goes; one that fits nowhere is `None`.
fn fit(image: &[u8], f: &FilePatch, opts: &ApplyOpts, log: &mut String) -> Vec<Option<Hunk>> {
    // Each line of the image, and whether a hunk wrote it.
    let mut img: Vec<(Vec<u8>, bool)> = image
        .split_inclusive(|b| *b == b'\n')
        .map(|l| (l.to_vec(), false))
        .collect();
    let mut out = Vec::new();
    for (n, h) in f.hunks.iter().enumerate() {
        let mut parts: Vec<Part> = Vec::new();
        for l in &h.lines {
            let (tag, body) = (l.as_bytes()[0], l.as_bytes()[1..].to_vec());
            match tag {
                b' ' => parts.push(Part {
                    pre: Some(body.clone()),
                    post: Some(body),
                }),
                b'-' => parts.push(Part {
                    pre: Some(body),
                    post: None,
                }),
                b'+' => parts.push(Part {
                    pre: None,
                    post: Some(body),
                }),
                // `\ No newline at end of file` ends the line before it.
                _ => {
                    if let Some(p) = parts.last_mut() {
                        for s in [&mut p.pre, &mut p.post].into_iter().flatten() {
                            if s.last() == Some(&b'\n') {
                                s.pop();
                            }
                        }
                    }
                }
            }
        }
        if opts.inaccurate_eof {
            let last_pre = parts.iter().rposition(|p| p.pre.is_some());
            let last_post = parts.iter().rposition(|p| p.post.is_some());
            if let (Some(a), Some(b)) = (last_pre, last_post)
                && parts[a].pre.as_ref().is_some_and(|l| l.ends_with(b"\n"))
                && parts[b].post.as_ref().is_some_and(|l| l.ends_with(b"\n"))
            {
                parts[a].pre.as_mut().map(Vec::pop);
                parts[b].post.as_mut().map(Vec::pop);
            }
        }
        let common = |p: &Part| p.pre.is_some() && p.post.is_some();
        let leading = parts.iter().take_while(|p| common(p)).count();
        let trailing = parts.iter().rev().take_while(|p| common(p)).count();
        let (mut lead, mut trail) = (leading, trailing);
        let limit = opts.context.unwrap_or(usize::MAX);
        let mut beginning = h.old.0 == 0 || (h.old.0 == 1 && !opts.unidiff_zero);
        let mut end = !opts.unidiff_zero && trailing == 0;
        let mut pos = h.new.0 as isize - 1;
        let fix = matches!(opts.whitespace.as_deref(), Some("fix" | "strip")).then_some(f.ws_rule);
        let found = loop {
            if let Some(found) = find_pos(&img, &parts, pos, beginning, end, opts, fix) {
                break Some(found);
            }
            if lead <= limit && trail <= limit {
                break None;
            }
            if beginning || end {
                beginning = false;
                end = false;
                continue;
            }
            if lead >= trail {
                parts.remove(0);
                pos -= 1;
                lead -= 1;
            }
            if trail > lead {
                parts.pop();
                trail -= 1;
            }
        };
        let Some((at, how)) = found else {
            out.push(None);
            continue;
        };
        if opts.verbose && at as isize != pos {
            let offset = if opts.reverse {
                pos - at as isize
            } else {
                at as isize - pos
            };
            let _ = writeln!(
                log,
                "Hunk #{} succeeded at {} (offset {offset} line{}).",
                n + 1,
                at + 1,
                if offset == 1 { "" } else { "s" }
            );
        }
        if (lead, trail) != (leading, trailing) && !opts.quiet {
            let _ = writeln!(
                log,
                "Context reduced to ({lead}/{trail}) to apply fragment at {}",
                at + 1
            );
        }
        // The preimage is what the image holds; with whitespace fuzz the
        // context keeps the image's whitespace too, and matched by fixing
        // whitespace it takes the fixed line (update_pre_post_images).
        let mut k = at;
        for p in &mut parts {
            if let Some(pre) = &mut p.pre {
                *pre = img[k].0.clone();
                if p.post.is_some() {
                    match (how, fix) {
                        (Fit::Fuzzy, _) => p.post = Some(pre.clone()),
                        (Fit::Fixed, Some(rule)) => p.post = Some(crate::ws::fix(pre, rule).0),
                        _ => {}
                    }
                }
                k += 1;
            }
        }
        let post: Vec<(Vec<u8>, bool)> = parts
            .iter()
            .filter_map(|p| p.post.clone())
            .map(|l| (l, !opts.allow_overlap))
            .collect();
        let (old_len, new_len) = (k - at, post.len());
        img.splice(at..k, post);
        let mut lines = Vec::new();
        let mut push = |tag: char, body: &[u8]| {
            lines.push(format!(
                "{tag}{}\n",
                String::from_utf8_lossy(body).trim_end_matches('\n')
            ));
            if !body.ends_with(b"\n") {
                lines.push("\\ No newline at end of file\n".to_owned());
            }
        };
        for p in &parts {
            match (&p.pre, &p.post) {
                (Some(a), Some(b)) if a == b => push(' ', a),
                (a, b) => {
                    if let Some(a) = a {
                        push('-', a);
                    }
                    if let Some(b) = b {
                        push('+', b);
                    }
                }
            }
        }
        let start = at as u32 + 1;
        out.push(Some(Hunk {
            old: (start, old_len as u32),
            new: (start, new_len as u32),
            tail: h.tail.clone(),
            lines,
            linenrs: Vec::new(),
        }));
    }
    out
}

/// How a hunk's preimage matched the image.
#[derive(Clone, Copy)]
enum Fit {
    Exact,
    /// Equal but for the amount of whitespace (`--ignore-whitespace`).
    Fuzzy,
    /// Equal once whitespace errors are fixed (`--whitespace=fix`).
    Fixed,
}

/// git's `find_pos`: the line where `parts`' preimage matches `img`,
/// trying `line`, then one after, one before, two after, and so on; and
/// how it matched. With `fix` (`--whitespace=fix`'s rule) lines also match
/// when fixing their whitespace errors makes them equal.
fn find_pos(
    img: &[(Vec<u8>, bool)],
    parts: &[Part],
    line: isize,
    beginning: bool,
    end: bool,
    opts: &ApplyOpts,
    fix: Option<u32>,
) -> Option<(usize, Fit)> {
    let pre: Vec<&[u8]> = parts.iter().filter_map(|p| p.pre.as_deref()).collect();
    if pre.len() > img.len() {
        return None;
    }
    let line = if beginning {
        0
    } else if end {
        img.len() - pre.len()
    } else {
        // Before the start wraps around to the end, as in git.
        usize::try_from(line).map_or(img.len(), |l| l.min(img.len()))
    };
    let matches = |at: usize| -> Option<Fit> {
        if at + pre.len() > img.len()
            || (end && at + pre.len() != img.len())
            || (beginning && at != 0)
        {
            return None;
        }
        let lines = &img[at..at + pre.len()];
        if lines.iter().any(|(_, patched)| *patched) {
            return None;
        }
        // A last preimage line without its newline matches a line that has
        // one (git compares the preimage as a prefix of the image).
        let exact = lines.iter().zip(&pre).enumerate().all(|(i, ((l, _), p))| {
            l == p
                || (i + 1 == pre.len()
                    && !end
                    && !p.ends_with(b"\n")
                    && l.starts_with(p)
                    && l[p.len()..].iter().all(u8::is_ascii_whitespace))
        });
        if exact {
            return Some(Fit::Exact);
        }
        if opts.ignore_whitespace {
            return lines
                .iter()
                .zip(&pre)
                .all(|((l, _), p)| fuzzy_eq(l, p))
                .then_some(Fit::Fuzzy);
        }
        let rule = fix?;
        lines
            .iter()
            .zip(&pre)
            .all(|((l, _), p)| crate::ws::fix(l, rule).0 == crate::ws::fix(p, rule).0)
            .then_some(Fit::Fixed)
    };
    let (mut back, mut fwd, mut at) = (line, line, line);
    let mut i = 0usize;
    loop {
        if let Some(how) = matches(at) {
            return Some((at, how));
        }
        loop {
            if back == 0 && fwd == img.len() {
                return None;
            }
            if i & 1 == 1 {
                if back == 0 {
                    i += 1;
                    continue;
                }
                back -= 1;
                at = back;
            } else {
                if fwd == img.len() {
                    i += 1;
                    continue;
                }
                fwd += 1;
                at = fwd;
            }
            break;
        }
        i += 1;
    }
}

/// git's `fuzzy_matchlines`: equal but for the amount of whitespace, where
/// there is some on both sides, and line endings.
fn fuzzy_eq(a: &[u8], b: &[u8]) -> bool {
    let trim = |s: &[u8]| -> usize {
        s.iter()
            .rposition(|c| *c != b'\r' && *c != b'\n')
            .map_or(0, |i| i + 1)
    };
    let (a, b) = (&a[..trim(a)], &b[..trim(b)]);
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_whitespace() {
            if !b[j].is_ascii_whitespace() {
                return false;
            }
            while i < a.len() && a[i].is_ascii_whitespace() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
        } else if a[i] != b[j] {
            return false;
        } else {
            i += 1;
            j += 1;
        }
    }
    i == a.len() && j == b.len()
}

/// `git apply -N`: only files the patch creates go in the index, as
/// intent-to-add entries; modifications stay worktree-only (git 2.55
/// fixed its older mark-everything behavior, which also clobbered the
/// rest of the index, as an undocumented bug).
fn intent_to_add(repo: &Repository, files: &[FilePatch]) -> Result<(), GitError> {
    let mut index = repo.index()?;
    let written: Vec<(&str, u32)> = files
        .iter()
        .filter(|f| f.created.is_some())
        .filter_map(|f| {
            let path = f.new.as_deref()?;
            let mode = [f.new_mode.as_deref(), f.created.as_deref()]
                .into_iter()
                .flatten()
                .chain(f.index.as_ref().map(|(_, _, m)| m.trim()))
                .find_map(|m| u32::from_str_radix(m, 8).ok())
                .or_else(|| index.get_path(Path::new(path), 0).map(|e| e.mode))
                .unwrap_or(0o100644);
            Some((path, mode))
        })
        .collect();
    if written.is_empty() {
        return Ok(());
    }
    let empty = repo.blob(b"")?;
    for (path, mode) in written {
        index.add(&git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: empty,
            flags: path.len().min(0xfff) as u16,
            // GIT_INDEX_ENTRY_INTENT_TO_ADD
            flags_extended: 1 << 13,
            path: path.as_bytes().to_vec(),
        })?;
    }
    index.write()?;
    Ok(())
}

/// `git apply --build-fake-ancestor`: an index at `path` holding each
/// patched file's preimage blob, as the `index` lines name them.
fn fake_ancestor(repo: &Repository, files: &[FilePatch], path: &Path) -> Result<(), GitError> {
    let _ = std::fs::remove_file(path);
    let mut out = git2::Index::open(path)?;
    for f in files {
        let Some(name) = f.old.as_deref().filter(|_| f.created.is_none()) else {
            continue;
        };
        let (added, deleted) = f.counts();
        let mode_of = |m: &str| u32::from_str_radix(m.trim(), 8).ok();
        let (id, mode) = if added == 0 && deleted == 0 && !f.is_binary() {
            // A mode change only: the blob the index has now.
            let entry = repo.index()?.get_path(Path::new(name), 0).ok_or_else(|| {
                GitError::Other(format!(
                    "mode change for {name}, which is not in current HEAD"
                ))
            })?;
            let mode = f.old_mode.as_deref().and_then(mode_of);
            (entry.id, mode.unwrap_or(entry.mode))
        } else {
            let lacking =
                || GitError::Other(format!("sha1 information is lacking or useless ({name})."));
            let (pre, _, mode) = f.index.as_ref().ok_or_else(lacking)?;
            let id = repo
                .rev_single(pre)
                .ok()
                .filter(|o| o.kind() == Some(git2::ObjectType::Blob))
                .ok_or_else(lacking)?
                .id();
            let mode = f
                .old_mode
                .as_deref()
                .or(f.deleted.as_deref())
                .and_then(mode_of)
                .or_else(|| mode_of(mode))
                .unwrap_or(0o100644);
            (id, mode)
        };
        out.add(&git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            file_size: 0,
            id,
            flags: name.len().min(0xfff) as u16,
            flags_extended: 0,
            path: name.as_bytes().to_vec(),
        })?;
    }
    out.write()?;
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
    let base = repo.rev_single(pre).ok()?.peel_to_blob().ok()?;
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
        .and_then(|()| {
            let opts = ApplyOpts {
                intent_to_add: false,
                ..opts.clone()
            };
            apply(&repo, files, &opts)
        });
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
