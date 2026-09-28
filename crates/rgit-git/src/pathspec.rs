//! git's pathspecs (pathspec.c, dir.c's match_pathspec, wildmatch.c): the
//! `:(top,exclude,icase,literal,glob,attr:...)` magic and its short forms,
//! the GIT_*_PATHSPECS settings, and matching paths from the top level.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::GitError;

#[derive(Clone, PartialEq)]
enum Want {
    Set,
    Unset,
    Unspecified,
    Value(String),
}

#[derive(Clone, Default)]
struct Item {
    pattern: String,
    exclude: bool,
    icase: bool,
    glob: bool,
    /// Bytes of `pattern` before its first wildcard (all of it when literal).
    nowild: usize,
    attrs: Vec<(String, Want)>,
    magic: bool,
    /// The pathspec as typed, for messages.
    orig: String,
}

/// A parsed pathspec. Patterns are from the top of the work tree; the CLI
/// rewrites what is typed in a subfolder before it gets here.
#[derive(Clone, Default)]
pub struct Pathspec {
    items: Vec<Item>,
    plain: bool,
    git_dir: OnceLock<Option<PathBuf>>,
}

fn env_on(key: &str) -> bool {
    std::env::var(key).is_ok_and(|v| git2::Config::parse_bool(v).unwrap_or(false))
}

/// A pathspec's magic words (short forms spelled long) and the pattern after
/// them; None for a plain pathspec, or any under GIT_LITERAL_PATHSPECS.
fn split_magic(elt: &str) -> Result<Option<(Vec<&str>, &str)>, GitError> {
    let Some(after) = elt.strip_prefix(':') else {
        return Ok(None);
    };
    if env_on("GIT_LITERAL_PATHSPECS") {
        return Ok(None);
    }
    if let Some(long) = after.strip_prefix('(') {
        let Some(end) = long.find(')') else {
            return Err(bad(format!(
                "Missing ')' at the end of pathspec magic in '{elt}'"
            )));
        };
        let words = long[..end].split(',').filter(|w| !w.is_empty()).collect();
        return Ok(Some((words, &long[end + 1..])));
    }
    let n = after
        .find(|c: char| !matches!(c, '/' | '!' | '^'))
        .unwrap_or(after.len());
    let mut words = Vec::new();
    if after[..n].contains('/') {
        words.push("top");
    }
    if after[..n].contains(['!', '^']) {
        words.push("exclude");
    }
    Ok(Some((
        words,
        after[n..].strip_prefix(':').unwrap_or(&after[n..]),
    )))
}

/// `spec` as typed in a subfolder, its pattern made a path from the top by
/// `fix` unless it has `top` magic; other magic stays, spelled long.
pub fn relocate_pathspec(spec: &str, fix: impl FnOnce(&str) -> String) -> String {
    match split_magic(spec) {
        Ok(Some((words, pattern))) => {
            let top = words.contains(&"top");
            let rest: Vec<&str> = words.into_iter().filter(|w| *w != "top").collect();
            let pattern = if top {
                pattern.to_owned()
            } else {
                fix(pattern)
            };
            match (rest.is_empty(), pattern.is_empty()) {
                (true, true) => ".".to_owned(),
                (true, false) => pattern,
                _ => format!(":({}){pattern}", rest.join(",")),
            }
        }
        Ok(None) => fix(spec),
        Err(_) => spec.to_owned(),
    }
}

/// Fail as git does on a pathspec with bad magic.
pub fn check_pathspecs(specs: &[String]) -> Result<(), GitError> {
    Pathspec::new(specs).map(drop)
}

/// Whether every one of `specs` is an exclude, which commands that default
/// to the current folder (ls-files, grep, clean) limit to it.
pub fn only_excludes(specs: &[String]) -> bool {
    !specs.is_empty() && Pathspec::new(specs).is_ok_and(|s| s.items.iter().all(|i| i.exclude))
}

/// Whether no GIT_*_PATHSPECS setting changes how plain pathspecs match.
pub(crate) fn plain_env() -> bool {
    ![
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ]
    .iter()
    .any(|k| env_on(k))
}

fn bad(msg: String) -> GitError {
    GitError::Cli(format!("fatal: {msg}"))
}

