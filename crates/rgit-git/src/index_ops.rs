//! git's index and object plumbing: commit-tree, write-tree, read-tree,
//! update-index, checkout-index, mktree and mktag, over libgit2's index and
//! object database, with git's rules and messages.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use git2::build::CheckoutBuilder;
use git2::{Index, IndexEntry, IndexTime, ObjectType, Oid, Repository};

use crate::error::GitError;

/// What a command printed: `out` for stdout, `err` for stderr, and whether
/// it should exit 1.
#[derive(Default, Debug)]
pub struct Report {
    pub out: String,
    pub err: String,
    pub failed: bool,
}

const VALID: u16 = 0x8000;
const SKIP_WORKTREE: u16 = 1 << 14;
const INTENT_TO_ADD: u16 = 1 << 13;

fn other(message: impl Into<String>) -> GitError {
    GitError::Other(message.into())
}

/// The repository at `git_dir` and its index, `GIT_INDEX_FILE` when set.
fn open(git_dir: &Path) -> Result<(Repository, Index), GitError> {
    // git lets the index name objects it does not have (--cacheinfo, --index-info).
    git2::opts::strict_object_creation(false);
    let repo = Repository::open(git_dir)?;
    if let Some(file) = std::env::var_os("GIT_INDEX_FILE") {
        let mut index = Index::open(Path::new(&file))?;
        repo.set_index(&mut index)?;
    }
    let index = repo.index()?;
    Ok((repo, index))
}

fn top(repo: &Repository) -> Result<PathBuf, GitError> {
    repo.workdir()
        .map(Path::to_path_buf)
        .ok_or_else(|| other("this operation must be run in a work tree"))
}

fn stage(e: &IndexEntry) -> u16 {
    (e.flags >> 12) & 3
}

fn entry(path: &[u8], mode: u32, id: Oid, stage: u16) -> IndexEntry {
    IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        file_size: 0,
        id,
        flags: stage << 12,
        flags_extended: 0,
        path: path.to_vec(),
    }
}

/// `path` typed in the folder `prefix` as a path from the top, as git's
/// prefix_path normalizes it.
fn from_top(prefix: &str, path: &str) -> Result<String, GitError> {
    let mut parts: Vec<&str> = Vec::new();
    let joined = format!("{prefix}{path}");
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(other(format!("'{path}' is outside repository")));
                }
            }
            p => parts.push(p),
        }
    }
    Ok(parts.join("/"))
}

/// git's verify_path: no empty, `.` or `..` component and no `.git`.
fn valid_path(path: &[u8]) -> bool {
    !path.is_empty()
        && path
            .split(|b| *b == b'/')
            .all(|c| !c.is_empty() && c != b"." && c != b".." && !c.eq_ignore_ascii_case(b".git"))
}

/// A tree object's bytes for these entries, in git's order (a tree sorts as
/// if its name ended in `/`).
fn write_raw_tree(repo: &Repository, mut items: Vec<(Vec<u8>, u32, Oid)>) -> Result<Oid, GitError> {
    let key = |(name, mode, _): &(Vec<u8>, u32, Oid)| {
        let mut k = name.clone();
        if *mode == 0o40000 {
            k.push(b'/');
        }
        k
    };
    items.sort_by_key(key);
    let mut bytes = Vec::new();
    for (name, mode, id) in &items {
        bytes.extend_from_slice(format!("{mode:o} ").as_bytes());
        bytes.extend_from_slice(name);
        bytes.push(0);
        bytes.extend_from_slice(id.as_bytes());
    }
    Ok(repo.odb()?.write(ObjectType::Tree, &bytes)?)
}

/// Write the trees for index-ordered entries (paths relative to this level).
fn build_tree(repo: &Repository, entries: &[(&[u8], u32, Oid)]) -> Result<Oid, GitError> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let (path, mode, id) = entries[i];
        match path.iter().position(|b| *b == b'/') {
            Some(slash) => {
                let dir = &path[..=slash];
                let mut sub = Vec::new();
                while i < entries.len() && entries[i].0.starts_with(dir) {
                    sub.push((&entries[i].0[slash + 1..], entries[i].1, entries[i].2));
                    i += 1;
                }
                items.push((dir[..slash].to_vec(), 0o40000, build_tree(repo, &sub)?));
            }
            None => {
                items.push((path.to_vec(), mode, id));
                i += 1;
            }
        }
    }
    write_raw_tree(repo, items)
}

/// `git write-tree`: the index as a tree, or the subtree at `prefix`.
pub fn write_tree(
    git_dir: &Path,
    missing_ok: bool,
    prefix: Option<&str>,
) -> Result<String, GitError> {
    let (repo, index) = open(git_dir)?;
    let odb = repo.odb()?;
    let mut bad = String::new();
    let mut entries = Vec::new();
    for e in index.iter() {
        if stage(&e) != 0 {
            bad.push_str(&format!(
                "{}: unmerged ({})\n",
                String::from_utf8_lossy(&e.path),
                e.id
            ));
            continue;
        }
        if e.flags_extended & INTENT_TO_ADD != 0 {
            continue;
        }
        if !missing_ok && e.mode != 0o160000 && !odb.exists(e.id) {
            bad.push_str(&format!(
                "error: invalid object {:06o} {} for '{}'\n",
                e.mode,
                e.id,
                String::from_utf8_lossy(&e.path)
            ));
        }
        entries.push(e);
    }
    if !bad.is_empty() {
        return Err(other(format!(
            "{bad}fatal: git-write-tree: error building trees"
        )));
    }
    let dir = prefix
        .map(|p| format!("{}/", p.trim_end_matches('/')))
        .filter(|p| p != "/");
    let listed: Vec<(&[u8], u32, Oid)> = entries
        .iter()
        .filter_map(|e| {
            let path = match &dir {
                Some(d) => e.path.strip_prefix(d.as_bytes())?,
                None => &e.path[..],
            };
            Some((path, e.mode, e.id))
        })
        .collect();
    if let Some(d) = &dir
        && listed.is_empty()
    {
        return Err(other(format!("git-write-tree: prefix {d} not found")));
    }
    Ok(build_tree(&repo, &listed)?.to_string())
}

