//! git's `-p` hunk picker (add-patch.c) for `add`, `reset`, `checkout`,
//! `restore`, `stash` and `commit`: the same diff, prompts, keys, splitting,
//! editing, help and errors, applied through rgit's own `apply`.

use std::io::{BufRead, Write};
use std::sync::Arc;

use rgit_git::GitBackend;

/// Which command runs the picker, as git's `add_p_mode`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Add,
    Stash,
    Reset,
    Checkout,
    Worktree,
}

/// What happens to the chosen hunks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Apply {
    /// `git apply [-R] --cached`.
    Cached,
    /// `git apply [-R]` to the working tree.
    Worktree,
    /// Both the index and the working tree, checked first.
    Checkout,
    /// Handed back to the caller (stash builds its commit from it).
    Collect,
}

struct Mode {
    rev: Option<String>,
    cached: bool,
    reverse_diff: bool,
    apply: Apply,
    is_reverse: bool,
    prompts: [&'static str; 4],
    edit_hint: &'static str,
    help: &'static str,
}

macro_rules! mode_text {
    ($verb:literal, $target:literal, $what:literal, $done:literal) => {
        (
            [
                concat!($verb, " mode change", $target, " [y,n,q,a,d{},?]? "),
                concat!($verb, " deletion", $target, " [y,n,q,a,d{},?]? "),
                concat!($verb, " addition", $target, " [y,n,q,a,d{},?]? "),
                concat!($verb, " this hunk", $target, " [y,n,q,a,d{},?]? "),
            ],
            concat!(
                "If the patch applies cleanly, the edited hunk will immediately be marked for ",
                $done,
                "."
            ),
            concat!(
                "y - ",
                $what,
                "\nn - do not ",
                $what,
                "\nq - quit; do not ",
                $what,
                " or any of the remaining ones\na - ",
                $what,
                " and all later hunks in the file\nd - do not ",
                $what,
                " or any of the later hunks in the file\n"
            ),
        )
    };
}

impl Mode {
    /// git's run_add_p mode choice for `kind` and `rev`.
    fn new(kind: Kind, rev: Option<&str>) -> Self {
        let head = rev.is_none_or(|r| r == "HEAD");
        let (texts, rev, cached, reverse_diff, apply, is_reverse) = match (kind, rev) {
            (Kind::Add, _) => (
                mode_text!("Stage", "", "stage this hunk", "staging"),
                None,
                false,
                false,
                Apply::Cached,
                false,
            ),
            (Kind::Stash, _) => (
                mode_text!("Stash", "", "stash this hunk", "stashing"),
                Some("HEAD"),
                false,
                false,
                Apply::Collect,
                false,
            ),
            (Kind::Reset, _) if head => (
                mode_text!("Unstage", "", "unstage this hunk", "unstaging"),
                Some("HEAD"),
                true,
                false,
                Apply::Cached,
                true,
            ),
            (Kind::Reset, rev) => (
                mode_text!("Apply", " to index", "apply this hunk to index", "applying"),
                rev,
                true,
                true,
                Apply::Cached,
                false,
            ),
            (Kind::Checkout | Kind::Worktree, None) => (
                mode_text!(
                    "Discard",
                    " from worktree",
                    "discard this hunk from worktree",
                    "discarding"
                ),
                None,
                false,
                false,
                Apply::Worktree,
                true,
            ),
            (Kind::Checkout, _) if head => (
                mode_text!(
                    "Discard",
                    " from index and worktree",
                    "discard this hunk from index and worktree",
                    "discarding"
                ),
                Some("HEAD"),
                false,
                false,
                Apply::Checkout,
                true,
            ),
            (Kind::Checkout, rev) => (
                mode_text!(
                    "Apply",
                    " to index and worktree",
                    "apply this hunk to index and worktree",
                    "applying"
                ),
                rev,
                false,
                true,
                Apply::Checkout,
                false,
            ),
            (Kind::Worktree, _) if head => (
                mode_text!(
                    "Discard",
                    " from worktree",
                    "discard this hunk from worktree",
                    "discarding"
                ),
                Some("HEAD"),
                false,
                false,
                Apply::Worktree,
                true,
            ),
            (Kind::Worktree, rev) => (
                mode_text!(
                    "Apply",
                    " to worktree",
                    "apply this hunk to worktree",
                    "applying"
                ),
                rev,
                false,
                true,
                Apply::Worktree,
                false,
            ),
        };
        let (prompts, edit_hint, help) = texts;
        Self {
            rev: rev.map(str::to_owned),
            cached,
            reverse_diff,
            apply,
            is_reverse,
            prompts,
            edit_hint,
            help,
        }
    }
}

const HELP_REMAINDER: &str = "j - go to the next undecided hunk, roll over at the bottom\n\
J - go to the next hunk, roll over at the bottom\n\
k - go to the previous undecided hunk, roll over at the top\n\
K - go to the previous hunk, roll over at the top\n\
g - select a hunk to go to\n\
/ - search for a hunk matching the given regex\n\
s - split the current hunk into smaller hunks\n\
e - manually edit the current hunk\n\
p - print the current hunk\n\
P - print the current hunk using the pager\n\
? - print help\n";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Pick {
    #[default]
    Undecided,
    Skip,
    Use,
}

#[derive(Clone, Copy, Default)]
struct Header {
    old_offset: usize,
    old_count: usize,
    new_offset: usize,
    new_count: usize,
    extra_start: usize,
    extra_end: usize,
    colored_extra_start: usize,
    colored_extra_end: usize,
    suppress_colored_line_range: bool,
}

#[derive(Clone, Copy, Default)]
struct Hunk {
    start: usize,
    end: usize,
    colored_start: usize,
    colored_end: usize,
    splittable_into: usize,
    delta: isize,
    use_: Pick,
    header: Header,
}

#[derive(Default)]
struct FileDiff {
    head: Hunk,
    hunks: Vec<Hunk>,
    deleted: bool,
    added: bool,
    mode_change: bool,
    binary: bool,
}

pub(crate) struct Colors {
    pub(crate) on: bool,
    pub(crate) header: String,
    pub(crate) help: String,
    pub(crate) prompt: String,
    pub(crate) error: String,
    fraginfo: String,
    context: String,
    old: String,
    new: String,
    pub(crate) reset: String,
}

struct State<'a> {
    backend: &'a Arc<dyn GitBackend>,
    mode: Mode,
    plain: Vec<u8>,
    colored: Vec<u8>,
    files: Vec<FileDiff>,
    c: Colors,
    single_key: bool,
    answer: String,
    collected: Vec<u8>,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Empty context lines may omit the leading ' '.
fn normalize_marker(p: &[u8]) -> u8 {
    match p {
        [b'\n', ..] | [b'\r', b'\n', ..] => b' ',
        [c, ..] => *c,
        [] => 0,
    }
}

fn find_next_line(buf: &[u8], offset: usize) -> usize {
    match buf[offset..].iter().position(|&b| b == b'\n') {
        Some(i) => offset + i + 1,
        None => buf.len(),
    }
}

fn parse_range(p: &[u8], at: &mut usize) -> Option<(usize, usize)> {
    let digits = |at: usize| p[at..].iter().take_while(|b| b.is_ascii_digit()).count();
    let n = digits(*at);
    if n == 0 {
        return None;
    }
    let offset = std::str::from_utf8(&p[*at..*at + n]).ok()?.parse().ok()?;
    *at += n;
    if p.get(*at) != Some(&b',') {
        return Some((offset, 1));
    }
    let m = digits(*at + 1);
    if m == 0 {
        return None;
    }
    let count = std::str::from_utf8(&p[*at + 1..*at + 1 + m])
        .ok()?
        .parse()
        .ok()?;
    *at += 1 + m;
    Some((offset, count))
}

impl<'a> State<'a> {
    fn out(&self) -> std::io::StdoutLock<'static> {
        std::io::stdout().lock()
    }

