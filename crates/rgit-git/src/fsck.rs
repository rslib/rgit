//! `git fsck` as builtin/fsck.c and fsck.c do it: check every loose and
//! packed object, walk what the refs, HEADs, reflogs and index reach, and
//! report dangling, unreachable, missing and broken objects in git's words
//! and in git's order (its object hash table, simulated).

use crate::rev::RevParse;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use git2::{ObjectType, Oid, Repository};

use crate::GitError;
use crate::maintenance;

/// What `git fsck` checks and reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsckOptions {
    /// Check packed objects too (default on; --no-full).
    pub full: bool,
    /// Warnings are errors, and 100664 modes are bad (--strict).
    pub strict: bool,
    /// Report every unreachable object (--unreachable).
    pub unreachable: bool,
    /// Report dangling objects (default on; --no-dangling).
    pub dangling: bool,
    /// Only check that reachable objects exist (--connectivity-only).
    pub connectivity_only: bool,
    /// Write dangling objects to lost-found (--lost-found).
    pub lost_found: bool,
    /// Name objects by how they are reached (--name-objects).
    pub name_objects: bool,
    /// Report root commits (--root).
    pub root: bool,
    /// Report tags (--tags).
    pub tags: bool,
    /// Treat the index as a root even with objects given (--cache).
    pub cache: bool,
    /// Treat reflog entries as roots (default on; --no-reflogs).
    pub reflogs: bool,
    /// Walk from these instead of the refs, HEADs and index.
    pub objects: Vec<String>,
}

impl Default for FsckOptions {
    fn default() -> Self {
        FsckOptions {
            full: true,
            strict: false,
            unreachable: false,
            dangling: true,
            connectivity_only: false,
            lost_found: false,
            name_objects: false,
            root: false,
            tags: false,
            cache: false,
            reflogs: true,
            objects: Vec::new(),
        }
    }
}

/// What `git fsck` printed and its exit code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FsckReport {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

const ERROR_OBJECT: i32 = 1;
const ERROR_REACHABLE: i32 = 2;
const ERROR_PACK: i32 = 4;
const ERROR_REFS: i32 = 8;
const ERROR_COMMIT_GRAPH: i32 = 16;
const ERROR_MULTI_PACK_INDEX: i32 = 32;

#[derive(Default)]
struct Obj {
    kind: Option<ObjectType>,
    has: bool,
    used: bool,
    reachable: bool,
}

/// Parsed links of an object.
enum Links {
    Commit {
        tree: Oid,
        parents: Vec<Oid>,
    },
    Tree(Vec<(u32, String, Oid)>),
    Tag {
        target: Oid,
        kind: ObjectType,
        name: Option<String>,
    },
    Blob,
}

/// git's object hash table: linear probing, doubled when half full, which
/// is the order fsck reports in.
#[derive(Default)]
struct Table {
    slots: Vec<Option<Oid>>,
    objs: HashMap<Oid, Obj>,
}

impl Table {
    fn slot(oid: &Oid, size: usize) -> usize {
        let b = oid.as_bytes();
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize & (size - 1)
    }

    fn place(slots: &mut [Option<Oid>], oid: Oid) {
        let size = slots.len();
        let mut i = Self::slot(&oid, size);
        while slots[i].is_some() {
            i = (i + 1) & (size - 1);
        }
        slots[i] = Some(oid);
    }

    fn touch(&mut self, oid: Oid) -> &mut Obj {
        self.find(oid, true)
    }

    /// The object, created if new; `lookup` as git's lookup_object, which
    /// moves it, rather than following a parsed pointer.
    fn find(&mut self, oid: Oid, lookup: bool) -> &mut Obj {
        if self.objs.contains_key(&oid) {
            if !lookup {
                return self.objs.get_mut(&oid).expect("present");
            }
            let size = self.slots.len();
            let first = Self::slot(&oid, size);
            let mut i = first;
            while self.slots[i] != Some(oid) {
                i = (i + 1) & (size - 1);
            }
            self.slots.swap(i, first);
        } else {
            if self.slots.len() as i64 - 1 <= self.objs.len() as i64 * 2 {
                let size = if self.slots.len() < 32 {
                    32
                } else {
                    self.slots.len() * 2
                };
                let old = std::mem::replace(&mut self.slots, vec![None; size]);
                for o in old.into_iter().flatten() {
                    Self::place(&mut self.slots, o);
                }
            }
            Self::place(&mut self.slots, oid);
            self.objs.insert(oid, Obj::default());
        }
        self.objs.get_mut(&oid).expect("present")
    }
}

fn type_name(kind: Option<ObjectType>) -> &'static str {
    match kind {
        Some(ObjectType::Commit) => "commit",
        Some(ObjectType::Tree) => "tree",
        Some(ObjectType::Blob) => "blob",
        Some(ObjectType::Tag) => "tag",
        _ => "unknown",
    }
}

fn parse_hex(s: &[u8]) -> Option<Oid> {
    let hex = std::str::from_utf8(s.get(..40)?).ok()?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Oid::from_str(hex).ok()
}

/// Parse an object as git's parse_object_buffer does; None when git cannot.
fn parse(kind: ObjectType, data: &[u8]) -> Option<Links> {
    match kind {
        ObjectType::Commit => {
            let rest = data.strip_prefix(b"tree ")?;
            let tree = parse_hex(rest)?;
            if rest.get(40) != Some(&b'\n') {
                return None;
            }
            let mut at = &rest[41..];
            let mut parents = Vec::new();
            while let Some(r) = at.strip_prefix(b"parent ") {
                if r.get(40) != Some(&b'\n') {
                    return None;
                }
                parents.push(parse_hex(r)?);
                at = &r[41..];
            }
            Some(Links::Commit { tree, parents })
        }
        ObjectType::Tree => {
            let mut entries = Vec::new();
            let mut at = 0;
            while at < data.len() {
                let sp = data[at..].iter().position(|b| *b == b' ')?;
                let mode_text = &data[at..at + sp];
                if mode_text.is_empty() || !mode_text.iter().all(|b| (b'0'..=b'7').contains(b)) {
                    return None;
                }
                let mode = u32::from_str_radix(std::str::from_utf8(mode_text).ok()?, 8).ok()?;
                let nul = data[at..].iter().position(|b| *b == 0)?;
                let name = String::from_utf8_lossy(&data[at + sp + 1..at + nul]).into_owned();
                if name.is_empty() {
                    return None;
                }
                let id = data.get(at + nul + 1..at + nul + 21)?;
                entries.push((mode, name, Oid::from_bytes(id).ok()?));
                at += nul + 21;
            }
            Some(Links::Tree(entries))
        }
        ObjectType::Tag => {
            let rest = data.strip_prefix(b"object ")?;
            let target = parse_hex(rest)?;
            if rest.get(40) != Some(&b'\n') {
                return None;
            }
            let rest = rest[41..].strip_prefix(b"type ")?;
            let nl = rest.iter().position(|b| *b == b'\n')?;
            let kind = match &rest[..nl] {
                b"commit" => ObjectType::Commit,
                b"tree" => ObjectType::Tree,
                b"blob" => ObjectType::Blob,
                b"tag" => ObjectType::Tag,
                _ => return None,
            };
            let name = rest[nl + 1..].strip_prefix(b"tag ").and_then(|r| {
                let end = r.iter().position(|b| *b == b'\n')?;
                Some(String::from_utf8_lossy(&r[..end]).into_owned())
            });
            Some(Links::Tag { target, kind, name })
        }
        _ => Some(Links::Blob),
    }
}