/// `git commit-tree`: a commit of `tree` with these parents and message.
pub fn commit_tree(
    git_dir: &Path,
    tree: &str,
    parents: &[String],
    message: &[u8],
) -> Result<Report, GitError> {
    let repo = Repository::open(git_dir)?;
    let tree_id = repo
        .revparse_single(tree)
        .map_err(|_| other(format!("not a valid object name {tree}")))?;
    if tree_id.kind() != Some(ObjectType::Tree) {
        return Err(other(format!(
            "{} is not a valid 'tree' object",
            tree_id.id()
        )));
    }
    let mut report = Report::default();
    let mut seen: Vec<Oid> = Vec::new();
    for p in parents {
        let id = repo
            .revparse_single(p)
            .and_then(|o| o.peel_to_commit())
            .map_err(|_| other(format!("not a valid object name {p}")))?
            .id();
        if seen.contains(&id) {
            report
                .err
                .push_str(&format!("error: duplicate parent {id} ignored\n"));
        } else {
            seen.push(id);
        }
    }
    let mut body = format!("tree {}\n", tree_id.id());
    for p in &seen {
        body.push_str(&format!("parent {p}\n"));
    }
    body.push_str(&format!(
        "author {}\ncommitter {}\n\n",
        crate::plumbing::ident(&repo, false)?,
        crate::plumbing::ident(&repo, true)?
    ));
    let mut bytes = body.into_bytes();
    bytes.extend_from_slice(message);
    let id = repo.odb()?.write(ObjectType::Commit, &bytes)?;
    report.out = format!("{id}\n");
    Ok(report)
}

/// `git mktree`: trees from `ls-tree` lines; one per blank-line-separated
/// group with `batch`.
pub fn mktree(
    git_dir: &Path,
    input: &[u8],
    z: bool,
    missing: bool,
    batch: bool,
) -> Result<String, GitError> {
    let repo = Repository::open(git_dir)?;
    let odb = repo.odb()?;
    let mut out = String::new();
    let mut items: Vec<(Vec<u8>, u32, Oid)> = Vec::new();
    let mut used = false;
    let records: Vec<&[u8]> = input.split(|b| *b == if z { 0 } else { b'\n' }).collect();
    let last = records.len() - 1;
    for (n, line) in records.into_iter().enumerate() {
        if n == last && line.is_empty() {
            break;
        }
        if line.is_empty() {
            if !batch {
                return Err(other(
                    "input format error: (blank line only valid in batch mode)",
                ));
            }
            out.push_str(&format!(
                "{}\n",
                write_raw_tree(&repo, std::mem::take(&mut items))?
            ));
            used = false;
            continue;
        }
        used = true;
        let text = String::from_utf8_lossy(line);
        let bad = || other(format!("input format error: {text}"));
        let (head, path) = text.split_once('\t').ok_or_else(bad)?;
        let mut fields = head.split(' ');
        let (Some(mode), Some(kind), Some(id), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(bad());
        };
        let mode = u32::from_str_radix(mode, 8).map_err(|_| bad())?;
        let id = Oid::from_str(id)
            .ok()
            .filter(|_| id.len() == 40)
            .ok_or_else(bad)?;
        let path = if !z && path.starts_with('"') {
            crate::apply::unquote(path)
        } else {
            path.to_owned()
        };
        if path.contains('/') {
            return Err(other(format!("path {path} contains slash")));
        }
        let mode_kind = match mode {
            0o40000 => "tree",
            0o160000 => "commit",
            _ => "blob",
        };
        if kind != mode_kind {
            return Err(other(format!(
                "entry '{path}' object type ({kind}) doesn't match mode type ({mode_kind})"
            )));
        }
        if mode_kind != "commit" {
            match odb.read_header(id) {
                Ok((_, actual)) if actual.str() != kind => {
                    return Err(other(format!(
                        "entry '{path}' object {id} is a {} but specified type was ({kind})",
                        actual.str()
                    )));
                }
                Err(_) if !missing => {
                    return Err(other(format!("entry '{path}' object {id} is unavailable")));
                }
                _ => {}
            }
        }
        items.push((path.into_bytes(), mode, id));
    }
    if !batch || used {
        out.push_str(&format!("{}\n", write_raw_tree(&repo, items)?));
    }
    Ok(out)
}

/// git's fsck_ident on a `tagger` value; the fsck message id and text.
fn ident_problem(ident: &str) -> Option<&'static str> {
    let Some((name, rest)) = ident.split_once('<') else {
        return Some("missingEmail: invalid author/committer line - missing email");
    };
    if !name.ends_with(' ') {
        return Some(
            "missingSpaceBeforeEmail: invalid author/committer line - missing space before email",
        );
    }
    if name.contains('>') {
        return Some("badName: invalid author/committer line - bad name");
    }
    let Some((email, when)) = rest.split_once('>') else {
        return Some("badEmail: invalid author/committer line - bad email");
    };
    if email.contains('<') {
        return Some("badEmail: invalid author/committer line - bad email");
    }
    let Some(when) = when.strip_prefix(' ') else {
        return Some(
            "missingSpaceBeforeDate: invalid author/committer line - missing space before date",
        );
    };
    let (secs, zone) = when.split_once(' ').unwrap_or((when, ""));
    if secs.is_empty() || !secs.bytes().all(|b| b.is_ascii_digit()) {
        return Some("badDate: invalid author/committer line - bad date");
    }
    if secs.len() > 1 && secs.starts_with('0') {
        return Some("zeroPaddedDate: invalid author/committer line - zero-padded date");
    }
    let zone_ok = zone.len() == 5
        && matches!(zone.as_bytes()[0], b'+' | b'-')
        && zone[1..].bytes().all(|b| b.is_ascii_digit());
    if !zone_ok {
        return Some("badTimezone: invalid author/committer line - bad time zone");
    }
    None
}