    fn print(&self, text: &str) {
        let mut out = self.out();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    fn print_bytes(&self, bytes: &[u8]) {
        let mut out = self.out();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }

    /// Show `text` through git's pager (`P`).
    fn page(&self, text: &[u8]) {
        let env = |k: &str| std::env::var(k).ok();
        let pager = env("GIT_PAGER")
            .or_else(|| self.backend.config_get("core.pager").ok().flatten())
            .or_else(|| env("PAGER"))
            .unwrap_or_else(|| "less".to_owned());
        if pager.is_empty() || pager == "cat" {
            self.print_bytes(text);
            return;
        }
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&pager)
            .stdin(std::process::Stdio::piped());
        if env("LESS").is_none() {
            cmd.env("LESS", "FRX");
        }
        if env("LV").is_none() {
            cmd.env("LV", "-c");
        }
        match cmd.spawn() {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text);
                }
                let _ = child.wait();
            }
            Err(_) => self.print_bytes(text),
        }
    }

    fn err(&self, msg: &str) {
        self.print(&format!("{}{msg}{}\n", self.c.error, self.c.reset));
    }

    fn colored_line(&self, color: &str, text: &str) -> String {
        if color.is_empty() {
            format!("{text}\n")
        } else {
            format!("{color}{text}{}\n", self.c.reset)
        }
    }

    // ----- the diff -----

    fn parse_hunk_header(&mut self, f: usize, h: Option<usize>) -> anyhow::Result<()> {
        let plain = &self.plain;
        let hunk = match h {
            Some(i) => self.files[f].hunks[i],
            None => self.files[f].head,
        };
        let line_start = hunk.start;
        let eol = plain[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(plain.len(), |i| line_start + i);
        let line = &plain[line_start..eol];
        let bad = || {
            anyhow::anyhow!(
                "could not parse hunk header '{}'",
                String::from_utf8_lossy(line)
            )
        };
        let mut at = 0;
        if !line.starts_with(b"@@ -") {
            return Err(bad());
        }
        at += 4;
        let (oo, oc) = parse_range(line, &mut at).ok_or_else(bad)?;
        if !line[at..].starts_with(b" +") {
            return Err(bad());
        }
        at += 2;
        let (no, nc) = parse_range(line, &mut at).ok_or_else(bad)?;
        if !line[at..].starts_with(b" @@") {
            return Err(bad());
        }
        at += 3;
        let mut hunk = hunk;
        hunk.header.old_offset = oo;
        hunk.header.old_count = oc;
        hunk.header.new_offset = no;
        hunk.header.new_count = nc;
        hunk.start = eol + usize::from(eol < plain.len());
        hunk.header.extra_start = line_start + at;
        hunk.header.extra_end = hunk.start;
        if self.colored.is_empty() {
            hunk.header.colored_extra_start = 0;
            hunk.header.colored_extra_end = 0;
        } else {
            let colored = &self.colored;
            let cs = hunk.colored_start;
            let ceol = colored[cs..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(colored.len(), |i| cs + i);
            let cline = &colored[cs..ceol];
            match find(cline, b"@@ -")
                .and_then(|p| find(&cline[p + 4..], b" @@").map(|q| p + 4 + q))
            {
                Some(q) => hunk.header.colored_extra_start = cs + q + 3,
                None => {
                    hunk.header.colored_extra_start = cs;
                    hunk.header.suppress_colored_line_range = true;
                }
            }
            hunk.colored_start = ceol + usize::from(ceol < colored.len());
            hunk.header.colored_extra_end = hunk.colored_start;
        }
        match h {
            Some(i) => self.files[f].hunks[i] = hunk,
            None => self.files[f].head = hunk,
        }
        Ok(())
    }

    fn parse_diff(&mut self, paths: &[String]) -> anyhow::Result<()> {
        let mode = &self.mode;
        let mut plain = self.backend.patch_diff(
            mode.rev.as_deref(),
            mode.cached,
            mode.reverse_diff,
            None,
            paths,
        )?;
        if plain.is_empty() {
            return Ok(());
        }
        if !plain.ends_with(b"\n") {
            plain.push(b'\n');
        }
        self.plain = plain;
        if self.c.on {
            self.colored = colorize(&self.plain, &self.c);
        }
        let colored = !self.colored.is_empty();

        // `cur` is the hunk lines go to: None is the file's header.
        let mut cur: Option<usize> = None;
        let mut marker = 0u8;
        let mut p = 0;
        let mut cp = 0;
        let pend = self.plain.len();
        while p != pend {
            let eol = self.plain[p..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(pend, |i| p + i);
            let line = self.plain[p..eol].to_vec();
            let ch = normalize_marker(&self.plain[p..]);
            let mut mode_change = false;
            if line.starts_with(b"diff ") || line.starts_with(b"* Unmerged path ") {
                if let Some(f) = self.files.last_mut() {
                    complete_file(marker, hunk_mut(f, cur));
                }
                let mut f = FileDiff::default();
                f.head.start = p;
                if colored {
                    f.head.colored_start = cp;
                }
                self.files.push(f);
                cur = None;
                marker = 0;
            } else if self.files.is_empty() {
                anyhow::bail!(
                    "diff starts with unexpected line:\n{}",
                    String::from_utf8_lossy(&line)
                );
            } else if self.files.last().is_some_and(|f| f.deleted) {
                // Keep the rest of the file in a single "hunk".
            } else if line.starts_with(b"@@ ")
                || (cur.is_none() && line.starts_with(b"deleted file"))
            {
                let fi = self.files.len() - 1;
                if marker == b'-' || marker == b'+' {
                    hunk_mut(&mut self.files[fi], cur).splittable_into += 1;
                }
                let f = &mut self.files[fi];
                let mut h = Hunk {
                    start: p,
                    ..Hunk::default()
                };
                if colored {
                    h.colored_start = cp;
                }
                f.hunks.push(h);
                let hi = f.hunks.len() - 1;
                cur = Some(hi);
                if line.starts_with(b"deleted file") {
                    f.deleted = true;
                } else {
                    self.parse_hunk_header(fi, Some(hi))?;
                }
                marker = ch;
            } else if cur.is_none() && line.starts_with(b"new file") {
                self.files.last_mut().expect("file").added = true;
            } else if cur.is_none()
                && line
                    .strip_prefix(b"old mode ")
                    .is_some_and(|m| !m.is_empty() && m.iter().all(|b| (b'0'..=b'7').contains(b)))
            {
                let f = self.files.last_mut().expect("file");
                f.mode_change = true;
                let mut h = Hunk {
                    start: p,
                    ..Hunk::default()
                };
                if colored {
                    h.colored_start = cp;
                }
                f.hunks.push(h);
                mode_change = true;
            } else if cur.is_none()
                && line
                    .strip_prefix(b"new mode ")
                    .is_some_and(|m| !m.is_empty() && m.iter().all(|b| (b'0'..=b'7').contains(b)))
            {
                mode_change = true;
            } else if cur.is_none() && line.starts_with(b"Binary files ") {
                self.files.last_mut().expect("file").binary = true;
            }

            let fi = self.files.len() - 1;
            if (marker == b'-' || marker == b'+') && ch == b' ' {
                hunk_mut(&mut self.files[fi], cur).splittable_into += 1;
            }
            if marker != 0 && ch != b'\\' {
                marker = ch;
            }
            p = if eol == pend { pend } else { eol + 1 };
            let f = &mut self.files[fi];
            hunk_mut(f, cur).end = p;
            if colored {
                cp = find_next_line(&self.colored, cp.min(self.colored.len() - 1));
                hunk_mut(f, cur).colored_end = cp;
            }
            if mode_change {
                let (end, cend) = (f.head.end, f.head.colored_end);
                f.hunks[0].end = end;
                if colored {
                    f.hunks[0].colored_end = cend;
                }
            }
        }
        if let Some(f) = self.files.last_mut() {
            complete_file(marker, hunk_mut(f, cur));
        }
        Ok(())
    }

    // ----- rendering -----

    fn render_hunk(&self, hunk: &Hunk, delta: isize, colored: bool, out: &mut Vec<u8>) {
        let header = &hunk.header;
        if header.old_offset != 0 || header.new_offset != 0 {
            let extra: &[u8] = if !colored {
                &self.plain[header.extra_start..header.extra_end]
            } else if header.suppress_colored_line_range {
                out.extend_from_slice(
                    &self.colored[header.colored_extra_start..header.colored_extra_end],
                );
                out.extend_from_slice(&self.colored[hunk.colored_start..hunk.colored_end]);
                return;
            } else {
                out.extend_from_slice(self.c.fraginfo.as_bytes());
                &self.colored[header.colored_extra_start..header.colored_extra_end]
            };
            let (mut old_offset, mut new_offset) =
                (header.old_offset as isize, header.new_offset as isize);
            if self.mode.is_reverse {
                old_offset -= delta;
            } else {
                new_offset += delta;
            }
            let mut text = format!("@@ -{old_offset}");
            if header.old_count != 1 {
                text.push_str(&format!(",{}", header.old_count));
            }
            text.push_str(&format!(" +{new_offset}"));
            if header.new_count != 1 {
                text.push_str(&format!(",{}", header.new_count));
            }
            text.push_str(" @@");
            out.extend_from_slice(text.as_bytes());
            if !extra.is_empty() {
                out.extend_from_slice(extra);
            } else if colored {
                out.extend_from_slice(format!("{}\n", self.c.reset).as_bytes());
            } else {
                out.push(b'\n');
            }
        }
        if colored {
            out.extend_from_slice(&self.colored[hunk.colored_start..hunk.colored_end]);
        } else {
            out.extend_from_slice(&self.plain[hunk.start..hunk.end]);
        }
    }

    fn render_diff_header(&self, f: &FileDiff, colored: bool, out: &mut Vec<u8>) {
        let skip_mode_change = f.mode_change && f.hunks[0].use_ != Pick::Use;
        let head = &f.head;
        if !skip_mode_change {
            self.render_hunk(head, 0, colored, out);
            return;
        }
        let first = &f.hunks[0];
        if colored {
            out.extend_from_slice(&self.colored[head.colored_start..first.colored_start]);
            out.extend_from_slice(&self.colored[first.colored_end..head.colored_end]);
        } else {
            out.extend_from_slice(&self.plain[head.start..first.start]);
            out.extend_from_slice(&self.plain[first.end..head.end]);
        }
    }

    /// Coalesce hunks again that were split.
    fn merge_hunks(
        &mut self,
        f: usize,
        index: &mut usize,
        use_all: bool,
        merged: &mut Hunk,
    ) -> anyhow::Result<bool> {
        let mut i = *index;
        let hunk = self.files[f].hunks[i];
        if !use_all && hunk.use_ != Pick::Use {
            return Ok(false);
        }
        *merged = hunk;
        merged.colored_start = 0;
        merged.colored_end = 0;
        while i + 1 < self.files[f].hunks.len() {
            let next_hunk = self.files[f].hunks[i + 1];
            let next = next_hunk.header;
            let header = merged.header;
            if (!use_all && next_hunk.use_ != Pick::Use)
                || header.new_offset as isize >= next.new_offset as isize + merged.delta
                || ((header.new_offset + header.new_count) as isize)
                    < next.new_offset as isize + merged.delta
            {
                break;
            }
            i += 1;
            let delta;
            if merged.start < next_hunk.start && merged.end > next_hunk.start {
                merged.end = next_hunk.end;
                merged.colored_end = next_hunk.colored_end;
                delta = 0;
            } else {
                let overlapping = (header.new_offset + header.new_count) as isize
                    - merged.delta
                    - next.new_offset as isize;
                let mut overlap_end = next_hunk.start;
                let mut overlap_start = overlap_end;
                for j in 0..overlapping.max(0) as usize {
                    let overlap_next = find_next_line(&self.plain, overlap_end);
                    if overlap_next > next_hunk.end {
                        anyhow::bail!(
                            "failed to find {overlapping} context lines in:\n{}",
                            String::from_utf8_lossy(&self.plain[next_hunk.start..next_hunk.end])
                        );
                    }
                    if normalize_marker(&self.plain[overlap_end..]) != b' ' {
                        anyhow::bail!(
                            "expected context line #{} in\n{}",
                            j + 1,
                            String::from_utf8_lossy(&self.plain[next_hunk.start..next_hunk.end])
                        );
                    }
                    overlap_start = overlap_end;
                    overlap_end = overlap_next;
                }
                let len = overlap_end - overlap_start;
                if len > merged.end - merged.start
                    || self.plain[merged.end - len..merged.end]
                        != self.plain[overlap_start..overlap_end]
                {
                    anyhow::bail!(
                        "hunks do not overlap:\n{}\n\tdoes not end with:\n{}",
                        String::from_utf8_lossy(&self.plain[merged.start..merged.end]),
                        String::from_utf8_lossy(&self.plain[overlap_start..overlap_end])
                    );
                }
                if merged.end != self.plain.len() {
                    let start = self.plain.len();
                    let copy = self.plain[merged.start..merged.end].to_vec();
                    self.plain.extend_from_slice(&copy);
                    merged.start = start;
                    merged.end = self.plain.len();
                }
                let tail = self.plain[overlap_end..next_hunk.end].to_vec();
                self.plain.extend_from_slice(&tail);
                merged.end = self.plain.len();
                merged.splittable_into += next_hunk.splittable_into;
                delta = merged.delta;
                merged.delta += next_hunk.delta;
            }
            merged.header.old_count = next.old_offset + next.old_count - merged.header.old_offset;
            merged.header.new_count = (next.new_offset as isize + delta) as usize + next.new_count
                - merged.header.new_offset;
        }
        if i == *index {
            return Ok(false);
        }
        *index = i;
        Ok(true)
    }

    fn reassemble_patch(&mut self, f: usize, use_all: bool) -> anyhow::Result<Vec<u8>> {
        let mut out = Vec::new();
        let save_len = self.plain.len();
        self.render_diff_header(&self.files[f], false, &mut out);
        let mut delta: isize = 0;
        let mut i = usize::from(self.files[f].mode_change);
        while i < self.files[f].hunks.len() {
            let hunk = self.files[f].hunks[i];
            if !use_all && hunk.use_ != Pick::Use {
                delta += hunk.header.old_count as isize - hunk.header.new_count as isize;
            } else {
                let mut merged = Hunk::default();
                let rendered = if self.merge_hunks(f, &mut i, use_all, &mut merged)? {
                    merged
                } else {
                    hunk
                };
                self.render_hunk(&rendered, delta, false, &mut out);
                self.plain.truncate(save_len);
                delta += rendered.delta;
            }
            i += 1;
        }
        Ok(out)
    }

    fn split_hunk(&mut self, f: usize, index: usize) {
        let colored = !self.colored.is_empty();
        let hunk = self.files[f].hunks[index];
        if hunk.splittable_into < 2 {
            return;
        }
        let mut splittable_into = hunk.splittable_into;
        let end = hunk.end;
        let colored_end = hunk.colored_end;
        let mut remaining = hunk.header;
        let mut parts = vec![Hunk::default(); splittable_into];
        parts[0] = hunk;
        parts[0].splittable_into = 1;
        parts[0].header.old_count = 0;
        parts[0].header.new_count = 0;
        let mut k = 0;
        let mut current = hunk.start;
        let mut colored_current = if colored { hunk.colored_start } else { 0 };
        let mut marker = 0u8;
        let mut context_line_count = 0;
        let mut first = true;
        while splittable_into > 1 {
            let mut ch = normalize_marker(&self.plain[current..]);
            if (marker == b'-' || marker == b'+') && ch == b' ' {
                first = false;
                parts[k + 1].start = current;
                if colored {
                    parts[k + 1].colored_start = colored_current;
                }
                context_line_count = 0;
            }
            let new_hunk_here = marker == b' ' && (ch == b'-' || ch == b'+');
            if !new_hunk_here || first {
                if new_hunk_here && first {
                    parts[k].header.old_count = context_line_count;
                    parts[k].header.new_count = context_line_count;
                    context_line_count = 0;
                    first = false;
                }
                if ch == b'\\' {
                    ch = if marker != 0 { marker } else { b' ' };
                }
                match ch {
                    b' ' => context_line_count += 1,
                    b'-' => parts[k].header.old_count += 1,
                    _ => parts[k].header.new_count += 1,
                }
                marker = ch;
                current = find_next_line(&self.plain, current);
                if colored {
                    colored_current = find_next_line(&self.colored, colored_current);
                }
                continue;
            }
            remaining.old_offset += parts[k].header.old_count;
            remaining.old_count -= parts[k].header.old_count;
            remaining.new_offset += parts[k].header.new_count;
            remaining.new_count -= parts[k].header.new_count;
            parts[k + 1].header.old_offset = parts[k].header.old_offset + parts[k].header.old_count;
            parts[k + 1].header.new_offset = parts[k].header.new_offset + parts[k].header.new_count;
            parts[k].header.old_count += context_line_count;
            parts[k].header.new_count += context_line_count;
            parts[k].end = current;
            if colored {
                parts[k].colored_end = colored_current;
            }
            k += 1;
            parts[k].splittable_into = 1;
            parts[k].use_ = parts[k - 1].use_;
            parts[k].header.old_count = context_line_count;
            parts[k].header.new_count = context_line_count;
            context_line_count = 0;
            splittable_into -= 1;
            marker = ch;
        }
        parts[k].header.old_count = remaining.old_count;
        parts[k].header.new_count = remaining.new_count;
        parts[k].end = end;
        if colored {
            parts[k].colored_end = colored_end;
        }
        self.files[f].hunks.splice(index..=index, parts);
    }

    fn recolor_hunk(&mut self, hunk: &mut Hunk) {
        if self.colored.is_empty() {
            return;
        }
        hunk.colored_start = self.colored.len();
        let mut current = hunk.start;
        while current < hunk.end {
            let mut eol = current;
            while eol < hunk.end && self.plain[eol] != b'\n' {
                eol += 1;
            }
            let next = eol + usize::from(eol < hunk.end);
            if eol > current && self.plain[eol - 1] == b'\r' {
                eol -= 1;
            }
            let color = match self.plain[current] {
                b'-' => &self.c.old,
                b'+' => &self.c.new,
                _ => &self.c.context,
            };
            let mut line = color.as_bytes().to_vec();
            line.extend_from_slice(&self.plain[current..eol]);
            line.extend_from_slice(self.c.reset.as_bytes());
            if next > eol {
                line.extend_from_slice(&self.plain[eol..next]);
            }
            self.colored.extend_from_slice(&line);
            current = next;
        }
        hunk.colored_end = self.colored.len();
    }

    /// 0 when the user emptied the edit, 1 when it was taken.
    fn edit_hunk_manually(&mut self, hunk: &mut Hunk) -> anyhow::Result<i32> {
        let mut buf = b"# Manual hunk edit mode -- see bottom for a quick guide.\n".to_vec();
        self.render_hunk(hunk, 0, false, &mut buf);
        let (rm, add) = if self.mode.is_reverse {
            ('+', '-')
        } else {
            ('-', '+')
        };
        buf.extend_from_slice(
            format!(
                "# ---\n# To remove '{rm}' lines, make them ' ' lines (context).\n\
                 # To remove '{add}' lines, delete them.\n\
                 # Lines starting with # will be removed.\n# {}\n\
                 # If it does not apply cleanly, you will be given an opportunity to\n\
                 # edit again.  If all lines of the hunk are removed, then the edit is\n\
                 # aborted and the hunk is left unchanged.\n",
                self.mode.edit_hint
            )
            .as_bytes(),
        );
        let path = self.backend.git_dir().join("addp-hunk-edit.diff");
        std::fs::write(&path, &buf)?;
        crate::interactive::launch_editor(self.backend, &path)?;
        let edited = std::fs::read(&path)?;
        hunk.start = self.plain.len();
        let mut i = 0;
        while i < edited.len() {
            let next = find_next_line(&edited, i);
            if !edited[i..].starts_with(b"#") {
                self.plain.extend_from_slice(&edited[i..next]);
            }
            i = next;
        }
        hunk.end = self.plain.len();
        if hunk.end == hunk.start {
            return Ok(0);
        }
        self.recolor_hunk(hunk);
        if self.plain[hunk.start] == b'@' {
            // Parse the edited header in place of the old one.
            let fake = FileDiff {
                head: *hunk,
                ..FileDiff::default()
            };
            self.files.push(fake);
            let f = self.files.len() - 1;
            let parsed = self.parse_hunk_header(f, None);
            let fake = self.files.pop().expect("pushed");
            if parsed.is_err() {
                anyhow::bail!("could not parse hunk header");
            }
            *hunk = fake.head;
        }
        Ok(1)
    }

    fn recount_edited_hunk(&self, hunk: &mut Hunk, old: usize, new: usize) -> isize {
        let h = &mut hunk.header;
        h.old_count = 0;
        h.new_count = 0;
        let mut i = hunk.start;
        while i < hunk.end {
            match normalize_marker(&self.plain[i..]) {
                b'-' => h.old_count += 1,
                b'+' => h.new_count += 1,
                b' ' => {
                    h.old_count += 1;
                    h.new_count += 1;
                }
                _ => {}
            }
            i = find_next_line(&self.plain, i);
        }
        old as isize - new as isize - h.old_count as isize + h.new_count as isize
    }

    fn apply_opts(&self, cached: bool, check: bool) -> rgit_git::ApplyOpts {
        rgit_git::ApplyOpts {
            cached,
            check,
            reverse: self.mode.is_reverse,
            quiet: true,
            ..Default::default()
        }
    }

    /// `git apply [--cached] [--check] [-R]` on `patch`.
    fn apply(&self, patch: &[u8], cached: bool, check: bool) -> bool {
        let opts = self.apply_opts(cached, check);
        let Ok(files) = rgit_git::parse_patch(patch, &opts) else {
            return false;
        };
        if self.backend.apply_patch(&files, &opts).is_ok() {
            return true;
        }
        // git apply's own complaint: the first hunk that does not fit.
        let check = rgit_git::ApplyOpts {
            check: true,
            ..opts.clone()
        };
        for f in &files {
            let name = f.name().to_owned();
            let failing = (0..f.hunks.len()).find(|&i| {
                let part = rgit_git::FilePatch {
                    hunks: f.hunks[..=i].to_vec(),
                    ..f.clone()
                };
                self.backend.apply_patch(&[part], &check).is_err()
            });
            if let Some(i) = failing {
                let h = &f.hunks[i];
                let line = if self.mode.is_reverse {
                    h.new.0
                } else {
                    h.old.0
                };
                let _ = std::io::stdout().flush();
                eprintln!("error: patch failed: {name}:{line}");
                eprintln!("error: {name}: patch does not apply");
                break;
            }
        }
        false
    }

    fn run_apply_check(&mut self, f: usize) -> anyhow::Result<bool> {
        let patch = self.reassemble_patch(f, true)?;
        Ok(match self.mode.apply {
            Apply::Collect => true,
            Apply::Cached => self.apply(&patch, true, true),
            Apply::Worktree | Apply::Checkout => self.apply(&patch, false, true),
        })
    }

    fn read_answer(&mut self) -> bool {
        self.answer.clear();
        if self.single_key
            && let Some(c) = read_key()
        {
            self.answer.push(c);
            self.print(&format!("{c}\n"));
            return true;
        }
        match read_stdin_line() {
            Some(line) => {
                self.answer = line;
                true
            }
            None => false,
        }
    }

    fn prompt_yesno(&mut self, prompt: &str) -> i32 {
        loop {
            let text = if self.c.prompt.is_empty() {
                prompt.to_owned()
            } else {
                format!("{}{prompt}{}", self.c.prompt, self.c.reset)
            };
            self.print(&text);
            if !self.read_answer() {
                return -1;
            }
            match self.answer.chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('n') => return 0,
                Some('y') => return 1,
                _ => {}
            }
        }
    }

    /// 0 when the edit was taken, -1 when it was given up.
    fn edit_hunk_loop(&mut self, f: usize, index: usize) -> anyhow::Result<i32> {
        let (plain_len, colored_len) = (self.plain.len(), self.colored.len());
        let backup = self.files[f].hunks[index];
        loop {
            let mut hunk = self.files[f].hunks[index];
            let res = self.edit_hunk_manually(&mut hunk)?;
            if res == 0 {
                self.files[f].hunks[index] = backup;
                return Ok(-1);
            }
            hunk.delta += self.recount_edited_hunk(
                &mut hunk,
                backup.header.old_count,
                backup.header.new_count,
            );
            self.files[f].hunks[index] = hunk;
            if self.run_apply_check(f)? {
                return Ok(0);
            }
            self.plain.truncate(plain_len);
            self.colored.truncate(colored_len);
            self.files[f].hunks[index] = backup;
            if self.prompt_yesno(
                "Your edited hunk does not apply. Edit again (saying \"no\" discards!) [y/n]? ",
            ) < 1
            {
                return Ok(-1);
            }
        }
    }

    fn apply_for_checkout(&mut self, patch: &[u8]) {
        let applies_index = self.apply(patch, true, true);
        let applies_worktree = self.apply(patch, false, true);
        if applies_index && applies_worktree {
            self.apply(patch, true, false);
            self.apply(patch, false, false);
            return;
        }
        if !applies_index {
            self.err("The selected hunks do not apply to the index!");
            if self.prompt_yesno("Apply them to the worktree anyway? ") > 0 {
                self.apply(patch, false, false);
                return;
            }
            self.err("Nothing was applied.\n");
        } else {
            self.print_bytes(patch);
        }
    }

    fn summarize_hunk(&self, hunk: &Hunk, out: &mut String) {
        let len = out.len();
        let h = &hunk.header;
        out.push_str(&format!(
            " -{},{} +{},{} ",
            h.old_offset, h.old_count, h.new_offset, h.new_count
        ));
        if out.len() - len < 20 {
            let pad = 20 + len - out.len();
            out.push_str(&" ".repeat(pad));
        }
        let mut i = hunk.start;
        while i < hunk.end && self.plain[i] == b' ' {
            i = find_next_line(&self.plain, i);
        }
        if i < hunk.end {
            let next = find_next_line(&self.plain, i);
            out.push_str(&String::from_utf8_lossy(&self.plain[i..next]));
        }
        if out.len() - len > 80 {
            let mut cut = len + 80;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    fn display_hunks(&self, f: usize, start: usize) -> usize {
        let hunks = &self.files[f].hunks;
        let end = (start + 20).min(hunks.len());
        for (n, hunk) in hunks.iter().enumerate().take(end).skip(start) {
            let sign = match hunk.use_ {
                Pick::Use => '+',
                Pick::Skip => '-',
                Pick::Undecided => ' ',
            };
            let mut line = format!("{sign}{:2}: ", n + 1);
            self.summarize_hunk(hunk, &mut line);
            self.print(&line);
        }
        end
    }

    /// Ask about each hunk of file `f`; true when the user quit.
    fn patch_update_file(&mut self, f: usize) -> anyhow::Result<bool> {
        let colored = !self.colored.is_empty();
        if self.files[f].hunks.is_empty() && !self.files[f].added {
            return Ok(false);
        }
        let mut buf = Vec::new();
        self.render_diff_header(&self.files[f], colored, &mut buf);
        self.print_bytes(&buf);
        let mut hunk_index = 0usize;
        let mut rendered: Option<usize> = None;
        let mut quit = false;
        let mut use_pager = false;
        // As in git, what was once allowed in this file stays allowed.
        let mut permitted = 0u8;
        const PREV: u8 = 1;
        const PREV_UNDECIDED: u8 = 2;
        const NEXT: u8 = 4;
        const NEXT_UNDECIDED: u8 = 8;
        const SEARCH: u8 = 16;
        const SPLIT: u8 = 32;
        const EDIT: u8 = 64;
        loop {
            let n = self.files[f].hunks.len();
            if hunk_index >= n {
                hunk_index = 0;
            }
            // git scans for the nearest undecided hunk circularly, so k/j
            // are offered whenever any other hunk is still undecided.
            let (mut undecided_previous, mut undecided_next) = (None, None);
            if n > 0 {
                for step in 1..n {
                    let i = (hunk_index + n - step) % n;
                    if self.files[f].hunks[i].use_ == Pick::Undecided {
                        undecided_previous = Some(i);
                        break;
                    }
                }
                for step in 1..n {
                    let i = (hunk_index + step) % n;
                    if self.files[f].hunks[i].use_ == Pick::Undecided {
                        undecided_next = Some(i);
                        break;
                    }
                }
            }
            let current_use = if n > 0 {
                self.files[f].hunks[hunk_index].use_
            } else {
                self.files[f].head.use_
            };
            if undecided_previous.is_none()
                && undecided_next.is_none()
                && current_use != Pick::Undecided
            {
                break;
            }
            let mut keys = String::new();
            if n > 0 {
                if rendered != Some(hunk_index) {
                    let mut buf = Vec::new();
                    self.render_hunk(&self.files[f].hunks[hunk_index], 0, colored, &mut buf);
                    if use_pager {
                        self.page(&buf);
                        use_pager = false;
                    } else {
                        self.print_bytes(&buf);
                    }
                    rendered = Some(hunk_index);
                }
                if undecided_previous.is_some() {
                    permitted |= PREV_UNDECIDED;
                    keys.push_str(",k");
                }
                if n > 1 {
                    permitted |= PREV;
                    keys.push_str(",K");
                }
                if undecided_next.is_some() {
                    permitted |= NEXT_UNDECIDED;
                    keys.push_str(",j");
                }
                if n > 1 {
                    permitted |= NEXT;
                    keys.push_str(",J");
                }
                if n > 1 {
                    permitted |= SEARCH;
                    keys.push_str(",g,/");
                }
                if self.files[f].hunks[hunk_index].splittable_into > 1 {
                    permitted |= SPLIT;
                    keys.push_str(",s");
                }
                if hunk_index + 1 > usize::from(self.files[f].mode_change) && !self.files[f].deleted
                {
                    permitted |= EDIT;
                    keys.push_str(",e");
                }
                keys.push_str(",p,P");
            }
            let file = &self.files[f];
            let prompt = if file.deleted {
                1
            } else if file.added {
                2
            } else if file.mode_change && hunk_index == 0 {
                0
            } else {
                3
            };
            let text = self.mode.prompts[prompt].replacen("{}", &keys, 1);
            self.print(&format!(
                "{}({}/{}) {text}{}",
                self.c.prompt,
                hunk_index + 1,
                n.max(1),
                self.c.reset
            ));
            if !self.read_answer() {
                break;
            }
            if self.answer.is_empty() {
                continue;
            }
            let first = self.answer.chars().next().expect("non-empty");
            let ch = first.to_ascii_lowercase();
            if self.answer.chars().count() != 1 && ch != 'g' && ch != '/' {
                self.err(&format!(
                    "Only one letter is expected, got '{}'",
                    self.answer
                ));
                continue;
            }
            let soft_increment = |i: &mut usize| *i = undecided_next.unwrap_or(n);
            match first {
                _ if ch == 'y' => {
                    self.set_use(f, hunk_index, Pick::Use);
                    soft_increment(&mut hunk_index);
                }
                _ if ch == 'n' => {
                    self.set_use(f, hunk_index, Pick::Skip);
                    soft_increment(&mut hunk_index);
                }
                _ if ch == 'a' || ch == 'd' || ch == 'q' => {
                    let to = if ch == 'a' { Pick::Use } else { Pick::Skip };
                    if n > 0 {
                        while hunk_index < n {
                            if self.files[f].hunks[hunk_index].use_ == Pick::Undecided {
                                self.files[f].hunks[hunk_index].use_ = to;
                            }
                            hunk_index += 1;
                        }
                    } else if self.files[f].head.use_ == Pick::Undecided {
                        self.files[f].head.use_ = to;
                    }
                    if ch == 'q' {
                        quit = true;
                        break;
                    }
                }
                'K' => {
                    if permitted & PREV != 0 {
                        hunk_index = (hunk_index + n - 1) % n;
                    } else {
                        self.err("No previous hunk");
                    }
                }
                'J' => {
                    if permitted & NEXT != 0 {
                        hunk_index += 1;
                    } else {
                        self.err("No next hunk");
                    }
                }
                'k' => {
                    if permitted & PREV_UNDECIDED != 0 {
                        hunk_index = undecided_previous.unwrap_or(usize::MAX);
                    } else {
                        self.err("No previous hunk");
                    }
                }
                'j' => {
                    if permitted & NEXT_UNDECIDED != 0 {
                        hunk_index = undecided_next.unwrap_or(usize::MAX);
                    } else {
                        self.err("No next hunk");
                    }
                }
                'g' => {
                    if permitted & SEARCH == 0 {
                        self.err("No other hunks to goto");
                        continue;
                    }
                    let mut answer = self.answer[1..].trim().to_owned();
                    let mut i = hunk_index
                        .saturating_sub(10)
                        .max(usize::from(self.files[f].mode_change));
                    let mut eof = false;
                    while answer.is_empty() {
                        i = self.display_hunks(f, i);
                        self.print(if i < n {
                            "go to which hunk (<ret> to see more)? "
                        } else {
                            "go to which hunk? "
                        });
                        match read_stdin_line() {
                            Some(line) => answer = line,
                            None => {
                                eof = true;
                                break;
                            }
                        }
                    }
                    let answer = answer.trim();
                    match answer.parse::<usize>() {
                        _ if eof && answer.is_empty() => {
                            self.err("Invalid number: ''");
                        }
                        Ok(r) if r > 0 && r <= n => hunk_index = r - 1,
                        Ok(_) => self.err(&format!(
                            "Sorry, only {n} hunk{} available.",
                            if n == 1 { "" } else { "s" }
                        )),
                        Err(_) => self.err(&format!("Invalid number: '{answer}'")),
                    }
                }
                '/' => {
                    if permitted & SEARCH == 0 {
                        self.err("No other hunks to search");
                        continue;
                    }
                    let mut pattern = self.answer[1..].to_owned();
                    if pattern.is_empty() {
                        self.print("search for regex? ");
                        match read_stdin_line() {
                            Some(line) => pattern = line,
                            None => break,
                        }
                        if pattern.is_empty() {
                            continue;
                        }
                    }
                    let re = match regex::bytes::RegexBuilder::new(&pattern)
                        .multi_line(true)
                        .build()
                    {
                        Ok(re) => re,
                        Err(e) => {
                            self.err(&format!("Malformed search regexp {pattern}: {e}"));
                            continue;
                        }
                    };
                    let mut i = hunk_index;
                    loop {
                        let mut buf = Vec::new();
                        self.render_hunk(&self.files[f].hunks[i], 0, false, &mut buf);
                        if re.is_match(&buf) {
                            break;
                        }
                        i = (i + 1) % n;
                        if i == hunk_index {
                            self.err("No hunk matches the given pattern");
                            break;
                        }
                    }
                    hunk_index = i;
                }
                's' => {
                    let into = self.files[f]
                        .hunks
                        .get(hunk_index)
                        .map_or(0, |h| h.splittable_into);
                    if permitted & SPLIT == 0 {
                        self.err("Sorry, cannot split this hunk");
                    } else {
                        self.split_hunk(f, hunk_index);
                        let text = format!("Split into {into} hunks.");
                        let line = self.colored_line(&self.c.header, &text);
                        self.print(&line);
                        rendered = None;
                    }
                }
                'e' => {
                    if permitted & EDIT == 0 {
                        self.err("Sorry, cannot edit this hunk");
                    } else if self.edit_hunk_loop(f, hunk_index)? >= 0 {
                        self.files[f].hunks[hunk_index].use_ = Pick::Use;
                        soft_increment(&mut hunk_index);
                    }
                }
                _ if ch == 'p' => {
                    rendered = None;
                    use_pager = first == 'P';
                }
                '?' => {
                    let mut text = if self.c.help.is_empty() {
                        self.mode.help.to_owned()
                    } else {
                        format!("{}{}{}", self.c.help, self.mode.help, self.c.reset)
                    };
                    for line in HELP_REMAINDER.lines() {
                        let key = line.chars().next().expect("help line");
                        if key != '?' && !keys.contains(key) {
                            continue;
                        }
                        text.push_str(&self.colored_line(&self.c.help, line));
                    }
                    self.print(&text);
                }
                _ => self.err(&format!(
                    "Unknown command '{}' (use '?' for help)",
                    self.answer
                )),
            }
        }
        let file = &self.files[f];
        let any = file.hunks.iter().any(|h| h.use_ == Pick::Use)
            || (file.hunks.is_empty() && file.head.use_ == Pick::Use);
        if any {
            let patch = self.reassemble_patch(f, false)?;
            match self.mode.apply {
                Apply::Collect => self.collected.extend_from_slice(&patch),
                Apply::Checkout => self.apply_for_checkout(&patch),
                Apply::Cached => {
                    if !self.apply(&patch, true, false) {
                        eprintln!("error: 'git apply' failed");
                    }
                }
                Apply::Worktree => {
                    if !self.apply(&patch, false, false) {
                        eprintln!("error: 'git apply' failed");
                    }
                }
            }
        }
        self.print("\n");
        Ok(quit)
    }

    fn set_use(&mut self, f: usize, i: usize, to: Pick) {
        match self.files[f].hunks.get_mut(i) {
            Some(h) => h.use_ = to,
            None => self.files[f].head.use_ = to,
        }
    }
}

static STDIN_EOF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A line from stdin without its line end; once stdin ends it stays ended,
/// as C's stdio has it for git.
pub(crate) fn read_stdin_line() -> Option<String> {
    use std::sync::atomic::Ordering::Relaxed;
    if STDIN_EOF.load(Relaxed) {
        return None;
    }
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => {
            STDIN_EOF.store(true, Relaxed);
            None
        }
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            Some(line)
        }
    }
}

fn hunk_mut(f: &mut FileDiff, cur: Option<usize>) -> &mut Hunk {
    match cur {
        Some(i) => &mut f.hunks[i],
        None => &mut f.head,
    }
}

fn complete_file(marker: u8, hunk: &mut Hunk) {
    if marker == b'-' || marker == b'+' {
        hunk.splittable_into += 1;
    }
}

/// One key from the terminal without waiting for Enter
/// (interactive.singleKey).
fn read_key() -> Option<char> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, read};
    crossterm::terminal::enable_raw_mode().ok()?;
    let key = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                break match k.code {
                    KeyCode::Char(c) => Some(c),
                    KeyCode::Enter => Some('\n'),
                    _ => continue,
                };
            }
            Ok(_) => continue,
            Err(_) => break None,
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    key
}

