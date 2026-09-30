//! git's userdiff drivers (userdiff.c): the funcname and word regexes a
//! path's `diff` attribute picks, and the xdiff pieces that use them - hunk
//! headers, `-W` function context and `--word-diff`.

use std::path::Path;

use git2::Repository;

use crate::GitError;

/// A POSIX regex, compiled and run by the C library as git does, so its
/// leftmost-longest matches and bracket syntax are git's.
pub struct Regex(Box<posix::RegexT>);

// SAFETY: a compiled regex_t is only read by regexec, which POSIX makes
// thread-safe.
unsafe impl Send for Regex {}
unsafe impl Sync for Regex {}

pub const EXTENDED: i32 = libc::REG_EXTENDED;
pub const ICASE: i32 = libc::REG_ICASE;
pub const NEWLINE: i32 = libc::REG_NEWLINE;

impl Regex {
    /// `regcomp(pattern, cflags)`; basic regexes get REG_ENHANCED on macOS, as
    /// git's build there asks.
    pub fn new(pattern: &[u8], mut cflags: i32) -> Result<Regex, String> {
        if cfg!(target_vendor = "apple") && cflags & EXTENDED == 0 {
            cflags |= 0o400;
        }
        // git takes the character classes of the user's locale.
        static LOCALE: std::sync::Once = std::sync::Once::new();
        // SAFETY: once, before any regex of ours is compiled.
        LOCALE.call_once(|| unsafe {
            libc::setlocale(libc::LC_CTYPE, c"".as_ptr());
        });
        let c = std::ffi::CString::new(pattern).map_err(|e| e.to_string())?;
        let mut re = Box::new(posix::RegexT([0; 32]));
        // SAFETY: re is large enough for any libc's regex_t and c is
        // NUL-terminated.
        let rc = unsafe { posix::regcomp(&mut *re, c.as_ptr(), cflags) };
        if rc != 0 {
            let mut buf = [0u8; 1024];
            // SAFETY: regerror writes at most buf.len() bytes.
            unsafe { posix::regerror(rc, &*re, buf.as_mut_ptr().cast(), buf.len()) };
            let end = buf.iter().position(|&b| b == 0).unwrap_or(0);
            return Err(String::from_utf8_lossy(&buf[..end]).into_owned());
        }
        Ok(Regex(re))
    }

    /// git's `regexec_buf`: the whole match and its first group, as byte
    /// ranges of `text`.
    pub fn exec(&self, text: &[u8]) -> Option<[Option<(usize, usize)>; 2]> {
        let mut m = [
            posix::RegMatch {
                so: 0,
                eo: text.len() as _,
            },
            posix::RegMatch { so: -1, eo: -1 },
        ];
        #[cfg(not(target_env = "musl"))]
        let (ptr, eflags) = {
            // A non-null pointer even for an empty slice; REG_STARTEND bounds it.
            let ptr = if text.is_empty() {
                c"".as_ptr().cast()
            } else {
                text.as_ptr()
            };
            (ptr.cast::<libc::c_char>(), libc::REG_STARTEND)
        };
        // musl has no REG_STARTEND: match a NUL-terminated copy so offsets
        // stay relative to text.
        // ponytail: an interior NUL truncates the match (glibc matches past
        // it); userdiff text is source code, so reintroduce STARTEND-free
        // bounded matching only if that ever matters.
        #[cfg(target_env = "musl")]
        let c = std::ffi::CString::new(text).ok()?;
        #[cfg(target_env = "musl")]
        let (ptr, eflags) = (c.as_ptr(), 0);
        // SAFETY: REG_STARTEND keeps regexec inside text[..len]; on musl it
        // reads the NUL-terminated copy, alive for the call.
        let rc = unsafe { posix::regexec(&*self.0, ptr, 2, m.as_mut_ptr(), eflags) };
        if rc != 0 {
            return None;
        }
        let span = |r: &posix::RegMatch| (r.so >= 0).then_some((r.so as usize, r.eo as usize));
        Some([span(&m[0]), span(&m[1])])
    }

    pub fn is_match(&self, text: &[u8]) -> bool {
        self.exec(text).is_some()
    }

    /// The C library's words for a failed match, as git prints them.
    pub fn no_match(&self) -> String {
        let mut buf = [0u8; 1024];
        // SAFETY: regerror writes at most buf.len() bytes.
        unsafe {
            posix::regerror(
                libc::REG_NOMATCH,
                &*self.0,
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        let end = buf.iter().position(|&b| b == 0).unwrap_or(0);
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        // SAFETY: compiled by regcomp in new.
        unsafe { posix::regfree(&mut *self.0) }
    }
}

mod posix {
    #[repr(C, align(8))]
    pub struct RegexT(pub [u64; 32]);

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    pub type Off = i32;
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    pub type Off = i64;

    #[repr(C)]
    pub struct RegMatch {
        pub so: Off,
        pub eo: Off,
    }

    unsafe extern "C" {
        pub fn regcomp(
            re: *mut RegexT,
            pattern: *const libc::c_char,
            cflags: libc::c_int,
        ) -> libc::c_int;
        pub fn regexec(
            re: *const RegexT,
            text: *const libc::c_char,
            nmatch: libc::size_t,
            m: *mut RegMatch,
            eflags: libc::c_int,
        ) -> libc::c_int;
        pub fn regerror(
            code: libc::c_int,
            re: *const RegexT,
            buf: *mut libc::c_char,
            size: libc::size_t,
        ) -> libc::size_t;
        pub fn regfree(re: *mut RegexT);
    }
}

/// A funcname pattern list: the first line that matches decides, and a `!`
/// line says "not a function".
pub struct Funcname(Vec<(bool, Regex)>);

impl Funcname {
    /// git's `xdiff_set_find_func`.
    pub fn new(pattern: &str, cflags: i32) -> Result<Funcname, GitError> {
        let lines: Vec<&str> = pattern.split('\n').collect();
        let mut rules = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let (negate, expr) = match line.strip_prefix('!') {
                Some(l) => (true, l),
                None => (false, *line),
            };
            if negate && i == lines.len() - 1 {
                return Err(GitError::Other(format!(
                    "Last expression must not be negated: {line}"
                )));
            }
            let re = Regex::new(expr.as_bytes(), cflags).map_err(|_| {
                GitError::Other(format!("Invalid regexp to look for hunk header: {expr}"))
            })?;
            rules.push((negate, re));
        }
        Ok(Funcname(rules))
    }

    /// git's `ff_regexp`: the function text a line shows, or None.
    fn find<'a>(&self, line: &'a [u8]) -> Option<&'a [u8]> {
        let line = line
            .strip_suffix(b"\r\n")
            .or_else(|| line.strip_suffix(b"\n"))
            .unwrap_or(line);
        let (negate, m) = self
            .0
            .iter()
            .find_map(|(neg, re)| re.exec(line).map(|m| (*neg, m)))?;
        if negate {
            return None;
        }
        let (a, b) = m[1].or(m[0])?;
        Some(&line[a..b])
    }
}