/// `git mktag`: check a tag object as git's fsck does, then store it.
/// Without `strict`, problems git only warns about are allowed.
pub fn mktag(git_dir: &Path, input: &[u8], strict: bool) -> Result<Report, GitError> {
    let repo = Repository::open(git_dir)?;
    let mut report = Report::default();
    let text = String::from_utf8_lossy(input);
    let header = match text.find("\n\n") {
        Some(end) => &text[..=end],
        None if text.ends_with('\n') => &text[..],
        None => {
            return Err(other(
                "error: tag input does not pass fsck: unterminatedHeader: unterminated header\n\
                 fatal: tag on stdin did not pass our strict fsck check",
            ));
        }
    };
    let mut lines = header.lines();
    let mut field = |name: &str| {
        lines
            .next()
            .and_then(|l| l.strip_prefix(name))
            .and_then(|l| l.strip_prefix(' '))
            .map(str::to_owned)
    };
    let fatal = |why: &str| {
        other(format!(
            "error: tag input does not pass fsck: {why}\n\
             fatal: tag on stdin did not pass our strict fsck check"
        ))
    };
    let object = field("object")
        .ok_or_else(|| fatal("missingObject: invalid format - expected 'object' line"))?;
    let id = Oid::from_str(&object)
        .ok()
        .filter(|_| object.len() == 40)
        .ok_or_else(|| fatal("badObjectSha1: invalid 'object' line format - bad sha1"))?;
    let kind = field("type")
        .ok_or_else(|| fatal("missingTypeEntry: invalid format - expected 'type' line"))?;
    let kind = ObjectType::from_str(&kind)
        .filter(|k| *k != ObjectType::Any)
        .ok_or_else(|| fatal("badType: invalid 'type' value"))?;
    let name = field("tag")
        .ok_or_else(|| fatal("missingTagEntry: invalid format - expected 'tag' line"))?;
    let mut soft = Vec::new();
    if !git2::Reference::is_valid_name(&format!("refs/tags/{name}")) {
        soft.push(format!("badTagName: invalid 'tag' name: {name}"));
    }
    match field("tagger") {
        None => soft.push("missingTaggerEntry: invalid format - expected 'tagger' line".to_owned()),
        Some(ident) => {
            if let Some(why) = ident_problem(&ident) {
                return Err(fatal(why));
            }
            if lines.next().is_some_and(|l| !l.is_empty()) {
                soft.push(
                    "extraHeaderEntry: invalid format - extra header(s) after 'tagger'".to_owned(),
                );
            }
        }
    }
    if let Some(first) = soft.first() {
        if strict {
            return Err(fatal(first));
        }
        for why in &soft {
            report
                .err
                .push_str(&format!("warning: tag input does not pass fsck: {why}\n"));
        }
    }
    let (_, actual) = repo
        .odb()?
        .read_header(id)
        .map_err(|_| other(format!("could not read tagged object '{id}'")))?;
    if actual != kind {
        return Err(other(format!(
            "object '{id}' tagged as '{}', but is a '{}' type",
            kind.str(),
            actual.str()
        )));
    }
    let tag = repo.odb()?.write(ObjectType::Tag, input)?;
    report.out = format!("{tag}\n");
    Ok(report)
}

/// The working tree's file for an index entry: None when missing, else
/// whether it has the entry's type and content, and its metadata.
fn worktree_state(
    repo: &Repository,
    top: &Path,
    e: &IndexEntry,
) -> Result<Option<(bool, std::fs::Metadata)>, GitError> {
    let path = String::from_utf8_lossy(&e.path).into_owned();
    let full = top.join(&path);
    let Ok(meta) = std::fs::symlink_metadata(&full) else {
        return Ok(None);
    };
    let kind = e.mode & 0o170000;
    let same = if meta.file_type().is_symlink() {
        kind == 0o120000
            && Oid::hash_object(
                ObjectType::Blob,
                std::fs::read_link(&full)?.to_string_lossy().as_bytes(),
            )? == e.id
    } else if meta.is_dir() {
        kind == 0o160000
    } else if kind != 0o100000 {
        false
    } else {
        #[cfg(unix)]
        let exec = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
        #[cfg(not(unix))]
        let exec = e.mode == 0o100755;
        let filemode = repo
            .config()
            .and_then(|c| c.get_bool("core.filemode"))
            .unwrap_or(true);
        let data = crate::plumbing::clean(repo, &path, &std::fs::read(&full)?)?;
        (!filemode || exec == (e.mode == 0o100755))
            && Oid::hash_object(ObjectType::Blob, &data)? == e.id
    };
    Ok(Some((same, meta)))
}

/// Copy a file's stat data into an index entry, as git's fill_stat_data does.
fn fill_stat(e: &mut IndexEntry, meta: &std::fs::Metadata) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        e.ctime = IndexTime::new(meta.ctime() as i32, meta.ctime_nsec() as u32);
        e.mtime = IndexTime::new(meta.mtime() as i32, meta.mtime_nsec() as u32);
        e.dev = meta.dev() as u32;
        e.ino = meta.ino() as u32;
        e.uid = meta.uid();
        e.gid = meta.gid();
        e.file_size = meta.size() as u32;
    }
    #[cfg(not(unix))]
    {
        e.file_size = meta.len() as u32;
    }
}

