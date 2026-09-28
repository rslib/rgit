//! `git archive` natively: the tar, tgz and zip bytes git writes, its export
//! attributes (`export-ignore`, `export-subst`), and the client side of the
//! upload-archive protocol for `--remote`.

use std::path::Path;

use git2::{Commit, Oid, Repository, Tree};

use crate::GitError;

/// `git archive` options.
#[derive(Debug, Clone, Default)]
pub struct ArchiveOpts {
    /// The revision (default HEAD).
    pub rev: String,
    /// tar, tgz, tar.gz or zip.
    pub format: String,
    /// Put every entry under this folder.
    pub prefix: String,
    /// Limit to these paths.
    pub paths: Vec<String>,
    /// zlib level 0-9 for tgz and zip.
    pub level: Option<u32>,
    /// Untracked files to add as `(path in the archive, mode, content)`.
    pub extra: Vec<(String, i32, Vec<u8>)>,
    /// Also read .gitattributes from the working tree.
    pub worktree_attributes: bool,
    /// The entries' modification time (default: the commit's).
    pub mtime: Option<i64>,
    /// Print each archived path on stderr.
    pub verbose: bool,
    /// The current folder below the top (`src/`): only it is archived, with
    /// paths relative to it, and `paths` are relative to it.
    pub cwd: String,
}

/// One `.gitattributes` line: its folder, pattern and settings.
struct Rule {
    base: String,
    pattern: regex::Regex,
    /// Matched against the whole path below `base`, not just the name.
    anchored: bool,
    attrs: Vec<(String, bool)>,
}

/// The attribute rules that apply to `tree`, lowest precedence first: the
/// tree's own .gitattributes files (shallow before deep), the working tree's
/// with `worktree`, then $GIT_DIR/info/attributes.
pub(crate) struct Attributes(Vec<Rule>);

impl Attributes {
    pub(crate) fn load(repo: &Repository, tree: &Tree, worktree: bool) -> Result<Self, GitError> {
        let mut files: Vec<(String, String)> = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
            if e.name().ok() == Some(".gitattributes")
                && let Ok(blob) = repo.find_blob(e.id())
            {
                files.push((
                    root.to_owned(),
                    String::from_utf8_lossy(blob.content()).into_owned(),
                ));
            }
            git2::TreeWalkResult::Ok
        })?;
        if worktree && let Some(workdir) = repo.workdir() {
            let mut local = Vec::new();
            let mut bases: Vec<String> = files.iter().map(|(b, _)| b.clone()).collect();
            if !bases.iter().any(String::is_empty) {
                bases.push(String::new());
            }
            for base in &bases {
                if let Ok(text) = std::fs::read_to_string(workdir.join(base).join(".gitattributes"))
                {
                    local.push((base.clone(), text));
                }
            }
            files.extend(local);
        }
        files.sort_by_key(|(base, _)| base.matches('/').count());
        if let Ok(text) = std::fs::read_to_string(repo.path().join("info/attributes")) {
            files.push((String::new(), text));
        }
        let mut rules = Vec::new();
        for (base, text) in files {
            for line in text.lines() {
                let mut words = line.split_whitespace();
                let Some(pat) = words.next().filter(|p| !p.starts_with('#')) else {
                    continue;
                };
                let mut attrs: Vec<(String, bool)> = Vec::new();
                for w in words.filter(|w| !w.starts_with('!')) {
                    match w.strip_prefix('-') {
                        Some(name) => attrs.push((name.to_owned(), false)),
                        // The built-in macro: binary is -diff -merge -text.
                        None if w == "binary" => attrs.extend(
                            [("binary", true), ("diff", false), ("merge", false)]
                                .into_iter()
                                .chain([("text", false)])
                                .map(|(n, on)| (n.to_owned(), on)),
                        ),
                        None => attrs.push((w.split('=').next().unwrap_or(w).to_owned(), true)),
                    }
                }
                let anchored = pat.trim_start_matches('/').contains('/') || pat.starts_with('/');
                let Ok(pattern) = regex::Regex::new(&glob(pat.trim_start_matches('/'))) else {
                    continue;
                };
                rules.push(Rule {
                    base: base.clone(),
                    pattern,
                    anchored,
                    attrs,
                });
            }
        }
        Ok(Attributes(rules))
    }

    /// Whether `attr` is set for `path` (the last matching rule wins).
    pub(crate) fn is_set(&self, path: &str, attr: &str) -> bool {
        self.state(path, attr) == Some(true)
    }

    /// `attr` for `path`: set, unset (`-attr`) or unspecified.
    fn state(&self, path: &str, attr: &str) -> Option<bool> {
        let mut set = None;
        for rule in &self.0 {
            let Some(rel) = path.strip_prefix(&rule.base) else {
                continue;
            };
            let subject = if rule.anchored {
                rel
            } else {
                rel.rsplit('/').next().unwrap_or(rel)
            };
            if rule.pattern.is_match(subject)
                && let Some((_, on)) = rule.attrs.iter().rfind(|(n, _)| n == attr)
            {
                set = Some(*on);
            }
        }
        set
    }
}