/// The value of one `attr:` requirement list, as git's parse_pathspec_attr_match.
fn parse_attrs(spec: &str, elt: &str) -> Result<Vec<(String, Want)>, GitError> {
    let mut out = Vec::new();
    for word in spec.split(' ').filter(|w| !w.is_empty()) {
        let (name, want) = if let Some(n) = word.strip_prefix('-') {
            (n.to_owned(), Want::Unset)
        } else if let Some(n) = word.strip_prefix('!') {
            (n.to_owned(), Want::Unspecified)
        } else if let Some((n, v)) = word.split_once('=') {
            (n.to_owned(), Want::Value(v.replace('\\', "")))
        } else {
            (word.to_owned(), Want::Set)
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        {
            return Err(bad(format!("invalid attribute name {name}")));
        }
        out.push((name, want));
    }
    if out.is_empty() {
        return Err(bad(format!("attr spec must not be empty: '{elt}'")));
    }
    Ok(out)
}

impl Pathspec {
    /// Parse `specs` as git does, failing with git's message on bad magic or
    /// clashing global settings.
    pub fn new<I, T>(specs: I) -> Result<Self, GitError>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let (literal, glob, noglob, icase) = (
            env_on("GIT_LITERAL_PATHSPECS"),
            env_on("GIT_GLOB_PATHSPECS"),
            env_on("GIT_NOGLOB_PATHSPECS"),
            env_on("GIT_ICASE_PATHSPECS"),
        );
        if glob && noglob {
            return Err(bad(
                "global 'glob' and 'noglob' pathspec settings are incompatible".into(),
            ));
        }
        if literal && (glob || noglob || icase) {
            return Err(bad(
                "global 'literal' pathspec setting is incompatible with all other global pathspec settings".into(),
            ));
        }
        let mut items = Vec::new();
        for spec in specs {
            let elt = spec.as_ref();
            let mut item = Item::default();
            let mut is_literal = literal;
            let mut is_glob = false;
            let mut rest = elt;
            if let Some((words, pattern)) = split_magic(elt)? {
                item.magic = true;
                rest = pattern;
                for word in words {
                    match word {
                        "top" => {}
                        "exclude" => item.exclude = true,
                        "icase" => item.icase = true,
                        "literal" => is_literal = true,
                        "glob" => is_glob = true,
                        w if w.starts_with("prefix:") => {}
                        w if w.starts_with("attr:") => {
                            if !item.attrs.is_empty() {
                                return Err(bad(
                                    "Only one 'attr:' specification is allowed.".into()
                                ));
                            }
                            item.attrs = parse_attrs(&w[5..], elt)?;
                        }
                        w => {
                            return Err(bad(format!("Invalid pathspec magic '{w}' in '{elt}'")));
                        }
                    }
                }
            }
            if is_literal && is_glob {
                return Err(bad(format!("{elt}: 'literal' and 'glob' are incompatible")));
            }
            if !is_literal && !is_glob {
                is_glob = glob;
                is_literal = noglob;
            }
            item.glob = is_glob;
            item.icase |= icase;
            let pattern = rest.strip_prefix("./").unwrap_or(rest);
            item.pattern = if pattern == "." {
                String::new()
            } else {
                pattern.to_owned()
            };
            item.nowild = if is_literal {
                item.pattern.len()
            } else {
                item.pattern
                    .find(['*', '?', '[', '\\'])
                    .unwrap_or(item.pattern.len())
            };
            item.orig = elt.to_owned();
            items.push(item);
        }
        let plain = !(literal || glob || noglob || icase) && items.iter().all(|i| !i.magic);
        Ok(Self {
            items,
            plain,
            git_dir: OnceLock::new(),
        })
    }

    /// Whether libgit2's own pathspec matching gives git's answers: no magic
    /// and no GIT_*_PATHSPECS setting.
    pub fn is_plain(&self) -> bool {
        self.plain
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether `path` (from the top; a trailing `/` for a folder) matches:
    /// some positive item (every path when there are none) and no exclude.
    pub fn matches(&self, path: &str) -> bool {
        if self.items.is_empty() {
            return true;
        }
        let mut positive = self.items.iter().filter(|i| !i.exclude).peekable();
        let pos = positive.peek().is_none() || positive.any(|i| self.hit(i, path));
        pos && !self
            .items
            .iter()
            .filter(|i| i.exclude)
            .any(|i| self.hit(i, path))
    }

    /// [`Self::matches`] for a `Path`.
    pub fn matches_path(&self, path: &Path) -> bool {
        self.matches(&path.to_string_lossy())
    }

    /// Whether files inside the folder `dir` (ending in `/`) may match, for
    /// a folder libgit2 reports without recursing into it.
    fn may_contain(&self, dir: &str) -> bool {
        if self.matches(dir) {
            return true;
        }
        let excluded = self
            .items
            .iter()
            .filter(|i| i.exclude)
            .any(|i| self.hit(i, dir));
        !excluded
            && self.items.iter().filter(|i| !i.exclude).any(|i| {
                let lit = &i.pattern.as_bytes()[..i.nowild];
                let n = lit.len().min(dir.len());
                eq(i.icase, &lit[..n], &dir.as_bytes()[..n])
            })
    }

    /// The index paths that match.
    pub fn match_index(&self, index: &git2::Index) -> Vec<String> {
        index
            .iter()
            .map(|e| String::from_utf8_lossy(&e.path).into_owned())
            .filter(|p| self.matches(p))
            .collect()
    }

    /// The files (and submodules) of `tree` that match.
    pub fn match_tree(&self, tree: &git2::Tree) -> Vec<String> {
        let mut out = Vec::new();
        let _ = tree.walk(git2::TreeWalkMode::PreOrder, |dir, e| {
            if e.kind() != Some(git2::ObjectType::Tree) {
                let path = format!("{dir}{}", String::from_utf8_lossy(e.name_bytes()));
                if self.matches(&path) {
                    out.push(path);
                }
            }
            git2::TreeWalkResult::Ok
        });
        out
    }

    /// Whether some file under the work tree `root` matches.
    pub fn any_in_workdir(&self, root: &Path) -> bool {
        fn walk(spec: &Pathspec, root: &Path, rel: &str) -> bool {
            let Ok(entries) = std::fs::read_dir(root.join(rel)) else {
                return false;
            };
            entries.flatten().any(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if rel.is_empty() && name == ".git" {
                    return false;
                }
                let path = format!("{rel}{name}");
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    spec.may_contain(&format!("{path}/")) && walk(spec, root, &format!("{path}/"))
                } else {
                    spec.matches(&path)
                }
            })
        }
        walk(self, root, "")
    }

    /// Each positive item (an implicit `.` when all are excludes) as typed,
    /// and how the best of `names` matched it: 0 not at all, 1 as a path
    /// inside it, 2 by wildcard, 3 exactly, as git's `seen` for rm.
    pub fn seen(&self, names: &[String]) -> Vec<(String, u8)> {
        let positive: Vec<&Item> = self.items.iter().filter(|i| !i.exclude).collect();
        if positive.is_empty() {
            let any = names.iter().any(|n| self.matches(n));
            return vec![(".".to_owned(), u8::from(any))];
        }
        positive
            .into_iter()
            .map(|i| {
                let how = names.iter().map(|n| self.how(i, n)).max().unwrap_or(0);
                (i.orig.clone(), how)
            })
            .collect()
    }

    fn hit(&self, item: &Item, name: &str) -> bool {
        self.how(item, name) > 0
    }

    fn how(&self, item: &Item, name: &str) -> u8 {
        if !item.attrs.is_empty() && !self.attrs_hold(item, name) {
            return 0;
        }
        let (pat, name) = (item.pattern.as_bytes(), name.as_bytes());
        if pat.is_empty() {
            return 1;
        }
        let (m, n, w) = (pat.len(), name.len(), item.nowild);
        if m <= n && eq(item.icase, pat, &name[..m]) {
            if m == n {
                return 3;
            }
            if pat[m - 1] == b'/' || name[m] == b'/' {
                return 1;
            }
        } else if m == n + 1 && pat[n] == b'/' && eq(item.icase, &pat[..n], name) {
            // `dir/` names the folder `dir` itself.
            return 3;
        }
        if w < m
            && w <= n
            && eq(item.icase, &pat[..w], &name[..w])
            && wildmatch(&pat[w..], &name[w..], item.glob, item.icase)
        {
            2
        } else {
            0
        }
    }

    fn attrs_hold(&self, item: &Item, name: &str) -> bool {
        let git_dir = self.git_dir.get_or_init(|| {
            git2::Repository::open_from_env()
                .ok()
                .map(|r| r.path().to_path_buf())
        });
        let Some(git_dir) = git_dir else {
            return false;
        };
        let names: Vec<String> = item.attrs.iter().map(|(n, _)| n.clone()).collect();
        let path = name.trim_end_matches('/').to_owned();
        let Ok(rows) = crate::attr::check_attr(git_dir, &names, &[path], false) else {
            return false;
        };
        item.attrs.iter().all(|(n, want)| {
            let got = rows
                .iter()
                .find(|(_, a, _)| a == n)
                .map_or("unspecified", |(_, _, v)| v.as_str());
            match want {
                Want::Set => got == "set",
                Want::Unset => got == "unset",
                Want::Unspecified => got == "unspecified",
                Want::Value(v) => !matches!(got, "set" | "unset" | "unspecified") && got == v,
            }
        })
    }
}

