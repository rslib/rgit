//! git's own xdiff (the copy libgit2 vendors), called directly for what
//! libgit2's diff options do not reach: the histogram algorithm and
//! `--anchored`. Also the diff settings a command asks for, kept per thread
//! so the backend and the renderer see the same ones.

use std::cell::RefCell;
use std::ffi::{CString, c_char, c_int, c_long, c_ulong, c_void};

use crate::{DiffLine, Hunk, LineOrigin};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Algorithm {
    #[default]
    Myers,
    Minimal,
    Patience,
    Histogram,
}

impl Algorithm {
    /// git's parse_algorithm_value.
    pub fn parse(name: &str) -> Option<Algorithm> {
        Some(match name.to_ascii_lowercase().as_str() {
            "myers" | "default" => Algorithm::Myers,
            "minimal" => Algorithm::Minimal,
            "patience" => Algorithm::Patience,
            "histogram" => Algorithm::Histogram,
            _ => return None,
        })
    }
}

/// How the diff of the running command is computed and labelled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffTweaks {
    pub algorithm: Algorithm,
    /// `--anchored`: lines that start with these stay unchanged if they can.
    pub anchors: Vec<String>,
    pub inter_hunk: Option<u32>,
    /// `a/` and `b/` unless set.
    pub src_prefix: Option<String>,
    pub dst_prefix: Option<String>,
    pub full_index: bool,
    /// `--binary`: a binary patch for binary files.
    pub binary: bool,
    /// `-B[n][/m]`: break and merge scores out of 60000.
    pub break_rewrites: Option<(u32, u32)>,
    pub irreversible_delete: bool,
    pub rename_limit: Option<usize>,
    /// `--relative`: only paths under this folder (with a trailing `/`),
    /// named from it.
    pub relative: Option<String>,
    /// Run `diff.<driver>.textconv` filters.
    pub textconv: bool,
}

thread_local! {
    static TWEAKS: RefCell<DiffTweaks> = RefCell::new(DiffTweaks::default());
}

/// Set the diff settings for what this thread runs next.
pub fn set_tweaks(t: DiffTweaks) {
    TWEAKS.with(|c| *c.borrow_mut() = t);
}

pub fn tweaks() -> DiffTweaks {
    TWEAKS.with(|c| c.borrow().clone())
}

#[repr(C)]
struct MmFile {
    ptr: *const c_char,
    size: c_long,
}

#[repr(C)]
struct MmBuffer {
    ptr: *const c_char,
    size: c_long,
}

#[repr(C)]
struct XpParam {
    flags: c_ulong,
    ignore_regex: *mut c_void,
    ignore_regex_nr: usize,
    anchors: *mut *mut c_char,
    anchors_nr: usize,
}

type OutLine = extern "C" fn(*mut c_void, *mut MmBuffer, c_int) -> c_int;

#[repr(C)]
struct EmitCb {
    private: *mut c_void,
    out_hunk: *const c_void,
    out_line: OutLine,
}

#[repr(C)]
struct EmitConf {
    ctxlen: c_long,
    interhunkctxlen: c_long,
    flags: c_ulong,
    find_func: *const c_void,
    find_func_priv: *mut c_void,
    hunk_func: *const c_void,
}

unsafe extern "C" {
    fn xdl_diff(
        mf1: *mut MmFile,
        mf2: *mut MmFile,
        xpp: *const XpParam,
        xecfg: *const EmitConf,
        ecb: *mut EmitCb,
    ) -> c_int;
}

const NEED_MINIMAL: c_ulong = 1;
const IGNORE_WHITESPACE: c_ulong = 1 << 1;
const IGNORE_WHITESPACE_CHANGE: c_ulong = 1 << 2;
const PATIENCE: c_ulong = 1 << 14;
const HISTOGRAM: c_ulong = 1 << 15;
const INDENT_HEURISTIC: c_ulong = 1 << 23;
const EMIT_FUNCNAMES: c_ulong = 1;