/// The colored twin of a plain diff, line for line, as `git diff --color`
/// paints it.
pub(crate) fn colorize(plain: &[u8], c: &Colors) -> Vec<u8> {
    let mut out = Vec::with_capacity(plain.len() * 2);
    let reset = c.reset.as_bytes();
    let mut in_header = true;
    for raw in plain.split_inclusive(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\n").unwrap_or(raw);
        let nl = raw.len() > line.len();
        let paint = |out: &mut Vec<u8>, color: &str, text: &[u8]| {
            out.extend_from_slice(color.as_bytes());
            out.extend_from_slice(text);
            out.extend_from_slice(reset);
        };
        if line.starts_with(b"diff ") || line.starts_with(b"* Unmerged path ") {
            in_header = true;
        }
        if line.starts_with(b"@@ ") {
            in_header = false;
            let end = line[2..]
                .windows(2)
                .position(|w| w == b"@@")
                .map_or(line.len(), |i| i + 4);
            paint(&mut out, &c.fraginfo, &line[..end]);
            if end < line.len() {
                paint(&mut out, "", &line[end..]);
            }
        } else if in_header {
            paint(&mut out, "\x1b[1m", line);
        } else {
            match line.first() {
                Some(b'+') => {
                    let body = &line[1..];
                    let kept = body
                        .iter()
                        .rposition(|b| !b" \t".contains(b))
                        .map_or(0, |i| i + 1);
                    paint(&mut out, &c.new, b"+");
                    if kept > 0 {
                        paint(&mut out, &c.new, &body[..kept]);
                    }
                    if kept < body.len() {
                        paint(&mut out, "\x1b[41m", &body[kept..]);
                    }
                }
                Some(b'-') => paint(&mut out, &c.old, line),
                _ => paint(&mut out, &c.context, line),
            }
        }
        if nl {
            out.push(b'\n');
        }
    }
    out
}