/// A gitattributes pattern as an anchored regex: `*` stays inside a folder,
/// `**` crosses them.
fn glob(pat: &str) -> String {
    let mut re = String::from("^");
    let chars: Vec<char> = pat.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    re.push_str(".*");
                    i += 2;
                }
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '[' => {
                let end = chars[i..].iter().position(|c| *c == ']').map(|p| i + p);
                match end {
                    Some(end) => {
                        let class: String = chars[i + 1..end].iter().collect();
                        let class = class
                            .strip_prefix('!')
                            .map_or(class.clone(), |c| format!("^{c}"));
                        re.push_str(&format!("[{}]", class.replace('\\', "\\\\")));
                        i = end;
                    }
                    None => re.push_str("\\["),
                }
            }
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    re
}

/// `$Format:<fmt>$` placeholders in `data` expanded for `commit`, as git's
/// `export-subst` does.
pub(crate) fn export_subst(repo: &Repository, commit: &Commit, data: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(data);
    let mut out = String::with_capacity(text.len());
    let mut rest: &str = &text;
    while let Some(start) = rest.find("$Format:") {
        let after = &rest[start + "$Format:".len()..];
        let Some(end) = after.find('$').filter(|e| !after[..*e].contains('\n')) else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str(&pretty(repo, commit, &after[..end]));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.into_bytes()
}

/// The common `git log --format` placeholders for `c`.
pub(crate) fn pretty(repo: &Repository, c: &Commit, fmt: &str) -> String {
    let short = |id: Oid| {
        repo.find_object(id, None)
            .and_then(|o| o.short_id())
            .ok()
            .and_then(|b| b.as_str().ok().map(str::to_owned))
            .unwrap_or_else(|| id.to_string()[..7].to_owned())
    };
    let msg = String::from_utf8_lossy(c.message_bytes()).into_owned();
    let body = msg.split_once("\n\n").map_or("", |(_, b)| b).trim_end();
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let Some(p) = chars.next() else {
            out.push('%');
            break;
        };
        let who = |author: bool| if author { c.author() } else { c.committer() };
        match p {
            '%' => out.push('%'),
            'n' => out.push('\n'),
            'H' => out.push_str(&c.id().to_string()),
            'h' => out.push_str(&short(c.id())),
            'T' => out.push_str(&c.tree_id().to_string()),
            't' => out.push_str(&short(c.tree_id())),
            'P' => out.push_str(
                &c.parent_ids()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            'p' => out.push_str(&c.parent_ids().map(short).collect::<Vec<_>>().join(" ")),
            's' => out.push_str(c.summary().ok().flatten().unwrap_or("")),
            'b' => {
                if !body.is_empty() {
                    out.push_str(body);
                    out.push('\n');
                }
            }
            'B' => out.push_str(&msg),
            'd' | 'D' => {
                let refs = decorations(repo, c.id());
                if !refs.is_empty() {
                    out.push_str(&if p == 'd' { format!(" ({refs})") } else { refs });
                }
            }
            'a' | 'c' => {
                let sig = who(p == 'a');
                let t = sig.when();
                let field = chars.next().unwrap_or('n');
                let s = match field {
                    'n' | 'N' => String::from_utf8_lossy(sig.name_bytes()).into_owned(),
                    'e' | 'E' => String::from_utf8_lossy(sig.email_bytes()).into_owned(),
                    'l' | 'L' => String::from_utf8_lossy(sig.email_bytes())
                        .split('@')
                        .next()
                        .unwrap_or("")
                        .to_owned(),
                    't' => t.seconds().to_string(),
                    'd' => crate::git_repo::format_git_date(t, "default"),
                    'D' => crate::git_repo::rfc2822_date(t),
                    'i' => crate::git_repo::format_git_date(t, "iso"),
                    'I' => crate::git_repo::format_git_date(t, "iso-strict"),
                    's' => crate::git_repo::format_git_date(t, "short"),
                    other => format!("%{p}{other}"),
                };
                out.push_str(&s);
            }
            other => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out
}

/// `git log --decorate`'s names for `id`: `HEAD -> main, tag: v1, origin/main`.
fn decorations(repo: &Repository, id: Oid) -> String {
    let head = repo.head().ok();
    let head_branch = head
        .as_ref()
        .filter(|h| h.is_branch())
        .and_then(|h| h.name().ok().map(str::to_owned));
    let mut names = Vec::new();
    if head.as_ref().and_then(|h| h.target()) == Some(id) {
        names.push(match &head_branch {
            Some(b) => format!("HEAD -> {}", b.trim_start_matches("refs/heads/")),
            None => "HEAD".to_owned(),
        });
    }
    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            let Some(name) = r.name().ok().map(str::to_owned) else {
                continue;
            };
            if Some(&name) == head_branch.as_ref()
                || r.peel_to_commit().map(|c| c.id()).ok() != Some(id)
            {
                continue;
            }
            if let Some(b) = name.strip_prefix("refs/heads/") {
                names.push(b.to_owned());
            } else if let Some(b) = name.strip_prefix("refs/remotes/") {
                names.push(b.to_owned());
            } else if let Some(t) = name.strip_prefix("refs/tags/") {
                names.push(format!("tag: {t}"));
            }
        }
    }
    names.join(", ")
}

/// One archive member, in git's order.
struct Member {
    /// The name in the archive (folders end in `/`).
    path: String,
    /// The path in the tree, for attributes.
    tree_path: String,
    mode: u32,
    /// Names long paths' pax headers, as git's do.
    oid: Oid,
    data: Vec<u8>,
}

/// git's formats, in `git archive -l` order: tar, the tar filters
/// (`tar.<name>.command`, tgz and tar.gz built in), then zip; each filter with
/// its command.
pub(crate) fn formats(config: Option<&git2::Config>) -> Vec<(String, Option<String>)> {
    let mut filters: Vec<(String, Option<String>)> = ["tgz", "tar.gz"]
        .map(|n| (n.to_owned(), Some(GZIP.to_owned())))
        .into();
    if let Some(entries) = config.and_then(|c| c.entries(Some("^tar\\..*\\.command$")).ok()) {
        let _ = entries.for_each(|e| {
            let (Ok(name), Ok(value)) = (e.name(), e.value()) else {
                return;
            };
            let Some(fmt) = name
                .strip_prefix("tar.")
                .and_then(|n| n.strip_suffix(".command"))
            else {
                return;
            };
            match filters.iter_mut().find(|(n, _)| n == fmt) {
                Some(f) => f.1 = Some(value.to_owned()),
                None => filters.push((fmt.to_owned(), Some(value.to_owned()))),
            }
        });
    }
    let mut out = vec![("tar".to_owned(), None)];
    out.extend(filters);
    out.push(("zip".to_owned(), None));
    out
}

/// The format names `git archive -l` lists, with tar filters from the
/// repository at `dir`'s config, or the global config outside one.
pub fn format_names(dir: Option<&Path>) -> Vec<String> {
    let config = match dir {
        Some(d) => Repository::discover(d).and_then(|r| r.config()),
        None => git2::Config::open_default(),
    };
    formats(config.ok().as_ref())
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// git's in-process gzip filter.
const GZIP: &str = "git archive gzip";

/// The format git infers from an output file name (`x.tar.gz` is tar.gz).
pub fn format_from_filename(name: &str, formats: &[String]) -> Option<String> {
    formats
        .iter()
        .find(|f| {
            name.len() >= f.len() + 2
                && name.ends_with(f.as_str())
                && name.as_bytes()[name.len() - f.len() - 1] == b'.'
        })
        .cloned()
}

/// `path` resolved against the folder `cwd` (`src/`), `..` and `.` folded.
fn from_cwd(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for p in cwd.split('/').chain(path.split('/')) {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// `git archive` natively: the tar, zip or filtered tar git writes, byte for
/// byte.
pub(crate) fn archive(repo: &Repository, o: &ArchiveOpts) -> Result<Vec<u8>, GitError> {
    let obj = repo.revparse_single(&o.rev)?;
    let tree = obj.peel_to_tree()?;
    let commit = obj.peel_to_commit().ok();
    let time = o.mtime.unwrap_or_else(|| match &commit {
        Some(c) => c.time().seconds(),
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64),
    });
    let config = repo.config().ok();
    let format = formats(config.as_ref())
        .into_iter()
        .find(|(n, _)| *n == o.format)
        .ok_or_else(|| GitError::Other(format!("Unknown archive format '{}'", o.format)))?;
    if let Some(level) = o.level.filter(|_| format.0 == "tar") {
        return Err(GitError::Other(format!(
            "Argument not supported for format 'tar': -{level}"
        )));
    }
    let attrs = Attributes::load(repo, &tree, o.worktree_attributes)?;
    let cwd = &o.cwd;
    let specs: Vec<String> = o.paths.iter().map(|p| from_cwd(cwd, p)).collect();
    if !specs.is_empty() {
        let mut all = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
            all.push(format!("{root}{}", e.name().unwrap_or("")));
            git2::TreeWalkResult::Ok
        })?;
        for (spec, raw) in specs.iter().zip(&o.paths) {
            if !cwd.is_empty() && !format!("{spec}/").starts_with(cwd.as_str()) {
                return Err(GitError::Other(format!(
                    "pathspec '{raw}' matches files outside the current directory"
                )));
            }
            let one = std::slice::from_ref(spec);
            if !spec.is_empty()
                && !all
                    .iter()
                    .any(|p| crate::git_repo::pathspec_matches(one, p))
            {
                return Err(GitError::Other(format!(
                    "pathspec '{raw}' did not match any files"
                )));
            }
        }
    }
    let specs: Vec<String> = specs.into_iter().filter(|s| !s.is_empty()).collect();
    let prefix = &o.prefix;
    let mut members = Vec::new();
    let mut names = Vec::new();
    if prefix.ends_with('/') {
        let mut len = prefix.len();
        while len > 1 && prefix.as_bytes()[len - 2] == b'/' {
            len -= 1;
        }
        names.push(prefix[..len].to_owned());
        members.push(Member {
            path: prefix[..len].to_owned(),
            tree_path: String::new(),
            mode: 0o040777,
            oid: tree.id(),
            data: Vec::new(),
        });
    }
    // Folders are written only once a file inside them is.
    let mut pending: Vec<(String, Oid)> = Vec::new();
    let mut failed = None;
    let mut add = |tree_path: String, mode: u32, oid: Oid, data: Vec<u8>| {
        let rel = if cwd.is_empty() {
            &tree_path[..]
        } else {
            match tree_path.strip_prefix(cwd.as_str()) {
                Some(rel) if !rel.is_empty() => rel,
                _ => return,
            }
        };
        let path = format!("{prefix}{rel}");
        names.push(path.clone());
        members.push(Member {
            path,
            tree_path,
            mode,
            oid,
            data,
        });
    };
    tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
        let Ok(name) = e.name() else {
            return git2::TreeWalkResult::Ok;
        };
        while pending
            .last()
            .is_some_and(|(d, _)| !root.starts_with(d.as_str()))
        {
            pending.pop();
        }
        let path = format!("{root}{name}");
        let mode = e.filemode() as u32;
        if mode == 0o040000 {
            if attrs.is_set(&path, "export-ignore") {
                return git2::TreeWalkResult::Skip;
            }
            pending.push((format!("{path}/"), e.id()));
            return git2::TreeWalkResult::Ok;
        }
        if !specs.is_empty() && !crate::git_repo::pathspec_matches(&specs, &path) {
            return git2::TreeWalkResult::Ok;
        }
        // Like git's, a file's folders are written before its own
        // export-ignore is looked at.
        for (dir, id) in pending.drain(..) {
            add(dir, 0o040000, id, Vec::new());
        }
        if attrs.is_set(&path, "export-ignore") {
            return git2::TreeWalkResult::Ok;
        }
        if mode == 0o160000 {
            add(format!("{path}/"), mode, e.id(), Vec::new());
            return git2::TreeWalkResult::Ok;
        }
        let mut data = match repo.find_blob(e.id()) {
            Ok(b) => b.content().to_vec(),
            Err(err) => {
                failed = Some(err);
                return git2::TreeWalkResult::Abort;
            }
        };
        if let Some(c) = commit
            .as_ref()
            .filter(|_| mode != 0o120000 && attrs.is_set(&path, "export-subst"))
        {
            data = export_subst(repo, c, &data);
        }
        add(path, mode, e.id(), data);
        git2::TreeWalkResult::Ok
    })?;
    if let Some(err) = failed {
        return Err(err.into());
    }
    if o.verbose {
        for n in names {
            eprintln!("{n}");
        }
    }
    for (i, (path, mode, data)) in o.extra.iter().enumerate() {
        let mut fake = [0u8; 20];
        fake[..8].copy_from_slice(&(i as u64 + 1).to_be_bytes());
        members.push(Member {
            path: path.clone(),
            tree_path: path.clone(),
            mode: *mode as u32,
            oid: Oid::from_bytes(&fake)?,
            data: data.clone(),
        });
    }
    let commit_id = commit.as_ref().map(Commit::id);
    let level = o.level.map_or(-1, |l| l as i32);
    if format.0 == "zip" {
        return Ok(zip(&members, time, level, commit_id, &attrs));
    }
    let umask = match config
        .as_ref()
        .and_then(|c| c.get_string("tar.umask").ok())
        .as_deref()
    {
        Some("user") => {
            // SAFETY: umask only swaps the process mask; it is restored at once.
            unsafe {
                let m = libc::umask(0);
                libc::umask(m);
                u32::from(m)
            }
        }
        Some(v) => parse_c_int(v).unwrap_or(0o002),
        None => 0o002,
    };
    let tar = tar(&members, time, umask, commit_id);
    match format.1.as_deref() {
        None => Ok(tar),
        Some(GZIP) => gzip(&tar, level),
        Some(cmd) => filter(&tar, cmd, o.level),
    }
}

