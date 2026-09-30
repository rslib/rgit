//! `git archive` natively: the tar, tgz and zip bytes git writes, its export
//! attributes (`export-ignore`, `export-subst`), and the client side of the
//! upload-archive protocol for `--remote`.

use crate::rev::RevParse;
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
    /// Each attribute: set (`attr`), unset (`-attr`) or a value (`attr=v`).
    attrs: Vec<(String, Result<bool, String>)>,
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
                let mut attrs: Vec<(String, Result<bool, String>)> = Vec::new();
                for w in words.filter(|w| !w.starts_with('!')) {
                    match w.strip_prefix('-') {
                        Some(name) => attrs.push((name.to_owned(), Ok(false))),
                        // The built-in macro: binary is -diff -merge -text.
                        None if w == "binary" => attrs.extend(
                            [("binary", true), ("diff", false), ("merge", false)]
                                .into_iter()
                                .chain([("text", false)])
                                .map(|(n, on)| (n.to_owned(), Ok(on))),
                        ),
                        None => attrs.push(match w.split_once('=') {
                            Some((n, v)) => (n.to_owned(), Err(v.to_owned())),
                            None => (w.to_owned(), Ok(true)),
                        }),
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

    /// `attr` for `path`: set, unset (`-attr`) or unspecified (or a value).
    fn state(&self, path: &str, attr: &str) -> Option<bool> {
        self.value(path, attr).and_then(Result::ok)
    }

    /// `attr` for `path`: Ok(set or unset), Err(its value), or unspecified.
    fn value(&self, path: &str, attr: &str) -> Option<Result<bool, String>> {
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
                set = Some(on.clone());
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

/// convert.c's crlf actions, once the attributes and config resolve them.
#[derive(Clone, Copy, PartialEq)]
enum CrlfAction {
    Binary,
    Text,
    TextInput,
    TextCrlf,
    Auto,
    AutoInput,
    AutoCrlf,
}

/// convert_to_working_tree for an archived file at tree path `path`: ident,
/// then LF to CRLF, then the smudge filter, as its `ident`, `text`/`crlf`,
/// `eol` and `filter` attributes and core.autocrlf / core.eol say.
// ponytail: filter.<driver>.process and working-tree-encoding are not run.
fn to_worktree(
    repo: &Repository,
    attrs: &Attributes,
    path: &str,
    mut data: Vec<u8>,
) -> Result<Vec<u8>, GitError> {
    let config = repo.config()?.snapshot()?;
    if attrs.state(path, "ident") == Some(true) {
        data = ident(&data);
    }
    let autocrlf = match config.get_string("core.autocrlf").as_deref() {
        Ok("input") => Some(false),
        _ => config
            .get_bool("core.autocrlf")
            .unwrap_or(false)
            .then_some(true),
    };
    let eol_is_crlf = match autocrlf {
        Some(crlf) => crlf,
        None => match config.get_string("core.eol").as_deref() {
            Ok("crlf") => true,
            Ok("lf") => false,
            _ => cfg!(windows),
        },
    };
    let crlf_attr = |name: &str| match attrs.value(path, name) {
        Some(Ok(true)) => Some(CrlfAction::Text),
        Some(Ok(false)) => Some(CrlfAction::Binary),
        Some(Err(v)) if v == "input" => Some(CrlfAction::TextInput),
        Some(Err(v)) if v == "auto" => Some(CrlfAction::Auto),
        _ => None,
    };
    let mut action = crlf_attr("text").or_else(|| crlf_attr("crlf"));
    if action != Some(CrlfAction::Binary) {
        let eol = attrs.value(path, "eol").and_then(Result::err);
        action = match (action, eol.as_deref()) {
            (Some(CrlfAction::Auto), Some("lf")) => Some(CrlfAction::AutoInput),
            (Some(CrlfAction::Auto), Some("crlf")) => Some(CrlfAction::AutoCrlf),
            (_, Some("lf")) => Some(CrlfAction::TextInput),
            (_, Some("crlf")) => Some(CrlfAction::TextCrlf),
            (a, _) => a,
        };
    }
    let action = match action {
        Some(CrlfAction::Text) if eol_is_crlf => CrlfAction::TextCrlf,
        Some(CrlfAction::Text) => CrlfAction::TextInput,
        Some(a) => a,
        None => match autocrlf {
            None => CrlfAction::Binary,
            Some(true) => CrlfAction::AutoCrlf,
            Some(false) => CrlfAction::AutoInput,
        },
    };
    let out_crlf = match action {
        CrlfAction::Binary | CrlfAction::TextInput | CrlfAction::AutoInput => false,
        CrlfAction::TextCrlf | CrlfAction::AutoCrlf => true,
        CrlfAction::Text | CrlfAction::Auto => eol_is_crlf,
    };
    if out_crlf && !data.is_empty() {
        let (mut crlf, mut lone_cr, mut lone_lf, mut nul) = (0, 0, 0, 0);
        let (mut printable, mut nonprintable) = (0usize, 0usize);
        let mut i = 0;
        while i < data.len() {
            match data[i] {
                b'\r' if data.get(i + 1) == Some(&b'\n') => {
                    crlf += 1;
                    i += 1;
                }
                b'\r' => lone_cr += 1,
                b'\n' => lone_lf += 1,
                127 => nonprintable += 1,
                8 | 9 | 0o33 | 0o14 => printable += 1,
                0 => {
                    nul += 1;
                    nonprintable += 1;
                }
                c if c < 32 => nonprintable += 1,
                _ => printable += 1,
            }
            i += 1;
        }
        if data.last() == Some(&0o32) {
            nonprintable = nonprintable.saturating_sub(1);
        }
        let auto = matches!(
            action,
            CrlfAction::Auto | CrlfAction::AutoInput | CrlfAction::AutoCrlf
        );
        let binary = lone_cr > 0 || nul > 0 || (printable >> 7) < nonprintable;
        if lone_lf > 0 && !(auto && (lone_cr > 0 || crlf > 0 || binary)) {
            let mut out = Vec::with_capacity(data.len() + lone_lf);
            for (i, &b) in data.iter().enumerate() {
                if b == b'\n' && (i == 0 || data[i - 1] != b'\r') {
                    out.push(b'\r');
                }
                out.push(b);
            }
            data = out;
        }
    }
    let Some(Err(driver)) = attrs.value(path, "filter") else {
        return Ok(data);
    };
    let key = |k: &str| config.get_string(&format!("filter.{driver}.{k}")).ok();
    let required = config
        .get_bool(&format!("filter.{driver}.required"))
        .unwrap_or(false);
    let Some(cmd) = key("smudge").filter(|c| !c.is_empty()) else {
        if required && key("process").is_none() {
            return Err(GitError::Other(format!(
                "{path}: smudge filter {driver} failed"
            )));
        }
        return Ok(data);
    };
    let cmd = cmd.replace("%f", &crate::smart::sq_quote(path));
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(repo.workdir().unwrap_or(repo.path()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take();
    let input = data.clone();
    let writer = std::thread::spawn(move || {
        if let Some(s) = stdin.as_mut() {
            let _ = std::io::Write::write_all(s, &input);
        }
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    if out.status.success() {
        return Ok(out.stdout);
    }
    eprintln!(
        "error: external filter '{cmd}' failed {}",
        out.status.code().unwrap_or(-1)
    );
    eprintln!("error: external filter '{cmd}' failed");
    if required {
        return Err(GitError::Other(format!(
            "{path}: smudge filter {driver} failed"
        )));
    }
    Ok(data)
}

/// ident_to_worktree: `$Id$` (or an expanded `$Id: ... $` without spaces)
/// becomes `$Id: <blob id> $`.
fn ident(src: &[u8]) -> Vec<u8> {
    let id = Oid::hash_object(git2::ObjectType::Blob, src)
        .map_or_else(|_| String::new(), |o| o.to_string());
    let mut out = Vec::with_capacity(src.len());
    let mut s = src;
    let mut changed = false;
    while let Some(d) = s.iter().position(|&b| b == b'$') {
        out.extend_from_slice(&s[..=d]);
        s = &s[d + 1..];
        if s.len() < 3 || &s[..2] != b"Id" {
            continue;
        }
        if s[2] == b'$' {
            s = &s[3..];
        } else if s[2] == b':' {
            let Some(end) = s[3..].iter().position(|&b| b == b'$').map(|p| p + 3) else {
                break;
            };
            if s[3..end].contains(&b'\n') {
                continue;
            }
            if end >= 4
                && let Some(sp) = s[4..end].iter().position(|&b| b == b' ').map(|p| p + 4)
                && sp < end - 1
            {
                continue;
            }
            s = &s[end + 1..];
        } else {
            continue;
        }
        out.extend_from_slice(format!("Id: {id} $").as_bytes());
        changed = true;
    }
    out.extend_from_slice(s);
    if changed { out } else { src.to_vec() }
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
    /// A blob past core.bigFileThreshold, which git streams.
    stream: bool,
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
    let obj = repo.rev_single(&o.rev)?;
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
            stream: false,
        });
    }
    // Folders are written only once a file inside them is.
    let mut pending: Vec<(String, Oid)> = Vec::new();
    let mut failed = None;
    let mut convert_failed = None;
    let big_file = config
        .as_ref()
        .and_then(|c| c.get_i64("core.bigFileThreshold").ok())
        .map_or(512 << 20, |v| v as u64);
    let mut add = |tree_path: String, mode: u32, oid: Oid, data: Vec<u8>, stream: bool| {
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
            stream,
        });
    };
    let walked = tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
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
            add(dir, 0o040000, id, Vec::new(), false);
        }
        if attrs.is_set(&path, "export-ignore") {
            return git2::TreeWalkResult::Ok;
        }
        if mode == 0o160000 {
            add(format!("{path}/"), mode, e.id(), Vec::new(), false);
            return git2::TreeWalkResult::Ok;
        }
        let mut data = match repo.find_blob(e.id()) {
            Ok(b) => b.content().to_vec(),
            Err(err) => {
                failed = Some(err);
                return git2::TreeWalkResult::Abort;
            }
        };
        // Regular files are converted as a checkout would, except big ones
        // git streams unconverted when nothing is substituted in them.
        let subst = commit
            .as_ref()
            .filter(|_| mode != 0o120000 && attrs.is_set(&path, "export-subst"));
        let stream = mode & 0o170000 == 0o100000 && subst.is_none() && data.len() as u64 > big_file;
        if mode & 0o170000 == 0o100000 && !stream {
            data = match to_worktree(repo, &attrs, &path, data) {
                Ok(d) => d,
                Err(err) => {
                    convert_failed = Some(err);
                    return git2::TreeWalkResult::Abort;
                }
            };
        }
        if let Some(c) = subst {
            data = export_subst(repo, c, &data);
        }
        add(path, mode, e.id(), data, stream);
        git2::TreeWalkResult::Ok
    });
    if let Some(err) = convert_failed {
        return Err(err);
    }
    if let Some(err) = failed {
        return Err(err.into());
    }
    walked?;
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
            stream: false,
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
                // mode_t is u32 on linux and u16 on apple; `as` keeps both lint-clean.
                m as u32
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