/// Write these index paths into the working tree (or under `target`) from
/// the index on disk, as git's checkout_entry does, updating the index's
/// stat data when `update_index`.
fn checkout_paths(
    repo: &Repository,
    paths: &[String],
    target: Option<&Path>,
    update_index: bool,
) -> Result<(), GitError> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut checkout = CheckoutBuilder::new();
    checkout
        .force()
        .allow_conflicts(true)
        .disable_pathspec_match(true)
        .update_index(update_index);
    if let Some(dir) = target {
        checkout.target_dir(dir);
    }
    for p in paths {
        checkout.path(p);
    }
    repo.checkout_index(None, Some(&mut checkout))?;
    Ok(())
}

/// Remove a working tree file and the folders it leaves empty.
fn remove_worktree_file(top: &Path, path: &str) {
    let full = top.join(path);
    if std::fs::remove_file(&full).is_ok() {
        let mut dir = full.parent();
        while let Some(d) = dir
            && d != top
            && std::fs::remove_dir(d).is_ok()
        {
            dir = d.parent();
        }
    }
}

/// `git read-tree`'s options.
#[derive(Default, Clone)]
pub struct ReadTreeOpts {
    pub merge: bool,
    pub reset: bool,
    pub update: bool,
    pub index_only: bool,
    pub dry_run: bool,
    pub aggressive: bool,
    pub prefix: Option<String>,
    pub empty: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct Ent {
    mode: u32,
    id: Oid,
}

fn ent(e: &IndexEntry) -> Ent {
    Ent {
        mode: e.mode,
        id: e.id,
    }
}

fn same(a: Option<Ent>, b: Option<Ent>) -> bool {
    a == b
}

/// A tree's files by path.
fn flatten(repo: &Repository, rev: &str) -> Result<BTreeMap<Vec<u8>, Ent>, GitError> {
    let tree = repo
        .revparse_single(rev)
        .and_then(|o| o.peel_to_tree())
        .map_err(|_| other(format!("Not a valid object name {rev}")))?;
    let mut files = BTreeMap::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
        if e.kind() != Some(ObjectType::Tree) {
            let mut path = root.as_bytes().to_vec();
            path.extend_from_slice(e.name_bytes());
            files.insert(
                path,
                Ent {
                    mode: e.filemode() as u32,
                    id: e.id(),
                },
            );
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(files)
}

/// unpack-trees' merge of one path: the new index entries, what to write or
/// delete in the working tree, and the first rejected path.
struct Unpack<'a> {
    o: &'a ReadTreeOpts,
    repo: &'a Repository,
    top: Option<PathBuf>,
    result: Vec<IndexEntry>,
    update: Vec<String>,
    remove: Vec<String>,
    errors: Vec<String>,
}

impl Unpack<'_> {
    fn path(p: &[u8]) -> String {
        String::from_utf8_lossy(p).into_owned()
    }

    /// The working tree is clean for `old`, or the merge may overwrite it.
    fn uptodate(&mut self, old: &IndexEntry) -> bool {
        if self.o.index_only || self.o.reset {
            return true;
        }
        let Some(top) = &self.top else { return true };
        match worktree_state(self.repo, top, old) {
            Ok(None) | Ok(Some((true, _))) => true,
            _ if old.mode == 0o160000 => true,
            _ => {
                self.errors.push(format!(
                    "error: Entry '{}' not uptodate. Cannot merge.",
                    Self::path(&old.path)
                ));
                false
            }
        }
    }

    /// No untracked file is in the way of writing `path`.
    fn absent(&mut self, path: &[u8]) -> bool {
        if !self.o.update || self.o.reset {
            return true;
        }
        let Some(top) = &self.top else { return true };
        let name = Self::path(path);
        let full = top.join(&name);
        if std::fs::symlink_metadata(&full).is_err()
            || full.is_dir()
            || self.repo.is_path_ignored(&name).unwrap_or(false)
        {
            return true;
        }
        self.errors.push(format!(
            "error: Untracked working tree file '{name}' would be overwritten by merge."
        ));
        false
    }

    fn keep(&mut self, old: &IndexEntry) {
        self.result.push(dup(old));
    }

    fn keep_stage(&mut self, path: &[u8], e: Ent, stage: u16) {
        self.result.push(entry(path, e.mode, e.id, stage));
    }

    fn merged(&mut self, path: &[u8], new: Ent, old: Option<&IndexEntry>) {
        match old {
            None => {
                if !self.absent(path) {
                    return;
                }
            }
            Some(old) if ent(old) == new => return self.keep(old),
            Some(old) => {
                if !self.uptodate(old) {
                    return;
                }
            }
        }
        self.result.push(entry(path, new.mode, new.id, 0));
        self.update.push(Self::path(path));
    }

    fn deleted(&mut self, path: &[u8], old: Option<&IndexEntry>) {
        if let Some(old) = old
            && self.uptodate(old)
        {
            self.remove.push(Self::path(path));
        }
    }

    fn reject(&mut self, path: &[u8]) {
        self.errors.push(format!(
            "error: Entry '{}' would be overwritten by merge. Cannot merge.",
            Self::path(path)
        ));
    }

    fn oneway(&mut self, path: &[u8], old: Option<&IndexEntry>, a: Option<Ent>) {
        let Some(a) = a else {
            return self.deleted(path, old);
        };
        if let Some(old) = old
            && ent(old) == a
        {
            self.keep(old);
            if self.o.reset
                && self.o.update
                && let Some(top) = &self.top
                && !matches!(worktree_state(self.repo, top, old), Ok(Some((true, _))))
            {
                self.update.push(Self::path(path));
            }
            return;
        }
        self.merged(path, a, old);
    }