/// An integer as C's strtol with base 0 reads it: `0x1f`, `022` (octal), `18`.
fn parse_c_int(v: &str) -> Option<u32> {
    let v = v.trim();
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else if v.len() > 1 && v.starts_with('0') {
        u32::from_str_radix(&v[1..], 8).ok()
    } else {
        v.parse().ok()
    }
}

const RECORD: usize = 512;
const BLOCK: usize = RECORD * 20;
const USTAR_MAX: u64 = 0o77777777777;

/// A pax extended header record: `"<len> <key>=<value>\n"`, its length
/// counting itself.
fn pax_record(out: &mut Vec<u8>, key: &str, value: &[u8]) {
    let mut len = 1 + 1 + key.len() + 1 + value.len() + 1;
    let mut tmp = 1;
    while len / 10 >= tmp {
        len += 1;
        tmp *= 10;
    }
    out.extend(format!("{len} {key}=").as_bytes());
    out.extend(value);
    out.push(b'\n');
}

/// Append `data` padded with zeros to a whole record.
fn blocked(out: &mut Vec<u8>, data: &[u8]) {
    out.extend(data);
    out.resize(out.len().div_ceil(RECORD) * RECORD, 0);
}

/// A ustar header as git's prepare_header fills it.
fn ustar(
    name: &[u8],
    prefix: &[u8],
    linkname: &[u8],
    typeflag: u8,
    mode: u32,
    size: u64,
    mtime: u64,
) -> [u8; RECORD] {
    let mut h = [0u8; RECORD];
    let put = |h: &mut [u8; RECORD], at: usize, s: &[u8]| h[at..at + s.len()].copy_from_slice(s);
    put(&mut h, 0, name);
    put(&mut h, 100, format!("{:07o}", mode & 0o7777).as_bytes());
    put(&mut h, 108, b"0000000");
    put(&mut h, 116, b"0000000");
    let regular = mode & 0o170000 == 0o100000;
    put(
        &mut h,
        124,
        format!("{:011o}", if regular { size } else { 0 }).as_bytes(),
    );
    put(&mut h, 136, format!("{mtime:011o}").as_bytes());
    h[156] = typeflag;
    put(&mut h, 157, linkname);
    put(&mut h, 257, b"ustar\0");
    put(&mut h, 263, b"00");
    put(&mut h, 265, b"root");
    put(&mut h, 297, b"root");
    put(&mut h, 329, b"0000000");
    put(&mut h, 337, b"0000000");
    put(&mut h, 345, prefix);
    let sum: u32 = h[..148]
        .iter()
        .chain(&h[156..500])
        .map(|&b| u32::from(b))
        .sum::<u32>()
        + 8 * u32::from(b' ');
    put(&mut h, 148, format!("{sum:07o}").as_bytes());
    h
}