/// What [`hunks`] diffs with.
#[derive(Debug, Clone, Default)]
pub struct XdiffOpts {
    pub algorithm: Algorithm,
    pub anchors: Vec<String>,
    pub context: u32,
    pub inter_hunk: u32,
    pub ignore_all_space: bool,
    pub ignore_space_change: bool,
}

extern "C" fn out_line(private: *mut c_void, mb: *mut MmBuffer, n: c_int) -> c_int {
    // SAFETY: xdiff passes back our `Vec<Vec<u8>>` and `n` valid buffers.
    let (out, bufs) = unsafe {
        (
            &mut *(private as *mut Vec<Vec<u8>>),
            std::slice::from_raw_parts(mb, n as usize),
        )
    };
    let mut line = Vec::new();
    for b in bufs {
        if b.size > 0 {
            // SAFETY: each buffer is `size` readable bytes.
            line.extend_from_slice(unsafe {
                std::slice::from_raw_parts(b.ptr as *const u8, b.size as usize)
            });
        }
    }
    out.push(line);
    0
}

/// The hunks git's xdiff finds between `old` and `new`, as libgit2's patches
/// give them.
pub fn hunks(old: &[u8], new: &[u8], o: &XdiffOpts) -> Vec<Hunk> {
    // xdiff allocates through libgit2, which must be set up first.
    libgit2_sys::init();
    let anchors: Vec<CString> = o
        .anchors
        .iter()
        .filter_map(|a| CString::new(a.as_str()).ok())
        .collect();
    let mut anchor_ptrs: Vec<*mut c_char> = anchors.iter().map(|a| a.as_ptr() as _).collect();
    let mut flags = INDENT_HEURISTIC;
    flags |= match o.algorithm {
        Algorithm::Myers => 0,
        Algorithm::Minimal => NEED_MINIMAL,
        Algorithm::Patience => PATIENCE,
        Algorithm::Histogram => HISTOGRAM,
    };
    if o.ignore_all_space {
        flags |= IGNORE_WHITESPACE;
    }
    if o.ignore_space_change {
        flags |= IGNORE_WHITESPACE_CHANGE;
    }
    let xpp = XpParam {
        flags,
        ignore_regex: std::ptr::null_mut(),
        ignore_regex_nr: 0,
        anchors: anchor_ptrs.as_mut_ptr(),
        anchors_nr: anchor_ptrs.len(),
    };
    let conf = EmitConf {
        ctxlen: o.context as c_long,
        interhunkctxlen: o.inter_hunk as c_long,
        flags: EMIT_FUNCNAMES,
        find_func: std::ptr::null(),
        find_func_priv: std::ptr::null_mut(),
        hunk_func: std::ptr::null(),
    };
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut cb = EmitCb {
        private: &mut lines as *mut _ as *mut c_void,
        out_hunk: std::ptr::null(),
        out_line,
    };
    let mut a = MmFile {
        ptr: old.as_ptr() as _,
        size: old.len() as c_long,
    };
    let mut b = MmFile {
        ptr: new.as_ptr() as _,
        size: new.len() as c_long,
    };
    // SAFETY: every pointer outlives the call; xdiff only reads the files.
    unsafe { xdl_diff(&mut a, &mut b, &xpp, &conf, &mut cb) };
    let mut out: Vec<Hunk> = Vec::new();
    for raw in lines {
        let text = String::from_utf8_lossy(&raw);
        if let Some(h) = text.strip_prefix("@@ -") {
            let new_start = h
                .split_once(" +")
                .and_then(|(_, n)| n.split([',', ' ']).next()?.parse().ok())
                .unwrap_or(0);
            out.push(Hunk {
                header: text.trim_end_matches('\n').to_owned(),
                new_start,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(h) = out.last_mut() else { continue };
        let (origin, rest) = match text.chars().next() {
            Some('+') => (LineOrigin::Added, &text[1..]),
            Some('-') => (LineOrigin::Removed, &text[1..]),
            _ => (LineOrigin::Context, text.get(1..).unwrap_or("")),
        };
        let (body, eof) = match rest.split_once("\n\\ No newline at end of file\n") {
            Some((body, _)) => (body, true),
            None => (rest.strip_suffix('\n').unwrap_or(rest), false),
        };
        h.lines.push(DiffLine {
            origin,
            text: body.to_owned(),
        });
        if eof {
            h.lines.push(DiffLine {
                origin: LineOrigin::Meta,
                text: "\\ No newline at end of file".to_owned(),
            });
        }
    }
    out
}

/// git's parse_rename_score: `50` and `50%` are half of 60000, `5` too.
pub fn parse_score(s: &str) -> Option<u32> {
    let pct = s.ends_with('%');
    let digits = s.trim_end_matches('%');
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.is_empty() {
        return Some(0);
    }
    let num: u64 = digits.parse().ok()?;
    let scale: u64 = if pct {
        100
    } else {
        10u64.pow(digits.len() as u32)
    };
    Some(if num >= scale {
        60000
    } else {
        (num * 60000 / scale) as u32
    })
}

/// `-B[n][/m]`'s break and merge scores, git's defaults for the ones left out.
pub fn parse_break(s: &str) -> Option<(u32, u32)> {
    let (b, m) = s.split_once('/').unwrap_or((s, ""));
    let (b, m) = (parse_score(b)?, parse_score(m)?);
    Some((
        if b == 0 { 30000 } else { b },
        if m == 0 { 36000 } else { m },
    ))
}

/// A file's bytes on one side of a delta: its blob, else the work tree file.
fn side(repo: &git2::Repository, f: &git2::DiffFile) -> Vec<u8> {
    if !f.id().is_zero()
        && let Ok(blob) = repo.find_blob(f.id())
    {
        return blob.content().to_vec();
    }
    f.path()
        .zip(repo.workdir())
        .and_then(|(p, w)| std::fs::read(w.join(p)).ok())
        .unwrap_or_default()
}

/// `diff.<driver>.textconv` for `path`, from its `diff` attribute.
fn textconv_cmd(repo: &git2::Repository, path: &std::path::Path) -> Option<String> {
    let driver = repo
        .get_attr(path, "diff", git2::AttrCheckFlags::FILE_THEN_INDEX)
        .ok()
        .flatten()?
        .to_owned();
    repo.config()
        .ok()?
        .get_string(&format!("diff.{driver}.textconv"))
        .ok()
}

/// Run a textconv filter over `data` as git does: on a temporary file,
/// through the shell.
pub(crate) fn textconv(cmd: &str, data: &[u8]) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("rgit-textconv-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join(format!("{:x}", std::ptr::from_ref(data).addr()));
    if std::fs::write(&tmp, data).is_err() {
        return data.to_vec();
    }
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{cmd} \"$@\""))
        .arg(cmd)
        .arg(&tmp)
        .output();
    let _ = std::fs::remove_file(&tmp);
    out.map(|o| o.stdout).unwrap_or_default()
}

fn lines_of(data: &[u8]) -> Vec<&[u8]> {
    data.split_inclusive(|&b| b == b'\n').collect()
}

/// git's emit_rewrite_diff hunk: every old line out, every new line in.
fn rewrite_hunk(old: &[u8], new: &[u8]) -> Hunk {
    let (a, b) = (lines_of(old), lines_of(new));
    let count = |n: usize| match n {
        0 => "0,0".to_owned(),
        1 => "1".to_owned(),
        n => format!("1,{n}"),
    };
    let mut lines = Vec::new();
    for (side, origin) in [(&a, LineOrigin::Removed), (&b, LineOrigin::Added)] {
        for l in side.iter() {
            let text = String::from_utf8_lossy(l);
            lines.push(DiffLine {
                origin,
                text: text.strip_suffix('\n').unwrap_or(&text).to_owned(),
            });
            if !l.ends_with(b"\n") {
                lines.push(DiffLine {
                    origin: LineOrigin::Meta,
                    text: "\\ No newline at end of file".to_owned(),
                });
            }
        }
    }
    Hunk {
        header: format!("@@ -{} +{} @@", count(a.len()), count(b.len())),
        new_start: u32::from(!b.is_empty()),
        lines,
    }
}

/// Bytes of `new` that `old` has too and bytes it adds, by git's span
/// hashing (diffcore_count_changes).
pub fn count_changes(old: &[u8], new: &[u8]) -> (u64, u64) {
    let (sa, sb) = (
        crate::git_repo::span_hashes(old),
        crate::git_repo::span_hashes(new),
    );
    let mut copied: u64 = 0;
    let mut added: u64 = 0;
    for (h, &n) in &sb {
        let s = sa.get(h).copied().unwrap_or(0);
        copied += s.min(n);
        added += n.saturating_sub(s);
    }
    (copied, added)
}

/// git's diffcore-break should_break: whether `old` to `new` is a rewrite,
/// and its merge score.
fn should_break(old: &[u8], new: &[u8], break_score: u32) -> Option<u64> {
    const MAX: u64 = 60000;
    let (src, dst) = (old.len() as u64, new.len() as u64);
    let max = src.max(dst);
    if max < 400 || src == 0 {
        return None;
    }
    let (mut copied, mut added) = count_changes(old, new);
    copied = copied.min(src);
    if dst < added + copied {
        added = dst.saturating_sub(copied);
    }
    let removed = src - copied;
    let merge = removed * MAX / src;
    let break_score = u64::from(break_score);
    if merge > break_score {
        return Some(merge);
    }
    let delta = removed + added;
    if delta * MAX / max < break_score {
        return None;
    }
    if src * break_score < removed * MAX && added * 20 < removed && added * 20 < copied {
        return None;
    }
    Some(merge)
}

/// Replace the `Binary files ... differ` line (or binary patch) of a header
/// with the `---`/`+++` lines of a text diff.
fn text_header(header: &str, old: &str, new: &str) -> String {
    let mut out = String::new();
    for line in header.split_inclusive('\n') {
        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            out.push_str(&format!("--- {old}\n+++ {new}\n"));
            break;
        }
        out.push_str(line);
    }
    out
}