fn eq(icase: bool, a: &[u8], b: &[u8]) -> bool {
    if icase {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Hand `specs` to a libgit2 diff: as its own pathspecs when libgit2 matches
/// them as git would, else filtered here, delta by delta.
pub(crate) fn limit_diff(opts: &mut git2::DiffOptions, specs: &[String]) -> Result<(), GitError> {
    let spec = Pathspec::new(specs)?;
    if spec.is_plain() {
        // libgit2 has no `.` pathspec; it means the whole tree, the same as none.
        for p in specs.iter().filter(|p| *p != ".") {
            opts.pathspec(p);
        }
        return Ok(());
    }
    if spec.is_empty() {
        return Ok(());
    }
    let spec = intern(specs, spec);
    // SAFETY: the payload is an interned Pathspec that lives as long as the
    // process; the options struct is owned by `opts`.
    unsafe {
        let raw = opts.raw().cast_mut();
        (*raw).notify_cb = Some(notify);
        (*raw).payload = (spec as *const Pathspec).cast_mut().cast();
    }
    Ok(())
}

/// One leaked Pathspec per distinct list, for libgit2 callbacks to point at.
fn intern(specs: &[String], spec: Pathspec) -> &'static Pathspec {
    static SEEN: Mutex<Vec<(Vec<String>, &'static Pathspec)>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, s)) = seen.iter().find(|(k, _)| k == specs) {
        return s;
    }
    let s: &'static Pathspec = Box::leak(Box::new(spec));
    seen.push((specs.to_vec(), s));
    s
}

extern "C" fn notify(
    _diff: *const libgit2_sys::git_diff,
    delta: *const libgit2_sys::git_diff_delta,
    _matched: *const std::ffi::c_char,
    payload: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: libgit2 passes the delta it is about to add and our payload.
    let (spec, delta) = unsafe { (&*(payload as *const Pathspec), &*delta) };
    let path = |p: *const std::ffi::c_char| {
        (!p.is_null())
            // SAFETY: libgit2's delta paths are NUL-terminated.
            .then(|| {
                unsafe { std::ffi::CStr::from_ptr(p) }
                    .to_string_lossy()
                    .into_owned()
            })
    };
    let hit = [delta.old_file.path, delta.new_file.path]
        .into_iter()
        .filter_map(path)
        .any(|p| {
            if p.ends_with('/') {
                spec.may_contain(&p)
            } else {
                spec.matches(&p)
            }
        });
    if hit { 0 } else { 1 }
}

/// libgit2's `reset_default` over `specs`; magic ones go to it as the paths
/// they match in the index or `target`.
pub(crate) fn reset_default(
    repo: &git2::Repository,
    target: Option<&git2::Object>,
    specs: &[String],
) -> Result<(), GitError> {
    let spec = Pathspec::new(specs)?;
    if spec.is_plain() {
        return Ok(repo.reset_default(target, specs)?);
    }
    let mut paths = spec.match_index(&repo.index()?);
    if let Some(t) = target {
        paths.extend(spec.match_tree(&t.peel_to_tree()?));
    }
    paths.sort();
    paths.dedup();
    if !paths.is_empty() {
        repo.reset_default(target, &paths)?;
    }
    Ok(())
}

/// Run a libgit2 index walk (`add_all`, `update_all`) over `specs`: libgit2's
/// own pathspecs when plain, else every path, kept here. `skip` leaves
/// further paths alone (a non-zero return), as the sparse checkout asks.
pub(crate) fn index_walk<F>(
    specs: &[String],
    mut skip: impl FnMut(&Path, &[u8]) -> i32,
    run: F,
) -> Result<(), GitError>
where
    F: FnOnce(&[String], Option<&mut git2::IndexMatchedPath<'_>>) -> Result<(), git2::Error>,
{
    let spec = Pathspec::new(specs)?;
    if spec.is_plain() {
        return Ok(run(specs, Some(&mut skip))?);
    }
    let mut keep = |p: &Path, m: &[u8]| {
        if spec.matches(&p.to_string_lossy()) {
            skip(p, m)
        } else {
            1
        }
    };
    Ok(run(&["*".to_owned()], Some(&mut keep))?)
}

const MATCH: i32 = 0;
const NOMATCH: i32 = 1;
const ABORT_ALL: i32 = -1;
const ABORT_TO_STARSTAR: i32 = -2;

/// git's wildmatch: `*`, `?` and `[...]` (with `[:class:]`), and with
/// `pathname` (`:(glob)`), `*` and `?` not crossing `/` while `**` does.
pub(crate) fn wildmatch(pattern: &[u8], text: &[u8], pathname: bool, icase: bool) -> bool {
    dowild(pattern, text, pathname, icase) == MATCH
}

fn dowild(pat: &[u8], text: &[u8], pathname: bool, icase: bool) -> i32 {
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    let fold = |c: u8| if icase { c.to_ascii_lowercase() } else { c };
    let (mut p, mut t) = (0usize, 0usize);
    while at(pat, p) != 0 {
        let mut p_ch = fold(at(pat, p));
        let mut t_ch = at(text, t);
        if t_ch == 0 && p_ch != b'*' {
            return ABORT_ALL;
        }
        t_ch = fold(t_ch);
        match p_ch {
            b'?' => {
                if pathname && t_ch == b'/' {
                    return NOMATCH;
                }
            }
            b'*' => {
                p += 1;
                let match_slash;
                if at(pat, p) == b'*' {
                    let prev = p.checked_sub(2);
                    while at(pat, p) == b'*' {
                        p += 1;
                    }
                    if prev.is_none_or(|i| pat[i] == b'/')
                        && (at(pat, p) == 0
                            || at(pat, p) == b'/'
                            || (at(pat, p) == b'\\' && at(pat, p + 1) == b'/'))
                    {
                        if at(pat, p) == b'/'
                            && dowild(&pat[p + 1..], &text[t..], pathname, icase) == MATCH
                        {
                            return MATCH;
                        }
                        match_slash = true;
                    } else {
                        match_slash = !pathname;
                    }
                } else {
                    match_slash = !pathname;
                }
                if at(pat, p) == 0 {
                    if !match_slash && text[t..].contains(&b'/') {
                        return NOMATCH;
                    }
                    return MATCH;
                } else if !match_slash && at(pat, p) == b'/' {
                    let Some(slash) = text[t..].iter().position(|&c| c == b'/') else {
                        return NOMATCH;
                    };
                    t += slash;
                    p += 1;
                    t += 1;
                    continue;
                }
                loop {
                    if t_ch == 0 {
                        break;
                    }
                    if !matches!(at(pat, p), b'*' | b'?' | b'[' | b'\\') {
                        let want = fold(at(pat, p));
                        loop {
                            t_ch = at(text, t);
                            if t_ch == 0 || (!match_slash && t_ch == b'/') {
                                break;
                            }
                            t_ch = fold(t_ch);
                            if t_ch == want {
                                break;
                            }
                            t += 1;
                        }
                        if t_ch != want {
                            return NOMATCH;
                        }
                    }
                    let matched = dowild(&pat[p..], &text[t..], pathname, icase);
                    if matched != NOMATCH {
                        if !match_slash || matched != ABORT_TO_STARSTAR {
                            return matched;
                        }
                    } else if !match_slash && t_ch == b'/' {
                        return ABORT_TO_STARSTAR;
                    }
                    t += 1;
                    t_ch = fold(at(text, t));
                }
                return ABORT_ALL;
            }
            b'[' => {
                p += 1;
                p_ch = at(pat, p);
                if p_ch == b'^' {
                    p_ch = b'!';
                }
                let negated = p_ch == b'!';
                if negated {
                    p += 1;
                    p_ch = at(pat, p);
                }
                let mut prev_ch = 0u8;
                let mut matched = false;
                loop {
                    if p_ch == 0 {
                        return ABORT_ALL;
                    }
                    if p_ch == b'\\' {
                        p += 1;
                        p_ch = at(pat, p);
                        if p_ch == 0 {
                            return ABORT_ALL;
                        }
                        if t_ch == fold(p_ch) {
                            matched = true;
                        }
                    } else if p_ch == b'-'
                        && prev_ch != 0
                        && at(pat, p + 1) != 0
                        && at(pat, p + 1) != b']'
                    {
                        p += 1;
                        p_ch = at(pat, p);
                        if p_ch == b'\\' {
                            p += 1;
                            p_ch = at(pat, p);
                            if p_ch == 0 {
                                return ABORT_ALL;
                            }
                        }
                        let raw_t = at(text, t);
                        if (raw_t <= p_ch && raw_t >= prev_ch)
                            || (icase && {
                                let up = raw_t.to_ascii_uppercase();
                                let low = raw_t.to_ascii_lowercase();
                                (up <= p_ch && up >= prev_ch) || (low <= p_ch && low >= prev_ch)
                            })
                        {
                            matched = true;
                        }
                        p_ch = 0;
                    } else if p_ch == b'[' && at(pat, p + 1) == b':' {
                        p += 2;
                        let s = p;
                        while at(pat, p) != 0 && at(pat, p) != b']' {
                            p += 1;
                        }
                        if at(pat, p) == 0 {
                            return ABORT_ALL;
                        }
                        if p == s || at(pat, p - 1) != b':' {
                            p = s - 2;
                            p_ch = b'[';
                            if t_ch == p_ch {
                                matched = true;
                            }
                        } else {
                            let c = at(text, t);
                            let hit = match &pat[s..p - 1] {
                                b"alnum" => c.is_ascii_alphanumeric(),
                                b"alpha" => c.is_ascii_alphabetic(),
                                b"blank" => c == b' ' || c == b'\t',
                                b"cntrl" => c.is_ascii_control(),
                                b"digit" => c.is_ascii_digit(),
                                b"graph" => c.is_ascii_graphic(),
                                b"lower" => {
                                    c.is_ascii_lowercase() || (icase && c.is_ascii_uppercase())
                                }
                                b"print" => c.is_ascii_graphic() || c == b' ',
                                b"punct" => c.is_ascii_punctuation(),
                                b"space" => c.is_ascii_whitespace() || c == 11,
                                b"upper" => {
                                    c.is_ascii_uppercase() || (icase && c.is_ascii_lowercase())
                                }
                                b"xdigit" => c.is_ascii_hexdigit(),
                                _ => return ABORT_ALL,
                            };
                            matched |= hit;
                            p_ch = 0;
                        }
                    } else if t_ch == fold(p_ch) {
                        matched = true;
                    }
                    prev_ch = p_ch;
                    p += 1;
                    p_ch = at(pat, p);
                    if p_ch == b']' {
                        break;
                    }
                }
                if matched == negated || (pathname && t_ch == b'/') {
                    return NOMATCH;
                }
            }
            _ => {
                if p_ch == b'\\' {
                    p += 1;
                    p_ch = fold(at(pat, p));
                }
                if t_ch != p_ch {
                    return NOMATCH;
                }
            }
        }
        p += 1;
        t += 1;
    }
    if at(text, t) != 0 { NOMATCH } else { MATCH }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildmatch_like_git() {
        let w = |p: &str, t: &str, glob: bool| wildmatch(p.as_bytes(), t.as_bytes(), glob, false);
        assert!(w("*.c", "a/b.c", false));
        assert!(!w("*.c", "a/b.c", true));
        assert!(w("**/*.c", "a/b/c.c", true));
        assert!(w("**/*.c", "c.c", true));
        assert!(w("a/**", "a/b/c", true));
        assert!(w("a/**/b", "a/b", true));
        assert!(!w("a/?", "a/bc", true));
        assert!(w("[!a]x", "bx", true));
        assert!(w("[[:digit:]]", "7", true));
        assert!(!w("a*", "a/b", true));
        assert!(wildmatch(b"A*", b"abc", true, true));
    }

    #[test]
    fn magic_and_excludes() {
        let s = Pathspec::new([":(exclude)dir/sub", "dir"]).unwrap();
        assert!(s.matches("dir/a") && !s.matches("dir/sub/b") && !s.matches("c"));
        let s = Pathspec::new([":!*.txt"]).unwrap();
        assert!(s.matches("a.md") && !s.matches("x/a.txt"));
        let s = Pathspec::new([":(icase)DIR/A"]).unwrap();
        assert!(s.matches("dir/a"));
        let s = Pathspec::new([":(glob)*.txt"]).unwrap();
        assert!(s.matches("c.txt") && !s.matches("d/c.txt"));
        let s = Pathspec::new([":(literal)*.txt"]).unwrap();
        assert!(!s.matches("c.txt") && s.matches("*.txt"));
        assert!(Pathspec::new([":(literal,glob)x"]).is_err());
        assert!(Pathspec::new([":(bogus)x"]).is_err());
        let s = Pathspec::new([":/dir"]).unwrap();
        assert!(s.matches("dir/a") && !s.is_plain());
        assert!(Pathspec::new(["dir"]).unwrap().is_plain());
    }
}