/// Where git splits a long path into ustar's prefix and name: the last `/`
/// within the first `max` bytes, trailing slash aside.
fn path_prefix(path: &[u8], max: usize) -> usize {
    let mut i = path.len();
    if i > 1 && path[i - 1] == b'/' {
        i -= 1;
    }
    i = i.min(max);
    loop {
        i -= 1;
        if i == 0 || path[i] == b'/' {
            return i;
        }
    }
}

/// The tar git writes: a pax global header naming the commit, ustar entries
/// (pax headers for what ustar cannot hold), padded to 10 KiB blocks.
fn tar(members: &[Member], time: i64, umask: u32, commit: Option<Oid>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut time = time.max(0) as u64;
    let mut global = Vec::new();
    if let Some(id) = commit {
        pax_record(&mut global, "comment", id.to_string().as_bytes());
    }
    if time > USTAR_MAX {
        pax_record(&mut global, "mtime", time.to_string().as_bytes());
        time = USTAR_MAX;
    }
    if !global.is_empty() {
        let h = ustar(
            b"pax_global_header",
            b"",
            b"",
            b'g',
            0o100666,
            global.len() as u64,
            time,
        );
        blocked(&mut out, &h);
        blocked(&mut out, &global);
    }
    for m in members {
        let hex = m.oid.to_string();
        let (typeflag, mode) = match m.mode & 0o170000 {
            0o040000 | 0o160000 => (b'5', (m.mode | 0o777) & !umask),
            0o120000 => (b'2', m.mode | 0o777),
            _ => (
                b'0',
                (m.mode | if m.mode & 0o100 != 0 { 0o777 } else { 0o666 }) & !umask,
            ),
        };
        let path = m.path.as_bytes();
        let mut ext = Vec::new();
        let (mut name, mut prefix) = (path.to_vec(), Vec::new());
        if path.len() > 100 {
            let plen = path_prefix(path, 155);
            let rest = path.len().saturating_sub(plen + 1);
            if plen > 0 && rest <= 100 {
                prefix = path[..plen].to_vec();
                name = path[plen + 1..].to_vec();
            } else {
                name = format!("{hex}.data").into_bytes();
                pax_record(&mut ext, "path", path);
            }
        }
        let mut link = Vec::new();
        if typeflag == b'2' {
            if m.data.len() > 100 {
                link = format!("see {hex}.paxheader").into_bytes();
                pax_record(&mut ext, "linkpath", &m.data);
            } else {
                link = m.data.clone();
            }
        }
        let mut size = m.data.len() as u64;
        if typeflag == b'0' && size > USTAR_MAX {
            pax_record(&mut ext, "size", size.to_string().as_bytes());
            size = 0;
        }
        if !ext.is_empty() {
            let name = format!("{hex}.paxheader");
            let h = ustar(
                name.as_bytes(),
                b"",
                b"",
                b'x',
                0o100666,
                ext.len() as u64,
                time,
            );
            blocked(&mut out, &h);
            blocked(&mut out, &ext);
        }
        blocked(
            &mut out,
            &ustar(&name, &prefix, &link, typeflag, mode, size, time),
        );
        if typeflag == b'0' && !m.data.is_empty() {
            blocked(&mut out, &m.data);
        }
    }
    let tail = BLOCK - out.len() % BLOCK;
    out.resize(out.len() + tail, 0);
    if tail < 2 * RECORD {
        out.resize(out.len() + BLOCK, 0);
    }
    out
}