    fn twoway(
        &mut self,
        path: &[u8],
        cur: Option<&IndexEntry>,
        h: Option<Ent>,
        m: Option<Ent>,
        initial: bool,
    ) {
        match cur {
            Some(c) => {
                let i = Some(ent(c));
                if (h.is_none() && m.is_none())
                    || (h.is_none() && m.is_some() && same(i, m))
                    || (h.is_some() && m.is_some() && same(h, m))
                    || (h.is_some() && m.is_some() && same(i, m))
                {
                    self.keep(c);
                } else if h.is_some() && m.is_none() && same(i, h) {
                    self.deleted(path, cur);
                } else if let (Some(_), Some(m)) = (h, m)
                    && same(i, h)
                {
                    self.merged(path, m, cur);
                } else {
                    self.reject(path);
                }
            }
            None => match m {
                Some(m) => {
                    if h.is_some() && !initial {
                        if !same(h, Some(m)) {
                            self.reject(path);
                        }
                        return;
                    }
                    self.merged(path, m, None);
                }
                None => self.deleted(path, None),
            },
        }
    }

    fn threeway(
        &mut self,
        path: &[u8],
        index: Option<&IndexEntry>,
        base: Option<Ent>,
        head: Option<Ent>,
        remote: Option<Ent>,
    ) {
        let idx = index.map(ent);
        let (mut head_match, mut remote_match) = (false, false);
        if !same(remote, head) {
            head_match = same(base, head);
            remote_match = same(base, remote);
        }
        let no_anc = base.is_none();
        if let Some(r) = remote
            && head_match
            && !remote_match
        {
            if idx.is_some() && !same(idx, remote) && !same(idx, head) {
                return self.reject(path);
            }
            return self.merged(path, r, index);
        }
        if idx.is_some() && !same(idx, head) {
            return self.reject(path);
        }
        if let Some(h) = head
            && (same(head, remote) || (remote_match && !head_match))
        {
            return self.merged(path, h, index);
        }
        if head.is_none() && remote.is_none() && no_anc {
            return;
        }
        if self.o.aggressive {
            let (head_deleted, remote_deleted) = (head.is_none(), remote.is_none());
            if (head_deleted && (remote_deleted || remote_match)) || (remote_deleted && head_match)
            {
                if index.is_some() {
                    self.deleted(path, index);
                }
                return;
            }
            if no_anc
                && let (Some(h), Some(_)) = (head, remote)
                && same(head, remote)
            {
                return self.merged(path, h, index);
            }
        }
        if let Some(i) = index
            && !self.uptodate(i)
        {
            return;
        }
        if let Some(b) = base
            && (!head_match || !remote_match)
        {
            self.keep_stage(path, b, 1);
        }
        if let Some(h) = head {
            self.keep_stage(path, h, 2);
        }
        if let Some(r) = remote {
            self.keep_stage(path, r, 3);
        }
    }
}

/// `git read-tree`: read trees into the index, merging with -m or --reset,
/// under a folder with `prefix`, and update the working tree with -u.
pub fn read_tree(git_dir: &Path, trees: &[String], o: &ReadTreeOpts) -> Result<(), GitError> {
    let fatal = |m: &str| Err(other(m.to_owned()));
    if [o.merge, o.reset, o.prefix.is_some()]
        .iter()
        .filter(|b| **b)
        .count()
        > 1
    {
        return fatal("Which one? -m, --reset, or --prefix?");
    }
    let merge = o.merge || o.reset || o.prefix.is_some();
    if o.update && o.index_only {
        return fatal("-u and -i at the same time makes no sense");
    }
    if (o.update || o.index_only) && !merge {
        let flag = if o.update { "-u" } else { "-i" };
        return Err(other(format!(
            "{flag} is meaningless without -m, --reset, or --prefix"
        )));
    }
    if o.empty && !trees.is_empty() {
        return fatal("passing trees as arguments contradicts --empty");
    }
    if trees.is_empty() && !o.empty {
        return fatal("you must specify at least one tree to merge");
    }
    if merge && trees.len() > 3 {
        return fatal("rgit read-tree merges at most three trees");
    }
    if o.prefix.is_some() && trees.len() > 1 {
        return fatal("--prefix takes one tree");
    }
    let (repo, mut index) = open(git_dir)?;
    let old: Vec<IndexEntry> = index.iter().collect();
    if merge && !o.reset && old.iter().any(|e| stage(e) != 0) {
        return fatal("You need to resolve your current index first");
    }
    let maps = trees
        .iter()
        .map(|t| flatten(&repo, t))
        .collect::<Result<Vec<_>, _>>()?;
    let mut u = Unpack {
        o,
        repo: &repo,
        top: repo.workdir().map(Path::to_path_buf),
        result: Vec::new(),
        update: Vec::new(),
        remove: Vec::new(),
        errors: Vec::new(),
    };
    if !merge {
        let mut all = BTreeMap::new();
        for m in &maps {
            all.extend(m.iter().map(|(p, e)| (p.clone(), *e)));
        }
        for (p, e) in &all {
            u.keep_stage(p, *e, 0);
        }
    } else if let Some(prefix) = &o.prefix {
        let dir = format!("{}/", prefix.trim_end_matches('/'));
        u.result = old.iter().map(dup).collect();
        let taken: BTreeSet<&[u8]> = old.iter().map(|e| &e.path[..]).collect();
        for (p, e) in &maps[0] {
            let mut path = dir.as_bytes().to_vec();
            path.extend_from_slice(p);
            if taken.contains(&path[..]) {
                let name = Unpack::path(&path);
                u.errors.push(format!(
                    "error: Entry '{name}' overlaps with '{name}'.  Cannot bind."
                ));
                break;
            }
            if u.absent(&path) {
                u.keep_stage(&path, *e, 0);
                u.update.push(Unpack::path(&path));
            }
        }
    } else {
        let current: BTreeMap<&[u8], &IndexEntry> = old
            .iter()
            .filter(|e| stage(e) == 0)
            .map(|e| (&e.path[..], e))
            .collect();
        let mut paths: BTreeSet<&[u8]> = current.keys().copied().collect();
        for m in &maps {
            paths.extend(m.keys().map(Vec::as_slice));
        }
        let initial = old.is_empty() && !index.path().is_some_and(Path::exists);
        for p in paths {
            let cur = current.get(p).copied();
            let at = |i: usize| maps[i].get(p).copied();
            match maps.len() {
                1 => u.oneway(p, cur, at(0)),
                2 => u.twoway(p, cur, at(0), at(1), initial),
                _ => u.threeway(p, cur, at(0), at(1), at(2)),
            }
        }
    }
    if !u.errors.is_empty() {
        return Err(other(u.errors.join("\n")));
    }
    if o.dry_run {
        return Ok(());
    }
    let (result, update, remove) = (u.result, u.update, u.remove);
    index.clear()?;
    for e in &result {
        index.add(e)?;
    }
    index.write()?;
    if o.update {
        let top = top(&repo)?;
        for p in &remove {
            remove_worktree_file(&top, p);
        }
        checkout_paths(&repo, &update, None, true)?;
    }
    Ok(())
}