/// xdiff's `match_func_rec`: the header text (at most `size` bytes, trailing
/// space dropped) of a function line, with git's default rule when there is
/// no pattern.
pub fn func_text(f: Option<&Funcname>, line: &[u8], size: usize) -> Option<Vec<u8>> {
    let text = match f {
        Some(f) => f.find(line)?,
        None if line
            .first()
            .is_some_and(|&c| c.is_ascii_alphabetic() || c == b'_' || c == b'$') =>
        {
            line
        }
        None => return None,
    };
    let mut text = &text[..text.len().min(size)];
    while let [rest @ .., c] = text
        && is_space(*c)
    {
        text = rest;
    }
    Some(text.to_vec())
}

pub fn is_func(f: Option<&Funcname>, line: &[u8]) -> bool {
    func_text(f, line, 1).is_some()
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The regexes of one `diff` driver.
#[derive(Default)]
pub struct Driver {
    pub funcname: Option<Funcname>,
    pub word_regex: Option<Vec<u8>>,
}

/// How to find a path's driver when its `diff` attribute names none: git's
/// diff and grep fall back to the `default` driver, `-L` to none.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    Default,
    None,
}

/// git's `userdiff_find_by_path` (and the `default` driver where diff and
/// grep use it), with `diff.<driver>.*` config over the builtins.
pub fn driver(
    repo: &Repository,
    path: &str,
    fallback: Fallback,
) -> Result<Option<Driver>, GitError> {
    let value = repo
        .get_attr(Path::new(path), "diff", git2::AttrCheckFlags::default())
        .ok()
        .flatten();
    let name = match git2::AttrValue::from_string(value) {
        git2::AttrValue::True | git2::AttrValue::False => return Ok(Some(Driver::default())),
        git2::AttrValue::String(s) => Some(s.to_owned()),
        _ => None,
    };
    if let Some(d) = name
        .as_deref()
        .map(|n| by_name(repo, n))
        .transpose()?
        .flatten()
    {
        return Ok(Some(d));
    }
    match fallback {
        Fallback::Default => by_name(repo, "default"),
        Fallback::None => Ok(None),
    }
}

/// git's `userdiff_find_by_name`: a builtin, a configured driver, or none.
fn by_name(repo: &Repository, name: &str) -> Result<Option<Driver>, GitError> {
    let builtin = BUILTINS.iter().find(|b| b.0 == name);
    let (mut funcname, mut word): (Option<(String, i32)>, Option<Vec<u8>>) = match builtin {
        Some(&(_, f, w, icase)) => (
            Some((f.to_owned(), EXTENDED | if icase { ICASE } else { 0 })),
            Some(builtin_word_regex(w)),
        ),
        None => (None, None),
    };
    let mut configured = false;
    let config = repo.config()?;
    let prefix = format!("diff.{name}.");
    let mut entries = config.entries(None)?;
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let Some(key) = entry.name().ok().and_then(|k| k.strip_prefix(&prefix)) else {
            continue;
        };
        configured = true;
        let value = entry.value().unwrap_or_default().to_owned();
        match key {
            "funcname" => funcname = Some((value, 0)),
            "xfuncname" => funcname = Some((value, EXTENDED)),
            "wordregex" => word = Some(value.into_bytes()),
            _ => {}
        }
    }
    // git swaps a builtin's multi-byte word regex in on lookup, over any
    // configured one.
    if let Some(b) = builtin
        && multi_byte()
    {
        word = Some(builtin_word_regex(b.2));
    }
    if builtin.is_none() && !configured && name != "default" {
        return Ok(None);
    }
    Ok(Some(Driver {
        funcname: funcname
            .filter(|(p, _)| !p.is_empty())
            .map(|(p, flags)| Funcname::new(&p, flags))
            .transpose()?,
        word_regex: word,
    }))
}

/// git's `regexec_supports_multi_byte_chars`: whether the locale's regexes
/// take a UTF-8 character as one.
fn multi_byte() -> bool {
    static MULTI_BYTE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MULTI_BYTE.get_or_init(|| {
        Regex::new(b"[^[:space:]]", EXTENDED)
            .ok()
            .and_then(|re| re.exec(b"\xc2\xa3"))
            .is_some_and(|m| m[0] == Some((0, 2)))
    })
}