/// Raw deflate of `data` at zlib `level` (-1 is zlib's default).
fn deflate(data: &[u8], level: i32) -> Result<Vec<u8>, GitError> {
    use std::io::Write;
    let level = flate2::Compression::new(if level < 0 { 6 } else { level as u32 });
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), level);
    z.write_all(data)?;
    Ok(z.finish()?)
}

/// git's built-in gzip: zlib's gzip header with mtime 0 and OS 3 (Unix).
fn gzip(tar: &[u8], level: i32) -> Result<Vec<u8>, GitError> {
    let zlevel = if level < 0 { 6 } else { level };
    let xfl = match zlevel {
        9 => 2,
        0 | 1 => 4,
        _ => 0,
    };
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, xfl, 3];
    out.extend(deflate(tar, level)?);
    let mut crc = flate2::Crc::new();
    crc.update(tar);
    out.extend(crc.sum().to_le_bytes());
    out.extend((tar.len() as u32).to_le_bytes());
    Ok(out)
}

/// A tar piped through a `tar.<format>.command` filter, as git runs it.
fn filter(tar: &[u8], cmd: &str, level: Option<u32>) -> Result<Vec<u8>, GitError> {
    use std::io::Write;
    let cmd = match level {
        Some(l) => format!("{cmd} -{l}"),
        None => cmd.to_owned(),
    };
    let mut child = std::process::Command::new("sh")
        .args(["-c", &cmd])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| GitError::Other(format!("unable to start '{cmd}' filter: {e}")))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let data = tar.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&data));
    let out = child.wait_with_output()?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(GitError::Other(format!("'{cmd}' filter reported error")));
    }
    Ok(out.stdout)
}