/// `git checkout-index`'s options.
#[derive(Default, Clone)]
pub struct CheckoutIndexOpts {
    pub all: bool,
    pub force: bool,
    pub update_index: bool,
    pub quiet: bool,
    pub no_create: bool,
    pub prefix: Option<String>,
}

/// `git checkout-index`: write index files into the working tree (or under
/// `--prefix`). `paths` are relative to the folder `cwd`.
pub fn checkout_index(
    git_dir: &Path,
    cwd: &str,
    paths: &[String],
    o: &CheckoutIndexOpts,
) -> Result<Report, GitError> {
    let (repo, index) = open(git_dir)?;
    let top = top(&repo)?;
    let mut report = Report::default();
    let entries: Vec<IndexEntry> = index.iter().collect();
    let mut chosen: Vec<&IndexEntry> = Vec::new();
    if o.all {
        chosen.extend(
            entries
                .iter()
                .filter(|e| stage(e) == 0 && e.flags_extended & SKIP_WORKTREE == 0),
        );
    }
    for p in paths {
        let name = from_top(cwd, p)?;
        match entries
            .iter()
            .find(|e| stage(e) == 0 && e.path == name.as_bytes())
        {
            Some(e) => chosen.push(e),
            None => {
                if !o.quiet {
                    report
                        .err
                        .push_str(&format!("git checkout-index: {name} is not in the cache\n"));
                }
                report.failed = true;
            }
        }
    }
    let (base, target) = match &o.prefix {
        Some(p) if !p.ends_with('/') => {
            return Err(other(
                "rgit checkout-index: --prefix must name a folder (end in /)",
            ));
        }
        Some(p) => (p.clone(), Some(std::path::absolute(p)?)),
        None => (String::new(), None),
    };
    let root = target.clone().unwrap_or_else(|| top.clone());
    let mut write = Vec::new();
    for e in chosen {
        let name = Unpack::path(&e.path);
        if std::fs::symlink_metadata(root.join(&name)).is_ok() {
            let clean =
                target.is_none() && matches!(worktree_state(&repo, &top, e)?, Some((true, _)));
            if clean && !o.force {
                continue;
            }
            if !o.force {
                if !o.quiet {
                    report
                        .err
                        .push_str(&format!("{base}{name} already exists, no checkout\n"));
                }
                report.failed = true;
                continue;
            }
        } else if o.no_create {
            continue;
        }
        write.push(name);
    }
    write.dedup();
    checkout_paths(
        &repo,
        &write,
        target.as_deref(),
        o.update_index && target.is_none(),
    )?;
    Ok(report)
}

/// Refresh the index's stat data from the working tree as git's
/// refresh_index does, listing files that differ.
fn refresh(
    repo: &Repository,
    index: &mut Index,
    really: bool,
    quiet: bool,
    unmerged: bool,
    ignore_missing: bool,
    report: &mut Report,
) -> Result<(), GitError> {
    let top = top(repo)?;
    let entries: Vec<IndexEntry> = index.iter().collect();
    let mut last: Option<&[u8]> = None;
    for e in &entries {
        let name = Unpack::path(&e.path);
        if stage(e) != 0 {
            if !unmerged && last != Some(&e.path[..]) {
                report.out.push_str(&format!("{name}: needs merge\n"));
                report.failed = true;
            }
            last = Some(&e.path);
            continue;
        }
        if e.flags_extended & SKIP_WORKTREE != 0 || (!really && e.flags & VALID != 0) {
            continue;
        }
        match worktree_state(repo, &top, e)? {
            Some((true, meta)) => {
                let mut fresh = dup(e);
                fill_stat(&mut fresh, &meta);
                index.add(&fresh)?;
            }
            None if ignore_missing => {}
            _ if quiet => {}
            _ => {
                report.out.push_str(&format!("{name}: needs update\n"));
                report.failed = true;
            }
        }
    }
    Ok(())
}

/// Stage 0 for `path` replaces its conflict stages, as git's
/// add_index_entry does.
fn put(index: &mut Index, e: &IndexEntry) -> Result<(), GitError> {
    let path = Path::new(std::str::from_utf8(&e.path).map_err(|_| other("non-UTF-8 path"))?);
    if stage(e) == 0 {
        for s in 1..=3 {
            let _ = index.remove(path, s);
        }
    }
    index.add(e)?;
    Ok(())
}