/// A builtin's word regex with git's catch-all for other non-space runs.
fn builtin_word_regex(w: &str) -> Vec<u8> {
    let mut out = w.as_bytes().to_vec();
    out.extend_from_slice(if multi_byte() {
        b"|[^[:space:]]"
    } else {
        b"|[^[:space:]]|[\xc0-\xff][\x80-\xbf]+"
    });
    out
}

/// One change of an edit script: `chg1` lines at `i1` of the old side became
/// `chg2` lines at `i2` of the new (0-based).
#[derive(Clone, Copy, Debug)]
pub struct Change {
    pub i1: usize,
    pub chg1: usize,
    pub i2: usize,
    pub chg2: usize,
}

/// One emitted hunk: its header and `(origin, line)` pairs, lines with their
/// newline if they had one.
pub type EmittedHunk<'a> = (String, Vec<(char, &'a [u8])>);

/// xdiff's `xdl_emit_diff`: group `changes` into hunks with `ctx` lines of
/// context (`-W` stretching them over whole functions) and function names in
/// their headers.
pub fn emit<'a>(
    old: &[&'a [u8]],
    new: &[&'a [u8]],
    changes: &[Change],
    ctx: usize,
    interhunk: usize,
    funccontext: bool,
    ff: Option<&Funcname>,
) -> Vec<EmittedHunk<'a>> {
    let (n1, n2) = (old.len() as i64, new.len() as i64);
    let ctx = ctx as i64;
    let func_at = |l: &[&[u8]], i: i64| is_func(ff, l[i as usize]);
    // xdiff's get_func_line: the first function line from start towards limit.
    let func_line = |start: i64, limit: i64, buf: Option<&mut Vec<u8>>| -> i64 {
        let step = if start > limit { -1 } else { 1 };
        let mut l = start;
        while l != limit && 0 <= l && l < n1 {
            if let Some(t) = func_text(ff, old[l as usize], 80) {
                if let Some(b) = buf {
                    *b = t;
                }
                return l;
            }
            l += step;
        }
        -1
    };
    let is_empty = |i: i64| old[i as usize].iter().all(|&c| is_space(c));
    let max_common = 2 * ctx + interhunk as i64;
    let mut out = Vec::new();
    let mut funcline = Vec::new();
    let mut funclineprev = -1;
    let mut x = 0;
    while x < changes.len() {
        let xchp0 = x;
        let mut first = x;
        let mut last = x;
        while last + 1 < changes.len() {
            let (a, b) = (&changes[last], &changes[last + 1]);
            if b.i1 as i64 - (a.i1 + a.chg1) as i64 > max_common {
                break;
            }
            last += 1;
        }
        let (mut s1, mut s2);
        'pre: loop {
            let c = changes[first];
            s1 = (c.i1 as i64 - ctx).max(0);
            s2 = (c.i2 as i64 - ctx).max(0);
            if !funccontext {
                break;
            }
            let mut i1 = c.i1 as i64;
            if i1 >= n1 {
                let mut i2 = c.i2 as i64;
                let mut whole = false;
                while i2 < n2 {
                    if func_at(new, i2) {
                        whole = true;
                        break;
                    }
                    i2 += 1;
                }
                if whole {
                    break;
                }
                i1 = n1 - 1;
            }
            let mut fs1 = func_line(i1, -1, None);
            while fs1 > 0 && !is_empty(fs1 - 1) && !func_at(old, fs1 - 1) {
                fs1 -= 1;
            }
            fs1 = fs1.max(0);
            if fs1 < s1 {
                s2 = (s2 - (s1 - fs1)).max(0);
                s1 = fs1;
                let mut p = xchp0;
                while p != first
                    && (changes[p].i1 + changes[p].chg1) as i64 <= s1
                    && (changes[p].i2 + changes[p].chg2) as i64 <= s2
                {
                    p += 1;
                }
                if p != first {
                    first = p;
                    continue 'pre;
                }
            }
            break;
        }
        let (mut e1, mut e2);
        loop {
            let c = changes[last];
            let lctx = ctx
                .min(n1 - (c.i1 + c.chg1) as i64)
                .min(n2 - (c.i2 + c.chg2) as i64);
            e1 = (c.i1 + c.chg1) as i64 + lctx;
            e2 = (c.i2 + c.chg2) as i64 + lctx;
            if !funccontext {
                break;
            }
            let mut fe1 = func_line((c.i1 + c.chg1) as i64, n1, None);
            while fe1 > 0 && is_empty(fe1 - 1) {
                fe1 -= 1;
            }
            if fe1 < 0 {
                fe1 = n1;
            }
            if fe1 > e1 {
                e2 = (e2 + (fe1 - e1)).min(n2);
                e1 = fe1;
            }
            if let Some(next) = changes.get(last + 1) {
                let l = (next.i1 as i64).min(n1 - 1);
                if l - ctx <= e1 || func_line(l, e1, None) < 0 {
                    last += 1;
                    continue;
                }
            }
            break;
        }
        func_line(s1 - 1, funclineprev, Some(&mut funcline));
        funclineprev = s1 - 1;
        let (c1, c2) = (e1 - s1, e2 - s2);
        let num = |s: i64, c: i64| {
            let s = if c > 0 { s + 1 } else { s };
            if c == 1 {
                s.to_string()
            } else {
                format!("{s},{c}")
            }
        };
        let mut header = format!("@@ -{} +{} @@", num(s1, c1), num(s2, c2));
        if !funcline.is_empty() {
            header.push(' ');
            header.push_str(&String::from_utf8_lossy(&funcline));
        }
        let mut lines = Vec::new();
        let mut j2 = s2;
        while j2 < changes[first].i2 as i64 {
            lines.push((' ', new[j2 as usize]));
            j2 += 1;
        }
        let (mut k1, mut k2) = (changes[first].i1, changes[first].i2);
        for c in &changes[first..=last] {
            while k1 < c.i1 && k2 < c.i2 {
                lines.push((' ', new[k2]));
                k1 += 1;
                k2 += 1;
            }
            lines.extend(old[c.i1..c.i1 + c.chg1].iter().map(|l| ('-', *l)));
            lines.extend(new[c.i2..c.i2 + c.chg2].iter().map(|l| ('+', *l)));
            k1 = c.i1 + c.chg1;
            k2 = c.i2 + c.chg2;
        }
        for l in new.iter().take(e2 as usize).skip(k2) {
            lines.push((' ', *l));
        }
        out.push((header, lines));
        x = last + 1;
    }
    out
}