/// The MS-DOS (time, date) zip stores for `secs`, in local time as git's.
fn dos_time(secs: i64) -> (u16, u16) {
    // SAFETY: localtime_r only writes the tm it is given.
    let tm = unsafe {
        let t = secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    let date = tm.tm_mday + (tm.tm_mon + 1) * 32 + (tm.tm_year + 1900 - 1980) * 512;
    let time = tm.tm_sec / 2 + tm.tm_min * 32 + tm.tm_hour * 2048;
    (time as u16, date as u16)
}

/// The zip git writes: stored or deflated entries with an extended mtime,
/// and the commit id as the archive comment.
fn zip(
    members: &[Member],
    time: i64,
    level: i32,
    commit: Option<Oid>,
    attrs: &Attributes,
) -> Vec<u8> {
    let (dtime, ddate) = dos_time(time);
    let mut out = Vec::new();
    let mut dir = Vec::new();
    let le16 = |v: &mut Vec<u8>, n: u32| v.extend((n as u16).to_le_bytes());
    let le32 = |v: &mut Vec<u8>, n: u32| v.extend(n.to_le_bytes());
    for m in members {
        let offset = out.len() as u32;
        let path = m.path.as_bytes();
        let flags = if m.path.is_ascii() { 0 } else { 1 << 11 };
        let kind = m.mode & 0o170000;
        let (mut method, attr2, creator, binary) = match kind {
            0o040000 | 0o160000 => (0, 16, 0, None),
            _ => {
                let link = kind == 0o120000;
                let exec = m.mode & 0o111 != 0;
                let attr2 = if link {
                    (m.mode | 0o777) << 16
                } else if exec {
                    m.mode << 16
                } else {
                    0
                };
                let binary = match attrs.state(&m.tree_path, "diff") {
                    Some(false) => true,
                    _ => m.data[..m.data.len().min(8000)].contains(&0),
                };
                let method = if !link && level != 0 && !m.data.is_empty() {
                    8
                } else {
                    0
                };
                (
                    method,
                    attr2,
                    if link || exec { 0x0317 } else { 0 },
                    Some(binary),
                )
            }
        };
        let mut crc = flate2::Crc::new();
        if binary.is_some() {
            crc.update(&m.data);
        }
        let mut body = if method == 8 {
            deflate(&m.data, level).unwrap_or_default()
        } else {
            m.data.clone()
        };
        if method == 8 && (body.is_empty() || body.len() >= m.data.len()) {
            method = 0;
            body = m.data.clone();
        }
        if binary.is_none() {
            body.clear();
        }
        let size = if binary.is_some() { m.data.len() } else { 0 } as u32;
        let mut extra = vec![0x55, 0x54, 5, 0, 1];
        extra.extend((time as u32).to_le_bytes());
        le32(&mut out, 0x04034b50);
        le16(&mut out, 10);
        le16(&mut out, flags);
        le16(&mut out, method);
        le16(&mut out, u32::from(dtime));
        le16(&mut out, u32::from(ddate));
        le32(&mut out, crc.sum());
        le32(&mut out, body.len() as u32);
        le32(&mut out, size);
        le16(&mut out, path.len() as u32);
        le16(&mut out, extra.len() as u32);
        out.extend(path);
        out.extend(&extra);
        out.extend(&body);
        le32(&mut dir, 0x02014b50);
        le16(&mut dir, creator);
        le16(&mut dir, 10);
        le16(&mut dir, flags);
        le16(&mut dir, method);
        le16(&mut dir, u32::from(dtime));
        le16(&mut dir, u32::from(ddate));
        le32(&mut dir, crc.sum());
        le32(&mut dir, body.len() as u32);
        le32(&mut dir, size);
        le16(&mut dir, path.len() as u32);
        le16(&mut dir, extra.len() as u32);
        le16(&mut dir, 0);
        le16(&mut dir, 0);
        le16(&mut dir, u32::from(binary == Some(false)));
        le32(&mut dir, attr2);
        le32(&mut dir, offset);
        dir.extend(path);
        dir.extend(&extra);
    }
    let start = out.len() as u32;
    out.extend(&dir);
    le32(&mut out, 0x06054b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, members.len() as u32);
    le16(&mut out, members.len() as u32);
    le32(&mut out, dir.len() as u32);
    le32(&mut out, start);
    le16(&mut out, if commit.is_some() { 40 } else { 0 });
    if let Some(id) = commit {
        out.extend(id.to_string().as_bytes());
    }
    out
}

/// Whether git reads `url` as a local path rather than a transport URL: no
/// `scheme://`, and no `host:` before the first `/`.
pub fn is_local_url(url: &str) -> bool {
    if url.starts_with("file://") {
        return true;
    }
    if url.contains("://") {
        return false;
    }
    match (url.find(':'), url.find('/')) {
        (None, _) => true,
        (Some(colon), Some(slash)) => slash < colon,
        (Some(_), None) => false,
    }
}

/// `s` in single quotes for a POSIX shell, as git's sq_quote_buf writes it.
fn sq_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' | '!' => {
                out.push_str("'\\");
                out.push(c);
                out.push('\'');
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Write one pkt-line.
fn pkt(w: &mut impl std::io::Write, data: &[u8]) -> std::io::Result<()> {
    w.write_all(format!("{:04x}", data.len() + 4).as_bytes())?;
    w.write_all(data)
}

/// Read one pkt-line; None is a flush (or the end of the stream).
fn read_pkt(r: &mut impl std::io::Read) -> Result<Option<Vec<u8>>, GitError> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = std::str::from_utf8(&len)
        .ok()
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .ok_or_else(|| GitError::Other("protocol error: bad line length character".into()))?;
    if len < 4 {
        return Ok(None);
    }
    let mut data = vec![0u8; len - 4];
    r.read_exact(&mut data)?;
    Ok(Some(data))
}