impl Links {
    /// Linked objects with the type the link expects, in git's walk order.
    fn children(&self) -> Vec<(Oid, ObjectType)> {
        match self {
            Links::Commit { tree, parents } => std::iter::once((*tree, ObjectType::Tree))
                .chain(parents.iter().map(|p| (*p, ObjectType::Commit)))
                .collect(),
            Links::Tree(entries) => entries
                .iter()
                .filter_map(|(mode, _, id)| match mode & 0o170000 {
                    0o040000 => Some((*id, ObjectType::Tree)),
                    0o100000 | 0o120000 => Some((*id, ObjectType::Blob)),
                    _ => None,
                })
                .collect(),
            Links::Tag { target, kind, .. } => vec![(*target, *kind)],
            Links::Blob => Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Error,
    Warn,
    Info,
    Ignore,
}

/// fsck.<msg-id> severities (keys lowercased) and fsck.skipList's objects.
fn severity_config(repo: &Repository) -> (HashMap<String, Level>, std::collections::HashSet<Oid>) {
    let mut levels = HashMap::new();
    let mut skip = std::collections::HashSet::new();
    let Ok(cfg) = crate::config::open_config(repo, crate::ConfigScope::Any, false) else {
        return (levels, skip);
    };
    if let Ok(mut entries) = cfg.entries(Some("fsck\\..*")) {
        while let Some(Ok(e)) = entries.next() {
            let (Ok(name), Ok(value)) = (e.name(), e.value()) else {
                continue;
            };
            let id = name["fsck.".len()..].to_ascii_lowercase();
            if id == "skiplist" {
                let path = match (value.strip_prefix("~/"), std::env::var_os("HOME")) {
                    (Some(rest), Some(home)) => Path::new(&home).join(rest),
                    _ => PathBuf::from(value),
                };
                let text = std::fs::read_to_string(path).unwrap_or_default();
                skip.extend(
                    text.lines().filter_map(|l| {
                        Oid::from_str(l.split('#').next().unwrap_or("").trim()).ok()
                    }),
                );
                continue;
            }
            let level = match value.to_ascii_lowercase().as_str() {
                "error" => Level::Error,
                "warn" => Level::Warn,
                "ignore" => Level::Ignore,
                _ => continue,
            };
            levels.insert(id, level);
        }
    }
    (levels, skip)
}

/// Whether a tree entry name is `.<base>` as HFS+ or NTFS would see it
/// (any case, trailing dots or spaces, the `<short>~1`..`~4` short names).
// ponytail: HFS+ ignorable code points and NTFS hashed short names are not
// matched.
fn is_dotfile(name: &str, base: &str, short: &str) -> bool {
    let n = name.trim_end_matches(['.', ' ']).to_ascii_lowercase();
    n.strip_prefix('.') == Some(base)
        || n.strip_prefix(short)
            .and_then(|r| r.strip_prefix('~'))
            .is_some_and(|d| matches!(d, "1" | "2" | "3" | "4"))
}

/// git's check_submodule_name: not empty, no `..` path component.
fn bad_submodule_name(name: &str) -> bool {
    name.is_empty() || name.split(['/', '\\']).any(|c| c == "..")
}

fn url_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// git's check_submodule_url: no option-like, newline-smuggling or
/// root-escaping urls, and curl urls must normalize.
fn bad_submodule_url(url: &str) -> bool {
    if url.starts_with('-') {
        return true;
    }
    let dot = |u: &str| u.starts_with("./") || u.starts_with(".\\");
    let dotdot = |u: &str| u.starts_with("../") || u.starts_with("..\\");
    if dot(url) || dotdot(url) || url.starts_with("git://") {
        if url_decode(url)
            .unwrap_or_else(|| url.to_owned())
            .contains('\n')
        {
            return true;
        }
        let mut rest = url;
        let mut ups = 0;
        loop {
            if dotdot(rest) {
                ups += 1;
                rest = &rest[3..];
            } else if dot(rest) {
                rest = &rest[2..];
            } else {
                break;
            }
        }
        return ups > 0 && (rest.starts_with(':') || rest.starts_with('/'));
    }
    let curl = ["http::", "https::", "ftp::", "ftps::"]
        .iter()
        .find_map(|p| url.strip_prefix(p))
        .or_else(|| {
            ["http://", "https://", "ftp://", "ftps://"]
                .iter()
                .any(|p| url.starts_with(p))
                .then_some(url)
        });
    let Some(curl) = curl else {
        return false;
    };
    let Some((_, after)) = curl.split_once("://") else {
        return true;
    };
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = host_port.split_once(':').unwrap_or((host_port, ""));
    if host.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    url_decode(curl).is_none_or(|d| d.contains('\n'))
}

struct Fsck<'r> {
    repo: &'r Repository,
    odb: git2::Odb<'r>,
    o: &'r FsckOptions,
    table: Table,
    names: HashMap<Oid, String>,
    links: HashMap<Oid, Links>,
    pending: Vec<Oid>,
    packed: std::collections::HashSet<Oid>,
    out: String,
    err: String,
    code: i32,
    objects_dir: String,
    levels: HashMap<String, Level>,
    skip: std::collections::HashSet<Oid>,
    gitmodules: Vec<Oid>,
    gitattributes: Vec<Oid>,
}

impl<'r> Fsck<'r> {
    fn describe(&self, oid: &Oid) -> String {
        match self.names.get(oid).filter(|_| self.o.name_objects) {
            Some(n) => format!("{oid} ({n})"),
            None => oid.to_string(),
        }
    }