/// A hunk header as git's `sane_truncate_line` leaves it: cut before any
/// invalid UTF-8 (a function name cut mid-character at 80 bytes).
pub fn header_of(at: &str, func: &[u8]) -> String {
    let mut h = at.as_bytes().to_vec();
    if !func.is_empty() {
        h.push(b' ');
        h.extend_from_slice(func);
    }
    let valid = match std::str::from_utf8(&h) {
        Ok(_) => h.len(),
        Err(e) => e.valid_up_to(),
    };
    String::from_utf8_lossy(&h[..valid]).into_owned()
}

/// A word-diff style's text before and after a run of words.
type Marks = (&'static str, &'static str);

/// The `--word-diff` output styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordStyle {
    Plain,
    Color,
    Porcelain,
}

/// git's `diff_words_show`: the word diff of a run of removed (`minus`) and
/// added (`plus`) lines, each with its newline, split into words by `regex`
/// (runs of non-space without one).
pub fn word_diff(
    minus: &[u8],
    plus: &[u8],
    regex: Option<&Regex>,
    style: WordStyle,
    color: bool,
) -> Vec<u8> {
    let (ctx, old, new, newline): (Marks, Marks, Marks, &str) = match style {
        WordStyle::Porcelain => ((" ", "\n"), ("-", "\n"), ("+", "\n"), "~\n"),
        WordStyle::Plain => (("", ""), ("[-", "-]"), ("{+", "+}"), "\n"),
        WordStyle::Color => (("", ""), ("", ""), ("", ""), "\n"),
    };
    let (old_color, new_color) = if color {
        ("\x1b[31m", "\x1b[32m")
    } else {
        ("", "")
    };
    let mut out = Vec::new();
    let write = |out: &mut Vec<u8>, el: (&str, &str), paint: &str, mut buf: &[u8]| {
        while !buf.is_empty() {
            let p = buf.iter().position(|&c| c == b'\n');
            let text = &buf[..p.unwrap_or(buf.len())];
            if !text.is_empty() {
                out.extend_from_slice(paint.as_bytes());
                out.extend_from_slice(el.0.as_bytes());
                out.extend_from_slice(text);
                out.extend_from_slice(el.1.as_bytes());
                if !paint.is_empty() {
                    out.extend_from_slice(b"\x1b[m");
                }
            }
            let Some(p) = p else { break };
            out.extend_from_slice(newline.as_bytes());
            buf = &buf[p + 1..];
        }
    };
    if plus.is_empty() {
        write(&mut out, old, old_color, minus);
        return out;
    }
    let (mw, mtext) = words(minus, regex);
    let (pw, ptext) = words(plus, regex);
    let mut current = 0;
    for (mf, ml, pf, pl) in word_changes(&mtext, &ptext) {
        let (mb, me) = if ml > 0 {
            (mw[mf].0, mw[mf + ml - 1].1)
        } else {
            (mw[mf].1, mw[mf].1)
        };
        let (pb, pe) = if pl > 0 {
            (pw[pf].0, pw[pf + pl - 1].1)
        } else {
            (pw[pf].1, pw[pf].1)
        };
        if current != pb {
            write(&mut out, ctx, "", &plus[current..pb]);
        }
        if mb != me {
            write(&mut out, old, old_color, &minus[mb..me]);
        }
        if pb != pe {
            write(&mut out, new, new_color, &plus[pb..pe]);
        }
        current = pe;
    }
    if current != plus.len() {
        write(&mut out, ctx, "", &plus[current..]);
    }
    out
}

/// git's `diff_words_fill`: word spans (after a fake empty 0th word) and the
/// words one per line for diffing.
fn words(text: &[u8], regex: Option<&Regex>) -> (Vec<(usize, usize)>, Vec<u8>) {
    let mut spans = vec![(0, 0)];
    let mut lines = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let Some((b, e)) = next_word(text, regex, i) else {
            break;
        };
        spans.push((b, e));
        lines.extend_from_slice(&text[b..e]);
        lines.push(b'\n');
        i = e;
    }
    (spans, lines)
}

/// git's `find_word_boundaries` from `begin`.
fn next_word(text: &[u8], regex: Option<&Regex>, mut begin: usize) -> Option<(usize, usize)> {
    if let Some(re) = regex {
        while begin < text.len() {
            let (so, eo) = re.exec(&text[begin..])?[0]?;
            let nl = text[begin + so..begin + eo]
                .iter()
                .position(|&c| c == b'\n');
            let end = nl.map_or(begin + eo, |p| begin + so + p);
            begin += so;
            if begin == end {
                begin += 1;
            } else {
                return (begin < end).then_some((begin, end));
            }
        }
    }
    while begin < text.len() && is_space(text[begin]) {
        begin += 1;
    }
    if begin >= text.len() {
        return None;
    }
    let mut end = begin + 1;
    while end < text.len() && !is_space(text[end]) {
        end += 1;
    }
    Some((begin, end))
}