/// Apply the thread's [`DiffTweaks`] that libgit2 cannot to the files of
/// `diff`, one per delta.
pub(crate) fn finish(
    repo: &git2::Repository,
    diff: &git2::Diff,
    files: &mut [crate::FileDiff],
    spec: &crate::DiffSpec,
    t: &DiffTweaks,
) {
    let rehunk = t.algorithm == Algorithm::Histogram || !t.anchors.is_empty();
    let xo = XdiffOpts {
        algorithm: t.algorithm,
        anchors: t.anchors.clone(),
        context: spec.context.unwrap_or(3),
        inter_hunk: t.inter_hunk.unwrap_or(0),
        ignore_all_space: spec.ignore_all_space,
        ignore_space_change: spec.ignore_space_change,
    };
    let (src, dst) = (
        t.src_prefix.as_deref().unwrap_or("a/"),
        t.dst_prefix.as_deref().unwrap_or("b/"),
    );
    for (idx, file) in files.iter_mut().enumerate() {
        let Some(delta) = diff.get_delta(idx) else {
            continue;
        };
        let (of, nf) = (delta.old_file(), delta.new_file());
        let path = nf.path().or(of.path()).map(|p| p.to_path_buf());
        let conv = t
            .textconv
            .then(|| path.as_deref().and_then(|p| textconv_cmd(repo, p)))
            .flatten();
        let regular =
            |m: git2::FileMode| matches!(m, git2::FileMode::Blob | git2::FileMode::BlobExecutable);
        let broken = t.break_rewrites.filter(|_| {
            file.status == crate::StatusCode::Modified
                && regular(of.mode())
                && regular(nf.mode())
                && of.id() != nf.id()
        });
        if !rehunk && conv.is_none() && broken.is_none() {
            continue;
        }
        let (mut old, mut new) = match delta.status() {
            git2::Delta::Added | git2::Delta::Untracked => (Vec::new(), side(repo, &nf)),
            git2::Delta::Deleted => (side(repo, &of), Vec::new()),
            _ => (side(repo, &of), side(repo, &nf)),
        };
        let score =
            broken.and_then(|(b, m)| should_break(&old, &new, b).filter(|&s| s >= u64::from(m)));
        if let Some(cmd) = &conv {
            if delta.status() != git2::Delta::Added {
                old = textconv(cmd, &old);
            }
            if delta.status() != git2::Delta::Deleted {
                new = textconv(cmd, &new);
            }
            if file.binary {
                let name = |p: &str, f: &git2::DiffFile, none: git2::Delta| {
                    if delta.status() == none {
                        "/dev/null".to_owned()
                    } else {
                        let path = f.path().map(|p| p.to_string_lossy()).unwrap_or_default();
                        format!("{p}{path}")
                    }
                };
                file.header = text_header(
                    &file.header,
                    &name(src, &of, git2::Delta::Added),
                    &name(dst, &nf, git2::Delta::Deleted),
                );
                file.binary = false;
            }
        } else if file.binary {
            continue;
        }
        if let Some(score) = score {
            let pct = score * 100 / 60000;
            file.similarity = pct as u16;
            if let Some(i) = file.header.find('\n') {
                file.header
                    .insert_str(i + 1, &format!("dissimilarity index {pct}%\n"));
            }
            file.hunks = vec![rewrite_hunk(&old, &new)];
        } else {
            file.hunks = hunks(&old, &new, &xo);
        }
    }
}