/// `git archive --remote` over git:// or ssh: send the arguments to the
/// remote's upload-archive and return the archive it streams back on
/// sideband 1, showing its sideband 2 messages on stderr as git does.
/// `ssh` is core.sshCommand; GIT_SSH_COMMAND and GIT_SSH win over it.
pub fn remote_archive(
    url: &str,
    exec: &str,
    args: &[String],
    ssh: Option<&str>,
) -> Result<Vec<u8>, GitError> {
    use std::io::{Read, Write};
    // [user@]host[:port] and the path, with `/~user` read as `~user`.
    let (scheme, authority, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            let (a, p) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            (scheme, a, p)
        }
        None => {
            let (a, p) = url.split_once(':').unwrap_or((url, ""));
            ("ssh", a, p)
        }
    };
    let path = path
        .strip_prefix('/')
        .filter(|p| p.starts_with('~'))
        .unwrap_or(path);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if url.contains("://") && !p.contains(']') => (h, Some(p)),
        _ => (authority, None),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let mut child = None;
    let (mut reader, mut writer): (Box<dyn Read>, Box<dyn Write>) = match scheme {
        "git" => {
            let port: u16 = port.and_then(|p| p.parse().ok()).unwrap_or(9418);
            let stream = std::net::TcpStream::connect((host, port))?;
            let mut w = stream.try_clone()?;
            pkt(
                &mut w,
                format!("{exec} {path}\0host={authority}\0").as_bytes(),
            )?;
            (Box::new(stream), Box::new(w))
        }
        "ssh" | "git+ssh" | "ssh+git" => {
            if host.starts_with('-') {
                return Err(GitError::Other(format!(
                    "strange hostname '{host}' blocked"
                )));
            }
            let command = format!("{exec} {}", sq_quote(path));
            let mut ssh_args: Vec<String> = Vec::new();
            if let Some(p) = port {
                ssh_args.extend(["-p".to_owned(), p.to_owned()]);
            }
            ssh_args.extend([host.to_owned(), command]);
            let git_ssh = std::env::var("GIT_SSH").ok();
            let shell = std::env::var("GIT_SSH_COMMAND")
                .ok()
                .or_else(|| ssh.filter(|_| git_ssh.is_none()).map(str::to_owned));
            let mut cmd = match (shell, git_ssh) {
                (Some(sh), _) => {
                    let mut c = std::process::Command::new("sh");
                    c.args(["-c", &format!("{sh} \"$@\""), &sh]);
                    c
                }
                (None, Some(prog)) => std::process::Command::new(prog),
                (None, None) => std::process::Command::new("ssh"),
            };
            let mut c = cmd
                .args(&ssh_args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            let r = c.stdout.take().expect("piped stdout");
            let w = c.stdin.take().expect("piped stdin");
            child = Some(c);
            (Box::new(r), Box::new(w))
        }
        _ => {
            return Err(GitError::Other(
                "operation not supported by protocol".into(),
            ));
        }
    };
    for a in args {
        pkt(&mut writer, format!("argument {a}\n").as_bytes())?;
    }
    writer.write_all(b"0000")?;
    writer.flush()?;
    let chomp = |l: Vec<u8>| {
        let s = String::from_utf8_lossy(&l).into_owned();
        s.strip_suffix('\n').map(str::to_owned).unwrap_or(s)
    };
    let remote_err = |l: &str| {
        l.strip_prefix("ERR ")
            .map(|m| GitError::Other(format!("remote error: {m}")))
    };
    let Some(ack) = read_pkt(&mut reader)?.map(chomp) else {
        return Err(GitError::Other(
            "git archive: expected ACK/NAK, got a flush packet".into(),
        ));
    };
    if let Some(e) = remote_err(&ack) {
        return Err(e);
    }
    if ack != "ACK" {
        return Err(GitError::Other(match ack.strip_prefix("NACK ") {
            Some(why) => format!("git archive: NACK {why}"),
            None => "git archive: protocol error".to_owned(),
        }));
    }
    if let Some(l) = read_pkt(&mut reader)? {
        return Err(remote_err(&chomp(l))
            .unwrap_or_else(|| GitError::Other("git archive: expected a flush".into())));
    }
    let mut out = Vec::new();
    let mut progress = Vec::new();
    let stderr_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let suffix: &[u8] = if stderr_tty { b"\x1b[K" } else { b"        " };
    let mut failure = None;
    while let Some(p) = read_pkt(&mut reader)? {
        let Some((&band, data)) = p.split_first() else {
            continue;
        };
        match band {
            1 => out.extend(data),
            2 => {
                let mut err = std::io::stderr().lock();
                for &b in data {
                    if b == b'\n' || b == b'\r' {
                        let mut line = b"remote: ".to_vec();
                        if !progress.is_empty() {
                            line.extend(&progress);
                            line.extend(suffix);
                        }
                        line.push(b);
                        let _ = err.write_all(&line);
                        progress.clear();
                    } else {
                        progress.push(b);
                    }
                }
            }
            3 => {
                failure = Some(format!(
                    "remote: {}",
                    String::from_utf8_lossy(data).trim_end()
                ));
                break;
            }
            b => {
                failure = Some(format!("archive: protocol error: bad band #{b}"));
                break;
            }
        }
    }
    if !progress.is_empty() {
        let mut line = b"remote: ".to_vec();
        line.extend(&progress);
        let _ = std::io::stderr().write_all(&line);
    }
    drop(writer);
    let status = child.map(|mut c| c.wait()).transpose()?;
    if let Some(msg) = failure {
        return Err(GitError::Other(msg));
    }
    if status.is_some_and(|s| !s.success()) {
        return Err(GitError::Other(
            "the remote end hung up unexpectedly".into(),
        ));
    }
    Ok(out)
}

/// Read an untracked file for `--add-file`: its name, git mode and content.
pub fn add_file(path: &Path) -> Result<(String, i32, Vec<u8>), GitError> {
    let data = std::fs::read(path)?;
    #[cfg(unix)]
    let exec = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path)?.permissions())
        & 0o111
        != 0;
    #[cfg(not(unix))]
    let exec = false;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| GitError::Other(format!("not a file: {}", path.display())))?;
    Ok((name, if exec { 0o100755 } else { 0o100644 }, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_follow_gitattributes() {
        let re = |p: &str| regex::Regex::new(&glob(p)).unwrap();
        assert!(re("*.txt").is_match("a.txt"));
        assert!(!re("*.txt").is_match("d/a.txt"));
        assert!(re("docs/**").is_match("docs/a/b.md"));
        assert!(re("**/tmp").is_match("x/y/tmp"));
        assert!(re("[!a]b").is_match("cb"));
    }
}