fn remove_all(index: &mut Index, path: &str) {
    for s in 0..=3 {
        let _ = index.remove(Path::new(path), s);
    }
}

fn has(index: &Index, path: &str) -> bool {
    (0..=3).any(|s| index.get_path(Path::new(path), s).is_some())
}

/// `git update-index`: options apply to the paths after them, in order, as
/// in git. `cwd` is the current folder under the top; `stdin` reads
/// standard input for --stdin and --index-info.
pub fn update_index(
    git_dir: &Path,
    cwd: &str,
    args: &[String],
    stdin: &mut dyn FnMut() -> std::io::Result<Vec<u8>>,
) -> Result<Report, GitError> {
    let (repo, mut index) = open(git_dir)?;
    let mut report = Report::default();
    let (mut add, mut remove, mut force_remove, mut info_only) = (false, false, false, false);
    let (mut quiet, mut verbose, mut unmerged, mut ignore_missing, mut z) =
        (false, false, false, false, false);
    let mut chmod: Option<bool> = None;
    // --[no-]assume-unchanged, then --[no-]skip-worktree: git marks with the first set.
    let (mut mark_valid, mut mark_skip): (Option<bool>, Option<bool>) = (None, None);
    let mut read_paths = false;
    let mut only_paths = false;
    let mut paths_from_args: Vec<String> = Vec::new();
    let mut i = 0;
    let last = |i: usize, opt: &str| {
        if i + 1 != args.len() {
            Err(other(format!(
                "error: option '{opt}' must be the last argument"
            )))
        } else {
            Ok(())
        }
    };
    macro_rules! one {
        ($path:expr) => {
            update_one(
                &repo,
                &mut index,
                $path,
                &State {
                    add,
                    remove,
                    force_remove,
                    info_only,
                    verbose,
                    chmod,
                    mark: mark_valid
                        .map(|s| (true, s))
                        .or(mark_skip.map(|s| (false, s))),
                },
                &mut report,
            )?
        };
    }
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        if only_paths || !a.starts_with('-') || a == "-" {
            let path = from_top(cwd, a)?;
            paths_from_args.push(path.clone());
            one!(&path);
            continue;
        }
        match a {
            "--" => only_paths = true,
            "--add" => add = true,
            "--remove" => remove = true,
            "--force-remove" => force_remove = true,
            "--replace" => {}
            "-q" => quiet = true,
            "--info-only" => info_only = true,
            "--verbose" => verbose = true,
            "--unmerged" => unmerged = true,
            "--ignore-missing" => ignore_missing = true,
            "-z" => z = true,
            "--refresh" | "--really-refresh" => refresh(
                &repo,
                &mut index,
                a == "--really-refresh",
                quiet,
                unmerged,
                ignore_missing,
                &mut report,
            )?,
            "--chmod=+x" => chmod = Some(true),
            "--chmod=-x" => chmod = Some(false),
            "--assume-unchanged" => mark_valid = Some(true),
            "--no-assume-unchanged" => mark_valid = Some(false),
            "--skip-worktree" => mark_skip = Some(true),
            "--no-skip-worktree" => mark_skip = Some(false),
            "--cacheinfo" => {
                let (mode, id, path) = match args.get(i).and_then(|s| {
                    let mut parts = s.splitn(3, ',');
                    Some((parts.next()?, parts.next()?, parts.next()?))
                }) {
                    Some((m, o, p)) => {
                        i += 1;
                        (m.to_owned(), o.to_owned(), p.to_owned())
                    }
                    None if i + 3 <= args.len() => {
                        i += 3;
                        (
                            args[i - 3].clone(),
                            args[i - 2].clone(),
                            args[i - 1].clone(),
                        )
                    }
                    None => {
                        return Err(other(
                            "error: option 'cacheinfo' expects <mode>,<sha1>,<path>",
                        ));
                    }
                };
                let (Ok(mode), Ok(id)) = (u32::from_str_radix(&mode, 8), Oid::from_str(&id)) else {
                    return Err(other(
                        "error: option 'cacheinfo' expects <mode>,<sha1>,<path>",
                    ));
                };
                if !valid_path(path.as_bytes()) {
                    report.err.push_str(&format!("Ignoring path {path}\n"));
                    continue;
                }
                if !add && !has(&index, &path) {
                    return Err(other(format!(
                        "error: {path}: cannot add to the index - missing --add option?\n\
                         fatal: git update-index: --cacheinfo cannot add {path}"
                    )));
                }
                put(&mut index, &entry(path.as_bytes(), mode, id, 0))?;
                if verbose {
                    report.out.push_str(&format!("add '{path}'\n"));
                }
            }
            "--index-info" => {
                last(i - 1, "index-info")?;
                index_info(&mut index, &stdin()?, z, &mut report)?;
            }
            "--stdin" => {
                last(i - 1, "stdin")?;
                read_paths = true;
            }
            _ => {
                return Err(other(format!(
                    "error: unknown option `{}'",
                    a.trim_start_matches('-')
                )));
            }
        }
    }
    if read_paths {
        let input = stdin()?;
        for rec in input.split(|b| *b == if z { 0 } else { b'\n' }) {
            if rec.is_empty() {
                continue;
            }
            let text = String::from_utf8_lossy(rec);
            let text = if !z && text.starts_with('"') {
                crate::apply::unquote(&text)
            } else {
                text.into_owned()
            };
            let path = from_top(cwd, &text)?;
            one!(&path);
        }
    }
    index.write()?;
    Ok(report)
}