fn config_color(backend: &Arc<dyn GitBackend>, on: bool, key: &str, default: &str) -> String {
    if !on {
        return String::new();
    }
    backend
        .config_get(&format!("color.{key}"))
        .ok()
        .flatten()
        .and_then(|v| rgit_git::ansi_color(&v))
        .unwrap_or_else(|| default.to_owned())
}

pub(crate) fn colors(backend: &Arc<dyn GitBackend>) -> Colors {
    use std::io::IsTerminal;
    let get = |k: &str| backend.config_get(k).ok().flatten();
    let tty =
        std::io::stdout().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb");
    let on = match get("color.interactive")
        .or_else(|| get("color.ui"))
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("never" | "false" | "no" | "off" | "0") => false,
        Some("always") => true,
        _ => tty,
    };
    Colors {
        on,
        header: config_color(backend, on, "interactive.header", "\x1b[1m"),
        help: config_color(backend, on, "interactive.help", "\x1b[1;31m"),
        prompt: config_color(backend, on, "interactive.prompt", "\x1b[1;34m"),
        error: config_color(backend, on, "interactive.error", "\x1b[1;31m"),
        fraginfo: config_color(backend, on, "diff.frag", "\x1b[36m"),
        context: config_color(backend, on, "diff.context", ""),
        old: config_color(backend, on, "diff.old", "\x1b[31m"),
        new: config_color(backend, on, "diff.new", "\x1b[32m"),
        reset: if on {
            "\x1b[m".to_owned()
        } else {
            String::new()
        },
    }
}