/// The zero-context hunks of a line diff as xdiff reports them: `(old
/// first, old count, new first, new count)`, a first 1-based unless its
/// count is 0, when it is the line before.
fn word_changes(a: &[u8], b: &[u8]) -> Vec<(usize, usize, usize, usize)> {
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(0).interhunk_lines(0).force_text(true);
    let Ok(patch) = git2::Patch::from_buffers(a, None, b, None, Some(&mut opts)) else {
        return Vec::new();
    };
    (0..patch.num_hunks())
        .filter_map(|h| patch.hunk(h).ok())
        .map(|(h, _)| {
            (
                h.old_start() as usize,
                h.old_lines() as usize,
                h.new_start() as usize,
                h.new_lines() as usize,
            )
        })
        .collect()
}

/// Give `files` (one per delta of `diff`) git's hunk headers - function names
/// found by the path's driver - and with `funccontext` git's `-W` hunks.
pub(crate) fn refine(
    repo: &Repository,
    diff: &git2::Diff,
    files: &mut [crate::FileDiff],
    ctx: usize,
    funccontext: bool,
) -> Result<(), GitError> {
    for (idx, file) in files.iter_mut().enumerate() {
        if file.binary || file.hunks.is_empty() {
            continue;
        }
        let Some(delta) = diff.get_delta(idx) else {
            continue;
        };
        let regular = |f: &git2::DiffFile| {
            matches!(
                f.mode(),
                git2::FileMode::Blob | git2::FileMode::BlobExecutable
            )
        };
        let path = |f: &git2::DiffFile| f.path().map(|p| p.to_string_lossy().into_owned());
        let (one, two) = (delta.old_file(), delta.new_file());
        let mut ff = None;
        for (f, reg) in [(&one, regular(&one)), (&two, regular(&two))] {
            let d = match (reg, path(f)) {
                (true, Some(p)) => driver(repo, &p, Fallback::Default)?,
                _ => by_name(repo, "default")?,
            };
            ff = d.and_then(|d| d.funcname);
            if ff.is_some() {
                break;
            }
        }
        let read = |f: &git2::DiffFile| -> Vec<u8> {
            if !f.id().is_zero()
                && let Ok(b) = repo.find_blob(f.id())
            {
                return b.content().to_vec();
            }
            if f.id().is_zero() && !f.exists() {
                return Vec::new();
            }
            path(f)
                .zip(repo.workdir())
                .and_then(|(p, w)| std::fs::read(w.join(p)).ok())
                .unwrap_or_default()
        };
        let old = read(&one);
        let old_lines: Vec<&[u8]> = old.split_inclusive(|&c| c == b'\n').collect();
        if !funccontext {
            let mut funcline = Vec::new();
            let mut prev: i64 = -1;
            for h in &mut file.hunks {
                let Some((at, s1)) = hunk_start(&h.header) else {
                    continue;
                };
                let mut l = s1 - 1;
                let step = if l > prev { -1 } else { 1 };
                while l != prev && 0 <= l && l < old_lines.len() as i64 {
                    if let Some(t) = func_text(ff.as_ref(), old_lines[l as usize], 80) {
                        funcline = t;
                        break;
                    }
                    l += step;
                }
                prev = s1 - 1;
                h.header = header_of(at, &funcline);
            }
            continue;
        }
        let new = read(&two);
        let new_lines: Vec<&[u8]> = new.split_inclusive(|&c| c == b'\n').collect();
        let mut changes: Vec<Change> = Vec::new();
        for h in &file.hunks {
            let Some((i1, i2)) = hunk_starts(&h.header) else {
                continue;
            };
            let (mut i1, mut i2) = (i1, i2);
            let mut open: Option<Change> = None;
            for l in &h.lines {
                match l.origin {
                    crate::LineOrigin::Context => {
                        changes.extend(open.take());
                        i1 += 1;
                        i2 += 1;
                    }
                    crate::LineOrigin::Removed => {
                        open.get_or_insert(Change {
                            i1,
                            chg1: 0,
                            i2,
                            chg2: 0,
                        })
                        .chg1 += 1;
                        i1 += 1;
                    }
                    crate::LineOrigin::Added => {
                        open.get_or_insert(Change {
                            i1,
                            chg1: 0,
                            i2,
                            chg2: 0,
                        })
                        .chg2 += 1;
                        i2 += 1;
                    }
                    crate::LineOrigin::Meta => {}
                }
            }
            changes.extend(open);
        }
        let eof = file
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .find(|l| l.origin == crate::LineOrigin::Meta)
            .map(|l| l.text.clone())
            .unwrap_or_else(|| "\\ No newline at end of file".to_owned());
        file.hunks = emit(&old_lines, &new_lines, &changes, ctx, 0, true, ff.as_ref())
            .into_iter()
            .map(|(header, lines)| {
                let new_start = hunk_starts(&header).map_or(0, |(_, s)| s as u32 + 1);
                let mut out = Vec::new();
                for (origin, text) in lines {
                    out.push(crate::DiffLine {
                        origin: match origin {
                            '+' => crate::LineOrigin::Added,
                            '-' => crate::LineOrigin::Removed,
                            _ => crate::LineOrigin::Context,
                        },
                        text: String::from_utf8_lossy(text.strip_suffix(b"\n").unwrap_or(text))
                            .into_owned(),
                    });
                    if !text.ends_with(b"\n") {
                        out.push(crate::DiffLine {
                            origin: crate::LineOrigin::Meta,
                            text: eof.clone(),
                        });
                    }
                }
                crate::Hunk {
                    header,
                    new_start,
                    lines: out,
                }
            })
            .collect();
    }
    Ok(())
}