    fn put_name(&mut self, oid: Oid, name: String) {
        if self.o.name_objects {
            self.names.entry(oid).or_insert(name);
        }
    }

    fn kind_of(&self, oid: &Oid) -> Option<ObjectType> {
        self.table
            .objs
            .get(oid)
            .and_then(|o| o.kind)
            .or_else(|| self.odb.read_header(*oid).ok().map(|(_, k)| k))
    }

    /// git's report(): `error in <type> <id>: <msgId>: <text>`; true for errors.
    fn report(&mut self, oid: Oid, kind: ObjectType, level: Level, id: &str, text: &str) -> bool {
        if self.skip.contains(&oid) {
            return false;
        }
        let level = match (self.levels.get(&id.to_ascii_lowercase()), level) {
            (Some(l), _) => *l,
            (None, Level::Warn) if self.o.strict => Level::Error,
            (None, l) => l,
        };
        if level == Level::Ignore {
            return false;
        }
        let word = if level == Level::Error {
            "error"
        } else {
            "warning"
        };
        self.err.push_str(&format!(
            "{word} in {} {}: {id}: {text}\n",
            type_name(Some(kind)),
            self.describe(&oid)
        ));
        level == Level::Error
    }

    fn check_ident(&mut self, oid: Oid, kind: ObjectType, line: &[u8]) -> bool {
        let bad = |s: &mut Self, id: &str, what: &str| {
            s.report(
                oid,
                kind,
                Level::Error,
                id,
                &format!("invalid author/committer line - {what}"),
            )
        };
        if line.first() == Some(&b'<') {
            return bad(self, "missingNameBeforeEmail", "missing space before email");
        }
        let p = line
            .iter()
            .position(|b| matches!(b, b'<' | b'>'))
            .unwrap_or(line.len());
        match line.get(p) {
            Some(b'>') => return bad(self, "badName", "bad name"),
            Some(b'<') => {}
            _ => return bad(self, "missingEmail", "missing email"),
        }
        if p == 0 || line[p - 1] != b' ' {
            return bad(
                self,
                "missingSpaceBeforeEmail",
                "missing space before email",
            );
        }
        let q = p
            + 1
            + line[p + 1..]
                .iter()
                .position(|b| matches!(b, b'<' | b'>'))
                .unwrap_or(line.len() - p - 1);
        if line.get(q) != Some(&b'>') {
            return bad(self, "badEmail", "bad email");
        }
        let rest = &line[q + 1..];
        let Some(rest) = rest.strip_prefix(b" ") else {
            return bad(self, "missingSpaceBeforeDate", "missing space before date");
        };
        if rest.first() == Some(&b'0') && rest.get(1) != Some(&b' ') {
            return bad(self, "zeroPaddedDate", "zero-padded date");
        }
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        let date = std::str::from_utf8(&rest[..digits]).unwrap_or("");
        if digits > 0 && date.parse::<u64>().is_err() {
            return bad(self, "badDateOverflow", "date causes integer overflow");
        }
        if digits == 0 || rest.get(digits) != Some(&b' ') {
            return bad(self, "badDate", "bad date");
        }
        let tz = &rest[digits + 1..];
        let good_tz =
            tz.len() == 5 && matches!(tz[0], b'+' | b'-') && tz[1..].iter().all(u8::is_ascii_digit);
        if !good_tz {
            return bad(self, "badTimezone", "bad time zone");
        }
        false
    }

    /// verify_headers: false when the header is terminated.
    fn check_headers(&mut self, oid: Oid, kind: ObjectType, data: &[u8]) -> bool {
        for (i, b) in data.iter().enumerate() {
            match b {
                0 => {
                    return self.report(
                        oid,
                        kind,
                        Level::Error,
                        "nulInHeader",
                        &format!("unterminated header: NUL at offset {i}"),
                    );
                }
                b'\n' if data.get(i + 1) == Some(&b'\n') => return false,
                _ => {}
            }
        }
        if data.last() == Some(&b'\n') {
            return false;
        }
        self.report(
            oid,
            kind,
            Level::Error,
            "unterminatedHeader",
            "unterminated header",
        )
    }

    fn check_commit(&mut self, oid: Oid, data: &[u8]) -> bool {
        let kind = ObjectType::Commit;
        if self.check_headers(oid, kind, data) {
            return true;
        }
        let head_end = data
            .windows(2)
            .position(|w| w == b"\n\n")
            .map_or(data.len(), |i| i + 1);
        let mut lines = data[..head_end].split(|b| *b == b'\n').peekable();
        match lines.next().and_then(|l| l.strip_prefix(b"tree ")) {
            None => {
                return self.report(
                    oid,
                    kind,
                    Level::Error,
                    "missingTree",
                    "invalid format - expected 'tree' line",
                );
            }
            Some(l) if l.len() != 40 || parse_hex(l).is_none() => {
                return self.report(
                    oid,
                    kind,
                    Level::Error,
                    "badTreeSha1",
                    "invalid 'tree' line format - bad sha1",
                );
            }
            _ => {}
        }
        while let Some(l) = lines.next_if(|l| l.starts_with(b"parent ")) {
            let hex = &l[7..];
            if hex.len() != 40 || parse_hex(hex).is_none() {
                return self.report(
                    oid,
                    kind,
                    Level::Error,
                    "badParentSha1",
                    "invalid 'parent' line format - bad sha1",
                );
            }
        }
        let mut authors = 0;
        while let Some(l) = lines.next_if(|l| l.starts_with(b"author ")) {
            authors += 1;
            if self.check_ident(oid, kind, &l[7..]) {
                return true;
            }
        }
        if authors == 0 {
            return self.report(
                oid,
                kind,
                Level::Error,
                "missingAuthor",
                "invalid format - expected 'author' line",
            );
        }
        if authors > 1 {
            return self.report(
                oid,
                kind,
                Level::Error,
                "multipleAuthors",
                "invalid format - multiple 'author' lines",
            );
        }
        match lines.next().and_then(|l| l.strip_prefix(b"committer ")) {
            None => {
                return self.report(
                    oid,
                    kind,
                    Level::Error,
                    "missingCommitter",
                    "invalid format - expected 'committer' line",
                );
            }
            Some(l) => {
                if self.check_ident(oid, kind, l) {
                    return true;
                }
            }
        }
        if data.contains(&0) {
            return self.report(
                oid,
                kind,
                Level::Warn,
                "nulInCommit",
                "NUL byte in the commit object body",
            );
        }
        false
    }