struct State {
    add: bool,
    remove: bool,
    force_remove: bool,
    info_only: bool,
    verbose: bool,
    chmod: Option<bool>,
    mark: Option<(bool, bool)>,
}

/// One path argument of update-index, as git's update_one and chmod_path.
fn update_one(
    repo: &Repository,
    index: &mut Index,
    path: &str,
    s: &State,
    report: &mut Report,
) -> Result<(), GitError> {
    let unable = |why: String| {
        other(format!(
            "error: {why}\nfatal: Unable to process path {path}"
        ))
    };
    if !valid_path(path.as_bytes()) {
        report.err.push_str(&format!("Ignoring path {path}\n"));
        return Ok(());
    }
    if let Some((valid, set)) = s.mark {
        let Some(mut e) = index.get_path(Path::new(path), 0) else {
            return Err(other(format!("Unable to mark file {path}")));
        };
        let (field, bit) = if valid {
            (&mut e.flags, VALID)
        } else {
            (&mut e.flags_extended, SKIP_WORKTREE)
        };
        if set {
            *field |= bit;
        } else {
            *field &= !bit;
        }
        index.add(&e)?;
        return Ok(());
    }
    if s.force_remove {
        remove_all(index, path);
        if s.verbose {
            report.out.push_str(&format!("remove '{path}'\n"));
        }
        return Ok(());
    }
    let top = top(repo)?;
    let existing = index.get_path(Path::new(path), 0);
    if existing
        .as_ref()
        .is_some_and(|e| e.flags_extended & SKIP_WORKTREE != 0)
    {
        return Ok(());
    }
    match std::fs::symlink_metadata(top.join(path)) {
        Err(_) => {
            if !s.remove {
                return Err(unable(format!(
                    "{path}: does not exist and --remove not passed"
                )));
            }
            remove_all(index, path);
        }
        Ok(meta) if meta.is_dir() && existing.as_ref().is_none_or(|e| e.mode != 0o160000) => {
            return Err(unable(format!(
                "{path}: is a directory - add individual files instead"
            )));
        }
        Ok(_) => {
            if !s.add && !has(index, path) {
                return Err(unable(format!(
                    "{path}: cannot add to the index - missing --add option?"
                )));
            }
            if s.info_only {
                let full = top.join(path);
                let data = crate::plumbing::clean(repo, path, &std::fs::read(&full)?)?;
                let mut e = index
                    .get_path(Path::new(path), 0)
                    .unwrap_or_else(|| entry(path.as_bytes(), 0o100644, Oid::ZERO_SHA1, 0));
                e.id = Oid::hash_object(ObjectType::Blob, &data)?;
                let meta = std::fs::symlink_metadata(&full)?;
                #[cfg(unix)]
                if repo
                    .config()
                    .and_then(|c| c.get_bool("core.filemode"))
                    .unwrap_or(true)
                {
                    let exec =
                        std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
                    e.mode = if exec { 0o100755 } else { 0o100644 };
                }
                fill_stat(&mut e, &meta);
                put(index, &e)?;
            } else {
                index.add_path(Path::new(path))?;
            }
        }
    }
    if s.verbose {
        report.out.push_str(&format!("add '{path}'\n"));
    }
    if let Some(exec) = s.chmod {
        let flip = if exec { '+' } else { '-' };
        match index.get_path(Path::new(path), 0) {
            Some(mut e) if e.mode & 0o170000 == 0o100000 => {
                e.mode = if exec { 0o100755 } else { 0o100644 };
                index.add(&e)?;
                if s.verbose {
                    report.out.push_str(&format!("chmod {flip}x '{path}'\n"));
                }
            }
            _ => {
                return Err(other(format!(
                    "git update-index: cannot chmod {flip}x '{path}'"
                )));
            }
        }
    }
    Ok(())
}

/// `update-index --index-info`: `mode sha1 TAB path`, `mode type sha1 TAB
/// path` or `mode sha1 stage TAB path` lines; mode 0 removes the path.
fn index_info(
    index: &mut Index,
    input: &[u8],
    z: bool,
    report: &mut Report,
) -> Result<(), GitError> {
    for rec in input.split(|b| *b == if z { 0 } else { b'\n' }) {
        if rec.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(rec);
        let bad = || other(format!("malformed index info {text}"));
        let (head, path) = text.split_once('\t').ok_or_else(bad)?;
        let path = if !z && path.starts_with('"') {
            crate::apply::unquote(path)
        } else {
            path.to_owned()
        };
        let fields: Vec<&str> = head.split(' ').collect();
        let (mode, id, stage) = match fields[..] {
            [m, o] => (m, o, 0),
            [m, o, s] if s.len() == 1 && ("0"..="3").contains(&s) => (m, o, s.parse().unwrap_or(0)),
            [m, _, o] => (m, o, 0),
            _ => return Err(bad()),
        };
        let mode = u32::from_str_radix(mode, 8).map_err(|_| bad())?;
        let id = Oid::from_str(id)
            .ok()
            .filter(|_| id.len() == 40)
            .ok_or_else(bad)?;
        if !valid_path(path.as_bytes()) {
            report.err.push_str(&format!("Ignoring path {path}\n"));
            continue;
        }
        if mode == 0 {
            remove_all(index, &path);
        } else {
            put(index, &entry(path.as_bytes(), mode, id, stage))?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn paths_normalize_like_git() {
    assert_eq!(from_top("d/", "../a").unwrap(), "a");
    assert_eq!(from_top("d/", "./x/y").unwrap(), "d/x/y");
    assert!(from_top("", "../a").is_err());
    assert!(!valid_path(b".git/config"));
    assert!(valid_path(b"a/b"));
}

fn dup(e: &IndexEntry) -> IndexEntry {
    IndexEntry {
        path: e.path.clone(),
        ..*e
    }
}