/// A hunk header's `@@ -a,b +c,d @@` part and its 0-based old start.
fn hunk_start(header: &str) -> Option<(&str, i64)> {
    let end = header.get(2..)?.find("@@")? + 4;
    let (i1, _) = hunk_starts(header)?;
    Some((&header[..end], i1 as i64))
}

/// The 0-based first old and new lines of a hunk header.
fn hunk_starts(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.strip_prefix("@@ -")?.split(' ');
    let side = |s: &str| -> Option<usize> {
        let (start, count) = s.split_once(',').unwrap_or((s, "1"));
        let (start, count): (usize, usize) = (start.parse().ok()?, count.parse().ok()?);
        Some(if count == 0 { start } else { start - 1 })
    };
    let old = side(parts.next()?)?;
    let new = side(parts.next()?.strip_prefix('+')?)?;
    Some((old, new))
}

/// The regex `--word-diff` splits a file pair with: the given one, else the
/// drivers', else `diff.wordRegex`.
pub fn word_regex(
    repo: &Repository,
    old: Option<&str>,
    new: &str,
) -> Result<Option<Vec<u8>>, GitError> {
    for path in old.into_iter().chain([new]) {
        if let Some(w) = driver(repo, path, Fallback::Default)?.and_then(|d| d.word_regex) {
            return Ok(Some(w));
        }
    }
    Ok(repo
        .config()?
        .get_string("diff.wordRegex")
        .ok()
        .map(String::into_bytes))
}