    fn check_tag(&mut self, oid: Oid, data: &[u8]) -> bool {
        let kind = ObjectType::Tag;
        if self.check_headers(oid, kind, data) {
            return true;
        }
        let Some(rest) = data.strip_prefix(b"object ") else {
            return self.report(
                oid,
                kind,
                Level::Error,
                "missingObject",
                "invalid format - expected 'object' line",
            );
        };
        if parse_hex(rest).is_none() || rest.get(40) != Some(&b'\n') {
            return self.report(
                oid,
                kind,
                Level::Error,
                "badObjectSha1",
                "invalid 'object' line format - bad sha1",
            );
        }
        let Some(rest) = rest[41..].strip_prefix(b"type ") else {
            return self.report(
                oid,
                kind,
                Level::Error,
                "missingTypeEntry",
                "invalid format - expected 'type' line",
            );
        };
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
            return self.report(
                oid,
                kind,
                Level::Error,
                "missingType",
                "invalid format - unexpected end after 'type' line",
            );
        };
        if !matches!(&rest[..nl], b"commit" | b"tree" | b"blob" | b"tag") {
            return self.report(oid, kind, Level::Error, "badType", "invalid 'type' value");
        }
        let Some(rest) = rest[nl + 1..].strip_prefix(b"tag ") else {
            return self.report(
                oid,
                kind,
                Level::Error,
                "missingTagEntry",
                "invalid format - expected 'tag' line",
            );
        };
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
            return self.report(
                oid,
                kind,
                Level::Error,
                "missingTag",
                "invalid format - unexpected end after 'type' line",
            );
        };
        let name = String::from_utf8_lossy(&rest[..nl]).into_owned();
        if !git2::Reference::is_valid_name(&format!("refs/tags/{name}"))
            && self.report(
                oid,
                kind,
                Level::Info,
                "badTagName",
                &format!("invalid 'tag' name: {name}"),
            )
        {
            return true;
        }
        let rest = &rest[nl + 1..];
        let rest = match rest.strip_prefix(b"tagger ") {
            None => {
                if self.report(
                    oid,
                    kind,
                    Level::Info,
                    "missingTaggerEntry",
                    "invalid format - expected 'tagger' line",
                ) {
                    return true;
                }
                rest
            }
            Some(r) => {
                let end = r.iter().position(|b| *b == b'\n').unwrap_or(r.len());
                if self.check_ident(oid, kind, &r[..end]) {
                    return true;
                }
                &r[(end + 1).min(r.len())..]
            }
        };
        // Extra headers after the tagger are ignored, as git does by default.
        let _ = rest;
        false
    }

    fn check_tree(&mut self, oid: Oid, links: &Links) -> bool {
        let Links::Tree(entries) = links else {
            return false;
        };
        let kind = ObjectType::Tree;
        let (mut null, mut full, mut dot, mut dotdot, mut dotgit, mut bad_mode) =
            (false, false, false, false, false, false);
        let (mut dups, mut unsorted) = (false, false);
        let mut last: Option<(u32, &str)> = None;
        let mut special = Vec::new();
        for (mode, name, id) in entries {
            let link = *mode == 0o120000;
            if is_dotfile(name, "gitmodules", "gitmod") {
                if link {
                    special.push((
                        Level::Error,
                        "gitmodulesSymlink",
                        ".gitmodules is a symbolic link",
                    ));
                } else {
                    self.gitmodules.push(*id);
                }
            }
            if is_dotfile(name, "gitattributes", "gitatt") {
                if link {
                    special.push((
                        Level::Info,
                        "gitattributesSymlink",
                        ".gitattributes is a symlink",
                    ));
                } else {
                    self.gitattributes.push(*id);
                }
            }
            if link && is_dotfile(name, "gitignore", "gitign") {
                special.push((Level::Info, "gitignoreSymlink", ".gitignore is a symlink"));
            }
            if link && is_dotfile(name, "mailmap", "mailma") {
                special.push((Level::Info, "mailmapSymlink", ".mailmap is a symlink"));
            }
            null |= id.is_zero();
            full |= name.contains('/');
            dot |= name == ".";
            dotdot |= name == "..";
            dotgit |= name.eq_ignore_ascii_case(".git") || name.eq_ignore_ascii_case("git~1");
            match *mode {
                0o100755 | 0o100644 | 0o120000 | 0o040000 | 0o160000 => {}
                0o100664 if !self.o.strict => {}
                _ => bad_mode = true,
            }
            if let Some((m1, n1)) = last {
                let (a, b) = (n1.as_bytes(), name.as_bytes());
                let len = a.len().min(b.len());
                match a[..len].cmp(&b[..len]) {
                    std::cmp::Ordering::Less => {}
                    std::cmp::Ordering::Greater => unsorted = true,
                    std::cmp::Ordering::Equal => {
                        let c1 =
                            a.get(len)
                                .copied()
                                .unwrap_or(if m1 == 0o040000 { b'/' } else { 0 });
                        let c2 =
                            b.get(len)
                                .copied()
                                .unwrap_or(if *mode == 0o040000 { b'/' } else { 0 });
                        if a.len() == b.len() {
                            dups = true;
                        } else if c1 >= c2 {
                            unsorted = true;
                        }
                    }
                }
            }
            last = Some((*mode, name));
        }
        let zero_pad = self
            .odb
            .read(oid)
            .map(|o| {
                let data = o.data();
                let mut at = 0;
                let mut found = false;
                while at < data.len() {
                    found |= data[at] == b'0';
                    let Some(nul) = data[at..].iter().position(|b| *b == 0) else {
                        break;
                    };
                    at += nul + 21;
                }
                found
            })
            .unwrap_or(false);
        let mut err = false;
        for (level, id, text) in special {
            err |= self.report(oid, kind, level, id, text);
        }
        for (on, level, id, text) in [
            (
                null,
                Level::Warn,
                "nullSha1",
                "contains entries pointing to null sha1",
            ),
            (full, Level::Warn, "fullPathname", "contains full pathnames"),
            (dot, Level::Warn, "hasDot", "contains '.'"),
            (dotdot, Level::Warn, "hasDotdot", "contains '..'"),
            (dotgit, Level::Warn, "hasDotgit", "contains '.git'"),
            (
                zero_pad,
                Level::Warn,
                "zeroPaddedFilemode",
                "contains zero-padded file modes",
            ),
            (
                bad_mode,
                Level::Info,
                "badFilemode",
                "contains bad file modes",
            ),
            (
                dups,
                Level::Error,
                "duplicateEntries",
                "contains duplicate file entries",
            ),
            (
                unsorted,
                Level::Error,
                "treeNotSorted",
                "not properly sorted",
            ),
        ] {
            if on {
                err |= self.report(oid, kind, level, id, text);
            }
        }
        err
    }

    /// git's fsck_obj for an object read from disk.
    fn check_object(&mut self, oid: Oid, kind: ObjectType, data: &[u8], path: Option<&str>) {
        self.table.touch(oid).kind = Some(kind);
        let Some(links) = parse(kind, data) else {
            if kind == ObjectType::Commit {
                self.err
                    .push_str(&format!("error: bogus commit object {oid}\n"));
            }
            self.code |= ERROR_OBJECT;
            match path {
                Some(p) => self
                    .err
                    .push_str(&format!("error: {oid}: object could not be parsed: {p}\n")),
                None => self
                    .err
                    .push_str(&format!("error: {oid}: object corrupt or missing\n")),
            }
            return;
        };
        // Parsing creates what a commit or tag names; walking a tree, its entries.
        for (child, ck) in links.children() {
            let o = self.table.touch(child);
            o.kind.get_or_insert(ck);
            o.used = true;
        }
        let obj = self.table.touch(oid);
        obj.has = true;
        obj.reachable = false;
        let err = match kind {
            ObjectType::Commit => self.check_commit(oid, data),
            ObjectType::Tag => self.check_tag(oid, data),
            ObjectType::Tree => self.check_tree(oid, &links),
            _ => false,
        };
        if err {
            self.code |= ERROR_OBJECT;
        } else {
            match &links {
                Links::Commit { parents, .. } if parents.is_empty() && self.o.root => {
                    self.out
                        .push_str(&format!("root {}\n", self.describe(&oid)));
                }
                Links::Tag { target, kind, name } if self.o.tags => {
                    self.out.push_str(&format!(
                        "tagged {} {} ({}) in {}\n",
                        type_name(Some(*kind)),
                        self.describe(target),
                        name.as_deref().unwrap_or(""),
                        self.describe(&oid)
                    ));
                }
                _ => {}
            }
        }
        self.links.insert(oid, links);
    }

    /// git's fsck_blob and fsck_finish for the blobs trees name
    /// .gitmodules or .gitattributes.
    fn check_special_blobs(&mut self) {
        let mut modules = std::mem::take(&mut self.gitmodules);
        modules.sort();
        modules.dedup();
        let mut attrs = std::mem::take(&mut self.gitattributes);
        attrs.sort();
        attrs.dedup();
        let blob = ObjectType::Blob;
        for (ids, file, missing, not_blob) in [
            (
                &modules,
                ".gitmodules",
                "gitmodulesMissing",
                "gitmodulesBlob",
            ),
            (
                &attrs,
                ".gitattributes",
                "gitattributesMissing",
                "gitattributesBlob",
            ),
        ] {
            for id in ids {
                let read = self.odb.read(*id).map(|o| (o.kind(), o.data().to_vec()));
                let Ok((kind, data)) = read else {
                    if self.report(
                        *id,
                        blob,
                        Level::Error,
                        missing,
                        &format!("unable to read {file} blob"),
                    ) {
                        self.code |= ERROR_OBJECT;
                    }
                    continue;
                };
                if kind != blob {
                    if self.report(
                        *id,
                        kind,
                        Level::Error,
                        not_blob,
                        &format!("non-blob found at {file}"),
                    ) {
                        self.code |= ERROR_OBJECT;
                    }
                    continue;
                }
                let err = if file == ".gitmodules" {
                    self.check_gitmodules(*id, &data)
                } else {
                    self.check_gitattributes(*id, &data)
                };
                if err {
                    self.code |= ERROR_OBJECT;
                }
            }
        }
    }

    fn check_gitmodules(&mut self, oid: Oid, data: &[u8]) -> bool {
        let blob = ObjectType::Blob;
        let big = crate::maintenance::cfg_int(self.repo, "core.bigFileThreshold", 512 << 20);
        if data.len() as i64 > big {
            return self.report(
                oid,
                blob,
                Level::Error,
                "gitmodulesLarge",
                ".gitmodules too large to parse",
            );
        }
        let tmp =
            std::env::temp_dir().join(format!("rgit-fsck-gitmodules-{}-{oid}", std::process::id()));
        let entries: Option<Vec<(String, Option<String>)>> = std::fs::write(&tmp, data)
            .ok()
            .and_then(|()| git2::Config::open(&tmp).ok())
            .and_then(|c| {
                let mut out = Vec::new();
                let mut it = c.entries(None).ok()?;
                while let Some(e) = it.next() {
                    let e = e.ok()?;
                    out.push((e.name().ok()?.to_owned(), e.value().ok().map(str::to_owned)));
                }
                Some(out)
            });
        let _ = std::fs::remove_file(&tmp);
        let Some(entries) = entries else {
            return self.report(
                oid,
                blob,
                Level::Info,
                "gitmodulesParse",
                "could not parse gitmodules blob",
            );
        };
        let mut err = false;
        for (name, value) in entries {
            let Some((sub, key)) = name
                .strip_prefix("submodule.")
                .and_then(|r| r.rsplit_once('.'))
            else {
                continue;
            };
            if bad_submodule_name(sub) {
                err |= self.report(
                    oid,
                    blob,
                    Level::Error,
                    "gitmodulesName",
                    &format!("disallowed submodule name: {sub}"),
                );
            }
            let Some(v) = value else {
                continue;
            };
            if key == "url" && bad_submodule_url(&v) {
                err |= self.report(
                    oid,
                    blob,
                    Level::Error,
                    "gitmodulesUrl",
                    &format!("disallowed submodule url: {v}"),
                );
            }
            if key == "path" && v.starts_with('-') {
                err |= self.report(
                    oid,
                    blob,
                    Level::Error,
                    "gitmodulesPath",
                    &format!("disallowed submodule path: {v}"),
                );
            }
            if key == "update" && v.starts_with('!') {
                err |= self.report(
                    oid,
                    blob,
                    Level::Error,
                    "gitmodulesUpdate",
                    &format!("disallowed submodule update setting: {v}"),
                );
            }
        }
        err
    }

    fn check_gitattributes(&mut self, oid: Oid, data: &[u8]) -> bool {
        let blob = ObjectType::Blob;
        if data.len() > 100 << 20 {
            return self.report(
                oid,
                blob,
                Level::Error,
                "gitattributesLarge",
                ".gitattributes too large to parse",
            );
        }
        let text = data.split(|b| *b == 0).next().unwrap_or_default();
        if text.split(|b| *b == b'\n').any(|l| l.len() >= 2048) {
            return self.report(
                oid,
                blob,
                Level::Error,
                "gitattributesLineLength",
                ".gitattributes has too long lines to parse",
            );
        }
        false
    }

    fn check_loose(&mut self, oid: Oid, path: &Path) {
        let shown = format!(
            "{}/{}",
            self.objects_dir,
            path.strip_prefix(path.parent().and_then(Path::parent).unwrap_or(path))
                .unwrap_or(path)
                .display()
        );
        let raw = std::fs::read(path).ok().and_then(|z| {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(&z[..])
                .read_to_end(&mut out)
                .ok()?;
            Some(out)
        });
        let Some(raw) = raw else {
            self.code |= ERROR_OBJECT;
            self.err.push_str(&format!(
                "error: {oid}: object corrupt or missing: {shown}\n"
            ));
            return;
        };
        let real = {
            use sha1::Digest;
            Oid::from_bytes(&sha1::Sha1::digest(&raw)).ok()
        };
        let Some(nul) = raw.iter().position(|b| *b == 0) else {
            self.code |= ERROR_OBJECT;
            self.err.push_str(&format!(
                "error: {oid}: object corrupt or missing: {shown}\n"
            ));
            return;
        };
        let header = String::from_utf8_lossy(&raw[..nul]).into_owned();
        let type_word = header.split(' ').next().unwrap_or("");
        let kind = match type_word {
            "commit" => ObjectType::Commit,
            "tree" => ObjectType::Tree,
            "blob" => ObjectType::Blob,
            "tag" => ObjectType::Tag,
            other => {
                self.code |= ERROR_OBJECT;
                self.err.push_str(&format!(
                    "error: {}: object is of unknown type '{other}': {shown}\n",
                    real.unwrap_or(oid)
                ));
                return;
            }
        };
        if real != Some(oid) {
            self.code |= ERROR_OBJECT;
            self.err.push_str(&format!(
                "error: {}: hash-path mismatch, found at: {shown}\n",
                real.map_or_else(|| oid.to_string(), |r| r.to_string())
            ));
            return;
        }
        self.check_object(oid, kind, &raw[nul + 1..], Some(&shown));
    }

    /// git's mark_object: reach `oid` from `parent` (a root when None).
    fn mark(&mut self, oid: Oid, want: Option<ObjectType>, parent: Option<Oid>, lookup: bool) {
        let actual = self.table.objs.get(&oid).and_then(|o| o.kind);
        if let (Some(w), Some(a), Some(p)) = (want, actual, parent)
            && w != a
        {
            let pk = self.kind_of(&p);
            self.code |= ERROR_OBJECT;
            self.err.push_str(&format!(
                "error in {} {}: wrong object type in link\n",
                type_name(pk),
                self.describe(&p)
            ));
        }
        let obj = self.table.find(oid, lookup);
        if let (None, Some(w)) = (obj.kind, want) {
            obj.kind = Some(w);
        }
        if obj.reachable {
            return;
        }
        obj.reachable = true;
        if !obj.has {
            if let Some(p) = parent
                && !self.odb.exists(oid)
            {
                let pk = self.kind_of(&p);
                let ok = self.table.objs.get(&oid).and_then(|o| o.kind);
                self.code |= ERROR_REACHABLE;
                self.out.push_str(&format!(
                    "broken link from {:>7} {}\n              to {:>7} {}\n",
                    type_name(pk),
                    self.describe(&p),
                    type_name(ok),
                    self.describe(&oid)
                ));
            }
            return;
        }
        self.pending.push(oid);
    }

    /// Parsed links of an object present on disk (read on demand in
    /// connectivity-only mode, creating what it names as git's parse does).
    fn links_of(&mut self, oid: Oid) -> Option<&Links> {
        if !self.links.contains_key(&oid) {
            let obj = self.odb.read(oid).ok()?;
            let links = parse(obj.kind(), obj.data())?;
            self.table.touch(oid).kind = Some(obj.kind());
            if let Links::Commit { .. } | Links::Tag { .. } = links {
                for (child, ck) in links.children() {
                    self.table.touch(child).kind.get_or_insert(ck);
                }
            }
            self.links.insert(oid, links);
        }
        self.links.get(&oid)
    }

    fn traverse(&mut self) {
        while let Some(oid) = self.pending.pop() {
            let name = self.names.get(&oid).cloned();
            let Some(links) = self.links_of(oid) else {
                continue;
            };
            let tree = matches!(links, Links::Tree(_));
            let (steps, errors, tag) = walk_steps(oid, links, name.as_deref());
            for e in errors {
                self.err.push_str(&e);
            }
            for (child, kind, cname) in steps {
                if let Some(n) = cname {
                    self.put_name(child, n);
                }
                self.mark(child, (!tag).then_some(kind), Some(oid), tree);
            }
        }
    }

    /// A root: a ref, HEAD, reflog entry, index entry or object argument.
    fn root(&mut self, oid: Oid, name: String) {
        self.table.touch(oid).used = true;
        self.put_name(oid, name);
        self.mark(oid, None, None, true);
    }

    fn handle_ref(&mut self, name: &str, oid: Oid) -> bool {
        let exists = self.table.objs.get(&oid).is_some_and(|o| o.has) || self.odb.exists(oid);
        if !exists {
            self.code |= ERROR_REACHABLE;
            self.err
                .push_str(&format!("error: {name}: invalid sha1 pointer {oid}\n"));
            return false;
        }
        if name.starts_with("refs/heads/") && self.kind_of(&oid) != Some(ObjectType::Commit) {
            self.code |= ERROR_REFS;
            self.err.push_str(&format!("error: {name}: not a commit\n"));
        }
        self.root(oid, name.to_owned());
        true
    }

    fn head(&mut self, file: &Path, name: &str) -> usize {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let text = text.trim();
        let mut found = 0;
        if let Some(target) = text.strip_prefix("ref: ") {
            match self.repo.refname_to_id(target) {
                Ok(oid) => found += usize::from(self.handle_ref(name, oid)),
                Err(_) if target.starts_with("refs/heads/") => self.err.push_str(&format!(
                    "notice: {name} points to an unborn branch ({})\n",
                    &target[11..]
                )),
                Err(_) => {
                    self.code |= ERROR_REFS;
                    self.err.push_str(&format!(
                        "error: {name} points to something strange ({target})\n"
                    ));
                }
            }
        } else if let Ok(oid) = Oid::from_str(text) {
            if oid.is_zero() {
                self.code |= ERROR_REFS;
                self.err
                    .push_str(&format!("error: {name}: detached HEAD points at nothing\n"));
            } else {
                found += usize::from(self.handle_ref(name, oid));
            }
        } else {
            self.code |= ERROR_REFS;
            self.err.push_str(&format!("error: invalid {name}\n"));
        }
        found
    }

    fn reflog(&mut self, name: &str, path: &Path) {
        for l in maintenance::read_reflog(path) {
            for (oid, when) in [(l.old, 0), (l.new, l.time)] {
                if oid.is_zero() {
                    continue;
                }
                if self.table.objs.get(&oid).is_some_and(|o| o.has) {
                    self.root(oid, format!("{name}@{{{when}}}"));
                } else {
                    self.code |= ERROR_REACHABLE;
                    self.err
                        .push_str(&format!("error: {name}: invalid reflog entry {oid}\n"));
                }
            }
        }
    }

    fn default_heads(&mut self) {
        let mut refs: Vec<(String, Oid)> = self
            .repo
            .references()
            .map(|it| {
                it.flatten()
                    .filter_map(|r| {
                        let name = r.name().ok()?.to_owned();
                        let oid = r.resolve().ok()?.target()?;
                        Some((name, oid))
                    })
                    .collect()
            })
            .unwrap_or_default();
        refs.sort();
        let mut found = 0;
        for (name, oid) in refs {
            found += usize::from(self.handle_ref(&name, oid));
        }
        let common = self.repo.commondir().to_path_buf();
        found += self.head(&common.join("HEAD"), "HEAD");
        if self.o.reflogs {
            let logs: Vec<(String, PathBuf)> = maintenance::reflog_files(self.repo)
                .into_iter()
                .filter(|(_, p)| p.starts_with(common.join("logs")))
                .collect();
            for (name, path) in logs {
                self.reflog(&name, &path);
            }
        }
        for wt in maintenance::worktree_dirs(self.repo) {
            let id = wt
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let name = format!("worktrees/{id}/HEAD");
            found += self.head(&wt.join("HEAD"), &name);
            if self.o.reflogs && wt.join("logs/HEAD").is_file() {
                self.reflog("HEAD", &wt.join("logs/HEAD"));
            }
        }
        if found == 0 {
            self.err.push_str("notice: No default references\n");
        }
    }

    fn index(&mut self) {
        let (entries, trees) = maintenance::read_index(&self.repo.path().join("index"));
        for e in entries {
            if e.mode == 0o160000 {
                continue;
            }
            self.table.touch(e.oid).kind.get_or_insert(ObjectType::Blob);
            self.root(e.oid, format!(":{}", e.path));
        }
        for t in trees {
            if !self.odb.exists(t) {
                self.code |= ERROR_REFS;
                self.err
                    .push_str(&format!("error: {t}: invalid sha1 pointer in cache-tree\n"));
                continue;
            }
            if self.kind_of(&t) != Some(ObjectType::Tree) {
                self.code |= ERROR_OBJECT;
                self.err.push_str(&format!(
                    "error in {} {}: non-tree in cache-tree\n",
                    type_name(self.kind_of(&t)),
                    self.describe(&t)
                ));
            }
            self.root(t, ":".to_owned());
        }
    }
}