/// Run git's `-p` loop over the changes under `paths`. For `stash`, the
/// chosen hunks come back as a patch against HEAD instead of being applied.
pub fn run(
    backend: &Arc<dyn GitBackend>,
    kind: Kind,
    rev: Option<&str>,
    paths: &[String],
) -> anyhow::Result<Vec<u8>> {
    let rev = match rev {
        Some(r) if r != "HEAD" => Some(backend.rev_parse(r).unwrap_or_else(|_| r.to_owned())),
        other => other.map(str::to_owned),
    };
    let single_key = backend
        .config_get("interactive.singleKey")
        .ok()
        .flatten()
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1"));
    let mut s = State {
        backend,
        mode: Mode::new(kind, rev.as_deref()),
        plain: Vec::new(),
        colored: Vec::new(),
        files: Vec::new(),
        c: colors(backend),
        single_key,
        answer: String::new(),
        collected: Vec::new(),
    };
    s.parse_diff(paths)?;
    let mut binary = 0;
    for f in 0..s.files.len() {
        if s.files[f].binary && s.files[f].hunks.is_empty() {
            binary += 1;
        } else if s.patch_update_file(f)? {
            break;
        }
    }
    if s.files.is_empty() {
        s.err("No changes.");
    } else if binary == s.files.len() {
        s.err("Only binary files changed.");
    }
    Ok(s.collected)
}