/// `--relative`, `--irreversible-delete` and `--binary`'s deflate level
/// over finished files.
pub(crate) fn relabel(mut files: Vec<crate::FileDiff>, t: &DiffTweaks) -> Vec<crate::FileDiff> {
    if let Some(rel) = &t.relative {
        let (src, dst) = (
            t.src_prefix.as_deref().unwrap_or("a/"),
            t.dst_prefix.as_deref().unwrap_or("b/"),
        );
        files.retain(|f| f.path.starts_with(rel.as_str()));
        for f in &mut files {
            f.path = f.path[rel.len()..].to_owned();
            if let Some(old) = &mut f.old_path
                && let Some(o) = old.strip_prefix(rel.as_str())
            {
                *old = o.to_owned();
            }
            let mut header = String::new();
            for line in f.header.split_inclusive('\n') {
                let mut line = line.to_owned();
                for p in [src, dst] {
                    line = line.replace(&format!("{p}{rel}"), p);
                }
                for key in ["rename from ", "rename to ", "copy from ", "copy to "] {
                    if let Some(rest) = line.strip_prefix(key)
                        && let Some(rest) = rest.strip_prefix(rel.as_str())
                    {
                        line = format!("{key}{rest}");
                    }
                }
                header.push_str(&line);
            }
            f.header = header;
        }
    }
    for f in &mut files {
        if t.irreversible_delete && f.status == crate::StatusCode::Deleted {
            if let Some(i) = f.header.find("\nindex ") {
                let end = f.header[i + 1..]
                    .find('\n')
                    .map_or(f.header.len(), |e| i + e + 2);
                f.header.truncate(end);
            }
            f.hunks.clear();
        }
        if t.binary && f.binary {
            f.header = crate::format_patch::rezip_binary(&f.header);
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_matches_git() {
        let o = XdiffOpts {
            algorithm: Algorithm::Histogram,
            context: 3,
            ..Default::default()
        };
        let h = hunks(b"a\nb\nc\n", b"a\nx\nc", &o);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].header, "@@ -1,3 +1,3 @@");
        let t: Vec<_> = h[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(t, ["a", "b", "c", "x", "c", "\\ No newline at end of file"]);
    }
}