type Step = (Oid, ObjectType, Option<String>);

/// What walking `oid` reaches, named from `name` as git's fsck_walk names
/// them; bad-mode errors; and whether it is a tag (whose target may be any
/// type).
fn walk_steps(oid: Oid, links: &Links, name: Option<&str>) -> (Vec<Step>, Vec<String>, bool) {
    let mut steps = Vec::new();
    let mut errors = Vec::new();
    match links {
        Links::Commit { tree, parents } => {
            steps.push((*tree, ObjectType::Tree, name.map(|n| format!("{n}:"))));
            let (generation, prefix) = match name {
                Some(n) if n.ends_with('^') => (1, n.len() - 1),
                Some(n) => {
                    let digits = n.len() - n.trim_end_matches(|c: char| c.is_ascii_digit()).len();
                    let head = &n[..n.len() - digits];
                    if digits > 0 && head.ends_with('~') {
                        (
                            n[n.len() - digits..].parse::<u64>().unwrap_or(0),
                            head.len() - 1,
                        )
                    } else {
                        (0, n.len())
                    }
                }
                None => (0, 0),
            };
            for (i, p) in parents.iter().enumerate() {
                let pname = name.map(|n| {
                    if i > 0 {
                        format!("{n}^{}", i + 1)
                    } else if generation > 0 {
                        format!("{}~{}", &n[..prefix], generation + 1)
                    } else {
                        format!("{n}^")
                    }
                });
                steps.push((*p, ObjectType::Commit, pname));
            }
        }
        Links::Tree(entries) => {
            for (mode, path, id) in entries {
                match mode & 0o170000 {
                    0o040000 => {
                        steps.push((*id, ObjectType::Tree, name.map(|n| format!("{n}{path}/"))))
                    }
                    0o100000 | 0o120000 => {
                        steps.push((*id, ObjectType::Blob, name.map(|n| format!("{n}{path}"))))
                    }
                    0o160000 => {}
                    _ => errors.push(format!(
                        "error: in tree {oid}: entry {path} has bad mode {mode:06o}\n"
                    )),
                }
            }
        }
        Links::Tag { target, kind, .. } => steps.push((*target, *kind, name.map(str::to_owned))),
        Links::Blob => {}
    }
    (steps, errors, matches!(links, Links::Tag { .. }))
}