/// git's builtin drivers: name, funcname, word regex, case-insensitive.
const BUILTINS: &[(&str, &str, &str, bool)] = &[
    (
        "ada",
        concat!(
            "!^(.*[ \t])?(is[ \t]+new|renames|is[ \t]+separate)([ \t].*)?$\n",
            "!^[ \t]*with[ \t].*$\n",
            "^[ \t]*((procedure|function)[ \t]+.*)$\n",
            "^[ \t]*((package|protected|task)[ \t]+.*)$"
        ),
        concat!(
            "[a-zA-Z][a-zA-Z0-9_]*",
            "|[-+]?[0-9][0-9#_.aAbBcCdDeEfF]*([eE][+-]?[0-9_]+)?",
            "|=>|\\.\\.|\\*\\*|:=|/=|>=|<=|<<|>>|<>"
        ),
        true,
    ),
    (
        "bash",
        concat!(
            "^[ \t]*",
            "(",
            "(",
            "([a-zA-Z_][a-zA-Z0-9_]*[ \t]*\\([ \t]*\\))",
            "|",
            "(function[ \t]+[a-zA-Z_][a-zA-Z0-9_]*(([ \t]*\\([ \t]*\\))|([ \t]+)))",
            ")",
            ".*$",
            ")"
        ),
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|\\$[a-zA-Z0-9_]+|\\$\\{",
            "|\\|\\||&&|<<|>>",
            "|==|!=|<=|>=|[-+*/%&|^]=",
            "|:=|:-|:\\+|:\\?|##|%%|\\^\\^|,,",
            "|[-a-zA-Z0-9_]+",
            "|\\(|\\)|\\{|\\}|\\[|\\]"
        ),
        false,
    ),
    (
        "bibtex",
        "(@[a-zA-Z]{1,}[ \t]*\\{{0,1}[ \t]*[^ \t\"@',\\#}{~%]*).*$",
        "[={}\"]|[^={}\" \t]+",
        false,
    ),
    (
        "cpp",
        concat!(
            "!^[ \t]*[A-Za-z_][A-Za-z_0-9]*:[[:space:]]*($|/[/*])\n",
            "^((::[[:space:]]*)?[A-Za-z_].*)$"
        ),
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|[0-9][0-9.]*([Ee][-+]?[0-9]+)?[fFlLuU]*",
            "|0[xXbB][0-9a-fA-F]+[lLuU]*",
            "|\\.[0-9][0-9]*([Ee][-+]?[0-9]+)?[fFlL]?",
            "|[-+*/<>%&^|=!]=|--|\\+\\+|<<=?|>>=?|&&|\\|\\||::|->\\*?|\\.\\*|<=>"
        ),
        false,
    ),
    (
        "csharp",
        concat!(
            "!(^|[ \t]+)",
            "(do|while|for|foreach|if|else|new|default|return|switch|case|throw",
            "|catch|using|lock|fixed)",
            "([ \t(]+|$)\n",
            "^[ \t]*",
            "(",
            "(",
            "[][[:alnum:]@_.]",
            "(<[][[:alnum:]@_, \t<>]+>)?",
            ")+",
            "([ \t]+",
            "([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+",
            ")+",
            "[ \t]*",
            "\\(",
            "[^;]*",
            ")$\n",
            "^[ \t]*(",
            "([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+",
            "([ \t]+",
            "([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+",
            ")+",
            "[^;=:,()]*",
            ")$\n",
            "^[ \t]*(((static|public|internal|private|protected|new|unsafe|sealed|abstract|partial)[ \t]+)*(class|enum|interface|struct|record)[ \t]+.*)$\n",
            "^[ \t]*(namespace[ \t]+.*)$"
        ),
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|[-+0-9.e]+[fFlL]?|0[xXbB]?[0-9a-fA-F]+[lL]?",
            "|[-+*/<>%&^|=!]=|--|\\+\\+|<<=?|>>=?|&&|\\|\\||::|->"
        ),
        false,
    ),
    (
        "css",
        concat!("![:;][[:space:]]*$\n", "^[:[@.#]?[_a-z0-9].*$"),
        concat!("-?[_a-zA-Z][-_a-zA-Z0-9]*", "|-?[0-9]+|\\#[0-9a-fA-F]+"),
        true,
    ),
    (
        "dts",
        concat!("!;\n", "!=\n", "^[ \t]*((/[ \t]*\\{|&?[a-zA-Z_]).*)"),
        concat!("[a-zA-Z0-9,._+?#-]+", "|[-+*/%&^|!~]|>>|<<|&&|\\|\\|"),
        false,
    ),
    (
        "elixir",
        "^[ \t]*((def(macro|module|impl|protocol|p)?|test)[ \t].*)$",
        concat!(
            "[@:]?[a-zA-Z0-9@_?!]+",
            "|[-+]?0[xob][0-9a-fA-F]+",
            "|[-+]?[0-9][0-9_.]*([eE][-+]?[0-9_]+)?",
            "|:?(\\+\\+|--|\\.\\.|~~~|<>|\\^\\^\\^|<?\\|>|<<<?|>?>>|<<?~|~>?>|<~>|<=|>=|===?|!==?|=~|&&&?|\\|\\|\\|?|=>|<-|\\\\\\\\|->)",
            "|:?%[A-Za-z0-9_.]\\{\\}?"
        ),
        false,
    ),
    (
        "fortran",
        concat!(
            "!^([C*]|[ \t]*!)\n",
            "!^[ \t]*MODULE[ \t]+PROCEDURE[ \t]\n",
            "^[ \t]*((END[ \t]+)?(PROGRAM|MODULE|BLOCK[ \t]+DATA",
            "|([^!'\" \t]+[ \t]+)*(SUBROUTINE|FUNCTION))[ \t]+[A-Z].*)$"
        ),
        concat!(
            "[a-zA-Z][a-zA-Z0-9_]*",
            "|\\.([Ee][Qq]|[Nn][Ee]|[Gg][TtEe]|[Ll][TtEe]|[Tt][Rr][Uu][Ee]|[Ff][Aa][Ll][Ss][Ee]|[Aa][Nn][Dd]|[Oo][Rr]|[Nn]?[Ee][Qq][Vv]|[Nn][Oo][Tt])\\.",
            "|[-+]?[0-9.]+([AaIiDdEeFfLlTtXx][Ss]?[-+]?[0-9.]*)?(_[a-zA-Z0-9][a-zA-Z0-9_]*)?",
            "|//|\\*\\*|::|[/<>=]="
        ),
        true,
    ),
    (
        "fountain",
        "^((\\.[^.]|(int|ext|est|int\\.?/ext|i/e)[. ]).*)$",
        "[^ \t-]+",
        true,
    ),
    (
        "golang",
        concat!(
            "^[ \t]*(func[ \t]*.*(\\{[ \t]*)?)\n",
            "^[ \t]*(type[ \t].*(struct|interface)[ \t]*(\\{[ \t]*)?)"
        ),
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|[-+0-9.eE]+i?|0[xX]?[0-9a-fA-F]+i?",
            "|[-+*/<>%&^|=!:]=|--|\\+\\+|<<=?|>>=?|&\\^=?|&&|\\|\\||<-|\\.{3}"
        ),
        false,
    ),
    (
        "html",
        "^[ \t]*(<[Hh][1-6]([ \t].*)?>.*)$",
        "[^<>= \t]+",
        false,
    ),
    ("ini", "^[ \t]*\\[[^]]+\\]", "[^ \t]+", false),
    (
        "java",
        concat!(
            "!^[ \t]*(catch|do|for|if|instanceof|new|return|switch|throw|while)\n",
            "^[ \t]*(([a-z-]+[ \t]+)*(class|enum|interface|record)[ \t]+.*)$\n",
            "^[ \t]*(([A-Za-z_<>&][][?&<>.,A-Za-z_0-9]*[ \t]+)+[A-Za-z_][A-Za-z_0-9]*[ \t]*\\([^;]*)$"
        ),
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|[-+0-9.e]+[fFlL]?|0[xXbB]?[0-9a-fA-F]+[lL]?",
            "|[-+*/<>%&^|=!]=",
            "|--|\\+\\+|<<=?|>>>?=?|&&|\\|\\|"
        ),
        false,
    ),
    (
        "kotlin",
        "^[ \t]*(([a-z]+[ \t]+)*(fun|class|interface)[ \t]+.*)$",
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|0[xXbB][0-9a-fA-F_]+[lLuU]*",
            "|[0-9][0-9_]*([.][0-9_]*)?([Ee][-+]?[0-9]+)?[fFlLuU]*",
            "|[.][0-9][0-9_]*([Ee][-+]?[0-9]+)?[fFlLuU]?",
            "|[-+*/<>%&^|=!]==?|--|\\+\\+|<<=|>>=|&&|\\|\\||->|\\.\\*|!!|[?:.][.:]"
        ),
        false,
    ),
    ("markdown", "^ {0,3}#{1,6}[ \t].*", "[^<>= \t]+", false),
    (
        "matlab",
        "^[[:space:]]*((classdef|function)[[:space:]].*)$|^(%%%?|##)[[:space:]].*$",
        "[a-zA-Z_][a-zA-Z0-9_]*|[-+0-9.e]+|[=~<>]=|\\.[*/\\^']|\\|\\||&&",
        false,
    ),
    (
        "objc",
        concat!(
            "!^[ \t]*(do|for|if|else|return|switch|while)\n",
            "^[ \t]*([-+][ \t]*\\([ \t]*[A-Za-z_][A-Za-z_0-9* \t]*\\)[ \t]*[A-Za-z_].*)$\n",
            "^[ \t]*(([A-Za-z_][A-Za-z_0-9]*[ \t]+)+[A-Za-z_][A-Za-z_0-9]*[ \t]*\\([^;]*)$\n",
            "^(@(implementation|interface|protocol)[ \t].*)$"
        ),
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|[-+0-9.e]+[fFlL]?|0[xXbB]?[0-9a-fA-F]+[lL]?",
            "|[-+*/<>%&^|=!]=|--|\\+\\+|<<=?|>>=?|&&|\\|\\||::|->"
        ),
        false,
    ),
    (
        "pascal",
        concat!(
            "^(((class[ \t]+)?(procedure|function)|constructor|destructor|interface",
            "|implementation|initialization|finalization)[ \t]*.*)$\n",
            "^(.*=[ \t]*(class|record).*)$"
        ),
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|[-+0-9.e]+|0[xXbB]?[0-9a-fA-F]+",
            "|<>|<=|>=|:=|\\.\\."
        ),
        false,
    ),
    (
        "perl",
        concat!(
            "^package .*\n",
            "^sub [[:alnum:]_':]+[ \t]*",
            "(\\([^)]*\\)[ \t]*)?",
            "(:[^;#]*)?",
            "(\\{[ \t]*)?",
            "(#.*)?$\n",
            "^(BEGIN|END|INIT|CHECK|UNITCHECK|AUTOLOAD|DESTROY)[ \t]*",
            "(\\{[ \t]*)?",
            "(#.*)?$\n",
            "^=head[0-9] .*"
        ),
        concat!(
            "[[:alpha:]_'][[:alnum:]_']*",
            "|0[xb]?[0-9a-fA-F_]*",
            "|[0-9a-fA-F_]+(\\.[0-9a-fA-F_]+)?([eE][-+]?[0-9_]+)?",
            "|=>|-[rwxoRWXOezsfdlpSugkbctTBMAC>]|~~|::",
            "|&&=|\\|\\|=|//=|\\*\\*=",
            "|&&|\\|\\||//|\\+\\+|--|\\*\\*|\\.\\.\\.?",
            "|[-+*/%.^&<>=!|]=",
            "|=~|!~",
            "|<<|<>|<=>|>>"
        ),
        false,
    ),
    (
        "php",
        concat!(
            "^[\t ]*(((public|protected|private|static|abstract|final)[\t ]+)*function.*)$\n",
            "^[\t ]*((((final|abstract)[\t ]+)?class|enum|interface|trait).*)$"
        ),
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|[-+0-9.e]+|0[xXbB]?[0-9a-fA-F]+",
            "|[-+*/<>%&^|=!.]=|--|\\+\\+|<<=?|>>=?|===|&&|\\|\\||::|->"
        ),
        false,
    ),
    (
        "python",
        "^[ \t]*((class|(async[ \t]+)?def)[ \t].*)$",
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|[-+0-9.e]+[jJlL]?|0[xX]?[0-9a-fA-F]+[lL]?",
            "|[-+*/<>%&^|=!]=|//=?|<<=?|>>=?|\\*\\*=?"
        ),
        false,
    ),
    (
        "ruby",
        "^[ \t]*((class|module|def)[ \t].*)$",
        concat!(
            "(@|@@|\\$)?[a-zA-Z_][a-zA-Z0-9_]*",
            "|[-+0-9.e]+|0[xXbB]?[0-9a-fA-F]+|\\?(\\\\C-)?(\\\\M-)?.",
            "|//=?|[-+*/<>%&^|=!]=|<<=?|>>=?|===|\\.{1,3}|::|[!=]~"
        ),
        false,
    ),
    (
        "rust",
        "^[\t ]*((pub(\\([^\\)]+\\))?[\t ]+)?((async|const|unsafe|extern([\t ]+\"[^\"]+\"))[\t ]+)?(struct|enum|union|mod|trait|fn|impl|macro_rules!)[< \t]+[^;]*)$",
        concat!(
            "[a-zA-Z_][a-zA-Z0-9_]*",
            "|[0-9][0-9_a-fA-Fiosuxz]*(\\.([0-9]*[eE][+-]?)?[0-9_fF]*)?",
            "|[-+*\\/<>%&^|=!:]=|<<=?|>>=?|&&|\\|\\||->|=>|\\.{2}=|\\.{3}|::"
        ),
        false,
    ),
    (
        "scheme",
        "^[\t ]*(\\(((define|def(struct|syntax|class|method|rules|record|proto|alias)?)[-*/ \t]|(library|module|struct|class)[*+ \t]).*)$",
        concat!("\\|([^\\\\]*)\\|", "|([^][)(}{[ \t])+"),
        false,
    ),
    (
        "tex",
        "^(\\\\((sub)*section|chapter|part)\\*{0,1}\\{.*)$",
        "\\\\[a-zA-Z@]+|\\\\.|([a-zA-Z0-9]|[^\x01-\x7f])+",
        false,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_compile_and_match_like_git() {
        for (name, f, w, icase) in BUILTINS {
            let flags = EXTENDED | if *icase { ICASE } else { 0 };
            assert!(Funcname::new(f, flags).is_ok(), "{name}");
            assert!(
                Regex::new(&builtin_word_regex(w), EXTENDED | NEWLINE).is_ok(),
                "{name}"
            );
        }
        let rust =
            Funcname::new(BUILTINS.iter().find(|b| b.0 == "rust").unwrap().1, EXTENDED).unwrap();
        assert_eq!(
            func_text(Some(&rust), b"pub fn a(x: u8) {  \n", 80).unwrap(),
            b"pub fn a(x: u8) {"
        );
        assert!(func_text(Some(&rust), b"    let x = 1;\n", 80).is_none());
        assert_eq!(func_text(None, b"abc  \n", 2).unwrap(), b"ab");
        let out = word_diff(b"a b c\n", b"a x c\n", None, WordStyle::Plain, false);
        assert_eq!(out, b"a [-b-]{+x+} c\n");
    }
}