/// The zip git writes (archive-zip.c): stored or deflated entries with an
/// extended mtime, the commit id as the archive comment, zip64 records past
/// 4 GiB or 65535 entries, and big blobs (`stream`) written as git streams
/// them, sizes and crc in a data descriptor after the data.
fn zip(
    members: &[Member],
    time: i64,
    level: i32,
    commit: Option<Oid>,
    attrs: &Attributes,
) -> Vec<u8> {
    const MAX32: u64 = 0xffff_ffff;
    let (dtime, ddate) = dos_time(time);
    let mut out = Vec::new();
    let mut dir = Vec::new();
    let le = |v: &mut Vec<u8>, n: u64, width: usize| v.extend(&n.to_le_bytes()[..width]);
    let clamp32 = |n: u64| n.min(MAX32);
    let mut max_creator = 0;
    for m in members {
        let offset = out.len() as u64;
        let path = m.path.as_bytes();
        let mut flags = if m.path.is_ascii() { 0 } else { 1 << 11 };
        if m.stream {
            flags |= 1 << 3;
        }
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
                // A streamed blob is judged by its first 16 KiB read.
                let head = &m.data[..m.data.len().min(if m.stream { 16384 } else { usize::MAX })];
                let binary = match attrs.state(&m.tree_path, "diff") {
                    Some(false) => true,
                    _ => head[..head.len().min(8000)].contains(&0),
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
        max_creator = max_creator.max(creator);
        let mut crc = flate2::Crc::new();
        if binary.is_some() {
            crc.update(&m.data);
        }
        let mut body = if method == 8 {
            deflate(&m.data, level).unwrap_or_default()
        } else {
            m.data.clone()
        };
        if method == 8 && !m.stream && (body.is_empty() || body.len() >= m.data.len()) {
            method = 0;
            body = m.data.clone();
        }
        if binary.is_none() {
            body.clear();
        }
        let size = if binary.is_some() { m.data.len() } else { 0 } as u64;
        let compressed = body.len() as u64;
        // What the local header knows before a streamed blob is read.
        let (head_crc, head_compressed) = match (m.stream, method) {
            (true, 8) => (0, 0),
            (true, _) => (0, size),
            _ => (crc.sum(), compressed),
        };
        let zip64 = size > MAX32 || head_compressed > MAX32 || (m.stream && size > 0x7fff_ffff);
        let version = if zip64 { 45 } else { 10 };
        let mut extra = vec![0x55, 0x54, 5, 0, 1];
        extra.extend((time as u32).to_le_bytes());
        le(&mut out, 0x04034b50, 4);
        le(&mut out, version, 2);
        le(&mut out, flags, 2);
        le(&mut out, method, 2);
        le(&mut out, u64::from(dtime), 2);
        le(&mut out, u64::from(ddate), 2);
        le(&mut out, u64::from(head_crc), 4);
        if zip64 {
            le(&mut out, MAX32, 4);
            le(&mut out, MAX32, 4);
        } else {
            le(&mut out, head_compressed, 4);
            le(&mut out, size, 4);
        }
        le(&mut out, path.len() as u64, 2);
        le(&mut out, extra.len() as u64 + if zip64 { 20 } else { 0 }, 2);
        out.extend(path);
        out.extend(&extra);
        if zip64 {
            le(&mut out, 1, 2);
            le(&mut out, 16, 2);
            le(&mut out, size, 8);
            le(&mut out, head_compressed, 8);
        }
        out.extend(&body);
        if m.stream {
            le(&mut out, 0x08074b50, 4);
            le(&mut out, u64::from(crc.sum()), 4);
            let width = if size >= MAX32 || compressed >= MAX32 {
                8
            } else {
                4
            };
            le(&mut out, compressed, width);
            le(&mut out, size, width);
        }
        let mut dir_extra = Vec::new();
        if compressed > MAX32 || size > MAX32 || offset > MAX32 {
            for (n, _) in [(size, 0), (compressed, 1), (offset, 2)] {
                if n >= MAX32 {
                    le(&mut dir_extra, n, 8);
                }
            }
            let payload = dir_extra.len() as u64;
            let mut head = Vec::new();
            le(&mut head, 1, 2);
            le(&mut head, payload, 2);
            dir_extra.splice(0..0, head);
        }
        le(&mut dir, 0x02014b50, 4);
        le(&mut dir, creator, 2);
        le(&mut dir, version, 2);
        le(&mut dir, flags, 2);
        le(&mut dir, method, 2);
        le(&mut dir, u64::from(dtime), 2);
        le(&mut dir, u64::from(ddate), 2);
        le(&mut dir, u64::from(crc.sum()), 4);
        le(&mut dir, clamp32(compressed), 4);
        le(&mut dir, clamp32(size), 4);
        le(&mut dir, path.len() as u64, 2);
        le(&mut dir, (extra.len() + dir_extra.len()) as u64, 2);
        le(&mut dir, 0, 2);
        le(&mut dir, 0, 2);
        le(&mut dir, u64::from(binary == Some(false)), 2);
        le(&mut dir, u64::from(attr2), 4);
        le(&mut dir, clamp32(offset), 4);
        dir.extend(path);
        dir.extend(&extra);
        dir.extend(&dir_extra);
    }
    let start = out.len() as u64;
    let entries = members.len() as u64;
    out.extend(&dir);
    if entries > 0xffff || start > MAX32 {
        le(&mut out, 0x06064b50, 4);
        le(&mut out, 44, 8);
        le(&mut out, max_creator, 2);
        le(&mut out, 45, 2);
        le(&mut out, 0, 4);
        le(&mut out, 0, 4);
        le(&mut out, entries, 8);
        le(&mut out, entries, 8);
        le(&mut out, dir.len() as u64, 8);
        le(&mut out, start, 8);
        le(&mut out, 0x07064b50, 4);
        le(&mut out, 0, 4);
        le(&mut out, start + dir.len() as u64, 8);
        le(&mut out, 1, 4);
    }
    le(&mut out, 0x06054b50, 4);
    le(&mut out, 0, 2);
    le(&mut out, 0, 2);
    le(&mut out, entries.min(0xffff), 2);
    le(&mut out, entries.min(0xffff), 2);
    le(&mut out, dir.len() as u64, 4);
    le(&mut out, clamp32(start), 4);
    le(&mut out, if commit.is_some() { 40 } else { 0 }, 2);
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
    use crate::smart::{pkt, read_pkt};
    use std::io::Write;
    let mut stream = crate::smart::connect(url, exec, ssh, false)?;
    let (reader, writer) = (&mut stream.reader, &mut stream.writer);
    for a in args {
        pkt(writer, format!("argument {a}\n").as_bytes())?;
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
    let Some(ack) = read_pkt(reader)?.map(chomp) else {
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
    if let Some(l) = read_pkt(reader)? {
        return Err(remote_err(&chomp(l))
            .unwrap_or_else(|| GitError::Other("git archive: expected a flush".into())));
    }
    let mut out = Vec::new();
    let mut progress = Vec::new();
    let stderr_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let suffix: &[u8] = if stderr_tty { b"\x1b[K" } else { b"        " };
    let mut failure = None;
    while let Some(p) = read_pkt(reader)? {
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
    let finished = stream.finish()?;
    if let Some(msg) = failure {
        return Err(GitError::Other(msg));
    }
    if !finished {
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