/// Loose objects in git's order: fan-out folders 00..ff, files in the order
/// the file system lists them.
fn loose_in_order(repo: &Repository) -> Vec<(Oid, PathBuf)> {
    let objects = repo.commondir().join("objects");
    let mut out = Vec::new();
    for i in 0..256 {
        let dir = objects.join(format!("{i:02x}"));
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.len() == 38
                && let Ok(oid) = Oid::from_str(&format!("{i:02x}{name}"))
            {
                out.push((oid, e.path()));
            }
        }
    }
    out
}

/// Packs as git lists them: newest first.
fn packs_in_order(repo: &Repository) -> Vec<maintenance::Pack> {
    let mut packs = maintenance::packs(repo);
    packs.sort_by_key(|p| std::cmp::Reverse(p.mtime));
    packs
}

/// Run `git fsck`.
pub fn fsck(repo: &Repository, o: &FsckOptions) -> Result<FsckReport, GitError> {
    let mut o = o.clone();
    if o.lost_found {
        o.full = true;
        o.reflogs = false;
    }
    let objects_dir = match repo.workdir() {
        Some(w) if repo.path() == w.join(".git") || repo.path() == w.join(".git/") => {
            ".git/objects".to_owned()
        }
        _ => repo.commondir().join("objects").display().to_string(),
    };
    let (levels, skip) = severity_config(repo);
    let mut f = Fsck {
        repo,
        odb: repo.odb()?,
        o: &o,
        table: Table::default(),
        names: HashMap::new(),
        links: HashMap::new(),
        pending: Vec::new(),
        packed: Default::default(),
        out: String::new(),
        err: String::new(),
        code: 0,
        objects_dir,
        levels,
        skip,
        gitmodules: Vec::new(),
        gitattributes: Vec::new(),
    };
    let packs = packs_in_order(repo);
    f.packed = packs.iter().flat_map(|p| p.ids()).collect();
    let loose = loose_in_order(repo);
    if o.connectivity_only {
        for (oid, _) in &loose {
            f.table.touch(*oid).has = true;
        }
        for p in &packs {
            for oid in p.ids() {
                f.table.touch(oid).has = true;
            }
        }
    } else {
        for (oid, path) in &loose {
            f.check_loose(*oid, path);
        }
        if o.full {
            for p in &packs {
                let mut order = maintenance::idx_offsets(&p.path.with_extension("idx"));
                order.sort_by_key(|(_, off)| *off);
                for (oid, _) in order {
                    let read = f.odb.read(oid).map(|obj| (obj.kind(), obj.data().to_vec()));
                    match read {
                        Ok((kind, data)) => f.check_object(oid, kind, &data, None),
                        Err(_) => {
                            f.code |= ERROR_PACK | ERROR_OBJECT;
                            f.err
                                .push_str(&format!("error: {oid}: object corrupt or missing\n"));
                        }
                    }
                }
            }
        }
    }
    f.check_special_blobs();
    let mut heads = 0;
    for arg in &o.objects {
        match repo.rev_single(arg) {
            Ok(obj) => {
                let oid = obj.id();
                if !f.table.objs.get(&oid).is_some_and(|x| x.has) {
                    f.code |= ERROR_OBJECT;
                    f.err.push_str(&format!("error: {oid}: object missing\n"));
                    continue;
                }
                f.root(oid, arg.clone());
                heads += 1;
            }
            Err(_) => {
                f.code |= ERROR_OBJECT;
                f.err.push_str(&format!(
                    "error: invalid parameter: expected sha1, got '{arg}'\n"
                ));
            }
        }
    }
    let mut show_unreachable = o.unreachable;
    if heads == 0 {
        let before = f.err.len();
        f.default_heads();
        if f.err[before..].contains("notice: No default references") {
            show_unreachable = false;
        }
    }
    if heads == 0 || o.cache {
        f.index();
    }
    f.traverse();
    if o.connectivity_only {
        let unreached: Vec<Oid> = loose
            .iter()
            .map(|(oid, _)| *oid)
            .chain(packs.iter().flat_map(|p| p.ids()))
            .filter(|oid| f.table.objs.get(oid).is_some_and(|x| x.has && !x.reachable))
            .collect();
        for oid in unreached {
            let children = f.links_of(oid).map(Links::children).unwrap_or_default();
            for (child, kind) in children {
                let c = f.table.touch(child);
                c.kind.get_or_insert(kind);
                c.used = true;
            }
        }
    }
    let order: Vec<Oid> = f.table.slots.iter().flatten().copied().collect();
    for oid in order {
        let (has, reachable, used, kind) = {
            let x = &f.table.objs[&oid];
            (x.has, x.reachable, x.used, x.kind)
        };
        let kind = kind.or_else(|| f.odb.read_header(oid).ok().map(|(_, k)| k));
        if reachable {
            if !has && !f.packed.contains(&oid) {
                f.code |= ERROR_REACHABLE;
                f.out.push_str(&format!(
                    "missing {} {}\n",
                    type_name(kind),
                    f.describe(&oid)
                ));
            }
            continue;
        }
        if !has {
            continue;
        }
        if show_unreachable {
            f.out.push_str(&format!(
                "unreachable {} {}\n",
                type_name(kind),
                f.describe(&oid)
            ));
            continue;
        }
        if used {
            continue;
        }
        if o.dangling {
            f.out.push_str(&format!(
                "dangling {} {}\n",
                type_name(kind),
                f.describe(&oid)
            ));
        }
        if o.lost_found {
            let dir = repo
                .path()
                .join("lost-found")
                .join(if kind == Some(ObjectType::Commit) {
                    "commit"
                } else {
                    "other"
                });
            std::fs::create_dir_all(&dir)?;
            let body = match (kind, f.odb.read(oid)) {
                (Some(ObjectType::Blob), Ok(b)) => b.data().to_vec(),
                _ => format!("{}\n", f.describe(&oid)).into_bytes(),
            };
            std::fs::write(dir.join(f.describe(&oid)), body)?;
        }
    }
    if crate::maintenance::cfg_bool(repo, "core.commitGraph", true) {
        let errors = crate::commit_graph::verify(repo, false);
        if !errors.is_empty() {
            f.code |= ERROR_COMMIT_GRAPH;
            f.err.extend(errors.into_iter().map(|e| e + "\n"));
        }
    }
    if crate::maintenance::cfg_bool(repo, "core.multiPackIndex", true) {
        let errors = crate::midx::verify(repo);
        if !errors.is_empty() {
            f.code |= ERROR_MULTI_PACK_INDEX;
            f.err.extend(errors.into_iter().map(|e| e + "\n"));
        }
    }
    Ok(FsckReport {
        stdout: f.out,
        stderr: f.err,
        code: f.code,
    })
}
