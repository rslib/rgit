//! Partial clones: the objects a `--filter` keeps, the promisor packs that
//! hold what was fetched, and fetching what the filter left out, from the
//! promisor remote, when something reads it.

use std::collections::HashSet;
use std::ffi::c_void;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use git2::{Binding, ObjectType, Oid, Repository};
use libgit2_sys as raw;

use crate::error::GitError;
use crate::smart::{FetchRequest, Session};

/// A `--filter` spec: what of the trees and blobs a fetch leaves out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Filter {
    /// `blob:none`: every blob.
    BlobNone,
    /// `blob:limit=<n>`: the blobs of n bytes or more.
    BlobLimit(u64),
    /// `tree:<depth>`: the trees and blobs that deep or deeper.
    Tree(u64),
}

impl Filter {
    pub fn parse(spec: &str) -> Result<Self, GitError> {
        let bad = || GitError::Other(format!("invalid filter-spec '{spec}'"));
        if spec == "blob:none" {
            return Ok(Filter::BlobNone);
        }
        if let Some(n) = spec.strip_prefix("blob:limit=") {
            return size(n).map(Filter::BlobLimit).ok_or_else(bad);
        }
        if let Some(n) = spec.strip_prefix("tree:") {
            return n.parse().map(Filter::Tree).map_err(|_| bad());
        }
        Err(bad())
    }

    /// The spec as git writes it, sizes in bytes.
    pub fn spec(self) -> String {
        match self {
            Filter::BlobNone => "blob:none".to_owned(),
            Filter::BlobLimit(n) => format!("blob:limit={n}"),
            Filter::Tree(d) => format!("tree:{d}"),
        }
    }

    /// Whether a tree `depth` entries below the root tree is kept.
    fn keeps_tree(self, depth: u64) -> bool {
        !matches!(self, Filter::Tree(d) if depth >= d)
    }

    /// Whether a blob of `size` bytes, `depth` entries down, is kept.
    fn keeps_blob(self, depth: u64, size: u64) -> bool {
        match self {
            Filter::BlobNone => false,
            Filter::BlobLimit(n) => size < n,
            Filter::Tree(d) => depth < d,
        }
    }
}

/// A size as git_parse_ulong reads it: digits with an optional k, m or g.
fn size(s: &str) -> Option<u64> {
    let (digits, unit) = match s.char_indices().last()? {
        (i, c) if c.is_ascii_alphabetic() => (&s[..i], c.to_ascii_lowercase()),
        _ => (s, ' '),
    };
    let n: u64 = digits.parse().ok()?;
    n.checked_mul(match unit {
        ' ' => 1,
        'k' => 1 << 10,
        'm' => 1 << 20,
        'g' => 1 << 30,
        _ => return None,
    })
}

/// A pack from `src` of `commits` with the trees and blobs `filter` keeps,
/// and of `extra` (tags, or objects asked for by id, which a filter never
/// leaves out, though it does what is under a tree), leaving out what
/// `have` has. Empty when there is nothing to send.
pub(crate) fn local_pack(
    src: &Repository,
    commits: &HashSet<Oid>,
    extra: &[Oid],
    filter: Option<Filter>,
    have: &git2::Odb<'_>,
) -> Result<Vec<u8>, GitError> {
    let mut walk = Walk {
        src,
        odb: src.odb()?,
        pack: src.packbuilder()?,
        seen: HashSet::new(),
        have,
        filter,
    };
    for &c in commits {
        if walk.add(c)? {
            walk.tree(src.find_commit(c)?.tree_id(), 0)?;
        }
    }
    for &id in extra {
        match walk.odb.read_header(id)?.1 {
            ObjectType::Tree => walk.tree(id, 0)?,
            ObjectType::Tag => {
                walk.add(id)?;
                let target = src.find_tag(id)?.target_id();
                match walk.odb.read_header(target)?.1 {
                    ObjectType::Tree => walk.tree(target, 0)?,
                    _ => {
                        walk.add(target)?;
                    }
                }
            }
            _ => {
                walk.add(id)?;
            }
        }
    }
    let mut out = Vec::new();
    if walk.pack.object_count() > 0 {
        walk.pack.foreach(|buf| {
            out.extend_from_slice(buf);
            true
        })?;
    }
    Ok(out)
}

struct Walk<'a> {
    src: &'a Repository,
    odb: git2::Odb<'a>,
    pack: git2::PackBuilder<'a>,
    seen: HashSet<Oid>,
    have: &'a git2::Odb<'a>,
    filter: Option<Filter>,
}

impl Walk<'_> {
    /// Put `id` in the pack unless it is there or here already.
    fn add(&mut self, id: Oid) -> Result<bool, GitError> {
        if !self.seen.insert(id) || self.have.exists(id) {
            return Ok(false);
        }
        self.pack.insert_object(id, None)?;
        Ok(true)
    }

    fn tree(&mut self, id: Oid, depth: u64) -> Result<(), GitError> {
        if self.filter.is_some_and(|f| !f.keeps_tree(depth)) || !self.add(id)? {
            return Ok(());
        }
        let tree = self.src.find_tree(id)?;
        for e in tree.iter() {
            match e.kind() {
                Some(ObjectType::Tree) => self.tree(e.id(), depth + 1)?,
                Some(ObjectType::Blob) => {
                    let keep = match self.filter {
                        Some(f) => f.keeps_blob(depth + 1, self.odb.read_header(e.id())?.0 as u64),
                        None => true,
                    };
                    if keep {
                        self.add(e.id())?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Index the pack `write` sends into `repo`'s objects, marked a promisor
/// pack holding `promisor` (the refs it was fetched for) when that is given.
pub(crate) fn store<T>(
    repo: &Repository,
    promisor: Option<&str>,
    write: impl FnOnce(&mut dyn Write) -> Result<T, GitError>,
) -> Result<T, GitError> {
    let dir = repo.commondir().join("objects/pack");
    std::fs::create_dir_all(&dir)?;
    let odb = repo.odb()?;
    let mut sink = Counted(git2::Indexer::new(Some(&odb), &dir, 0, false)?, 0);
    let out = write(&mut sink)?;
    if sink.1 == 0 {
        return Ok(out);
    }
    let name = sink.0.commit()?;
    if let Some(refs) = promisor {
        std::fs::write(dir.join(format!("pack-{name}.promisor")), refs)?;
    }
    Ok(out)
}

/// A writer that counts what goes through it.
struct Counted<W>(W, usize);

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.0.write(buf)?;
        self.1 += n;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Make `remote` the promisor remote of `repo`, fetched through `spec`.
pub(crate) fn mark(repo: &Repository, remote: &str, spec: &str) -> Result<(), GitError> {
    let mut config = repo.config()?;
    config.set_i32("core.repositoryformatversion", 1)?;
    config.set_bool(&format!("remote.{remote}.promisor"), true)?;
    config.set_str(
        &format!("remote.{remote}.partialclonefilter"),
        &Filter::parse(spec)?.spec(),
    )?;
    Ok(())
}

/// The remote `repo` fetches what it lacks from, and its URL.
pub(crate) fn remote(repo: &Repository) -> Option<(String, String)> {
    let config = repo.config().ok()?;
    let mut name = config.get_string("extensions.partialclone").ok();
    if name.is_none() {
        let entries = config.entries(Some(r"^remote\..*\.promisor$")).ok()?;
        entries
            .for_each(|e| {
                if name.is_none()
                    && e.value()
                        .ok()
                        .and_then(|v| git2::Config::parse_bool(v).ok())
                        == Some(true)
                {
                    let key = e.name().unwrap_or_default();
                    name = key
                        .strip_prefix("remote.")
                        .and_then(|k| k.strip_suffix(".promisor"))
                        .map(str::to_owned);
                }
            })
            .ok()?;
    }
    let name = name?;
    let url = config.get_string(&format!("remote.{name}.url")).ok()?;
    Some((name, url))
}

/// The filter the promisor remote `name` was fetched through, if it is one.
pub(crate) fn remote_filter(repo: &Repository, name: &str) -> Option<String> {
    let config = repo.config().ok()?;
    let promisor = config
        .get_bool(&format!("remote.{name}.promisor"))
        .unwrap_or(false)
        || config
            .get_string("extensions.partialclone")
            .is_ok_and(|n| n == name);
    promisor.then(|| {
        config
            .get_string(&format!("remote.{name}.partialclonefilter"))
            .ok()
    })?
}

/// Fetch `ids` from the promisor remote into `repo`, each with what is under
/// it but the blobs, into a promisor pack of its own.
pub(crate) fn fetch_missing(repo: &Repository, ids: &[Oid]) -> Result<(), GitError> {
    if ids.is_empty() {
        return Ok(());
    }
    let Some((_, url)) = remote(repo) else {
        return Ok(());
    };
    if crate::is_local_url(&url) {
        let path = Path::new(url.strip_prefix("file://").unwrap_or(&url));
        let path = repo.workdir().unwrap_or(repo.path()).join(path);
        let src = Repository::open(path)?;
        let pack = local_pack(
            &src,
            &HashSet::new(),
            ids,
            Some(Filter::BlobNone),
            &repo.odb()?,
        )?;
        return store(repo, Some(""), |w| Ok(w.write_all(&pack)?));
    }
    let mut session = Session::open(&url, Some(&repo.config()?))?;
    let req = FetchRequest {
        wants: ids.to_vec(),
        filter: session.filters().then(|| "blob:none".to_owned()),
        ..Default::default()
    };
    store(repo, Some(""), |w| session.fetch(&req, w, &|_| {}))?;
    session.finish()
}

/// Fetch at once what checking out `tree` needs and `repo` lacks (just the
/// top-level files when `top_only`), as git does before a checkout, rather
/// than one object at a time as the checkout reads them.
pub(crate) fn prefetch(repo: &Repository, tree: Oid, top_only: bool) -> Result<(), GitError> {
    if !repo.odb()?.exists(tree) {
        fetch_missing(repo, &[tree])?;
    }
    // Opened again to see the pack just fetched.
    let repo = Repository::open(repo.path())?;
    let odb = repo.odb()?;
    let mut missing = Vec::new();
    let mut trees = vec![tree];
    while let Some(id) = trees.pop() {
        let Ok(t) = repo.find_tree(id) else {
            continue;
        };
        for e in t.iter() {
            match e.kind() {
                Some(ObjectType::Tree) if !top_only => trees.push(e.id()),
                Some(ObjectType::Blob) if !odb.exists(e.id()) => missing.push(e.id()),
                _ => {}
            }
        }
    }
    missing.sort();
    missing.dedup();
    fetch_missing(&repo, &missing)
}

/// Read objects `repo` lacks from its promisor remote when asked for them,
/// as git does in a partial clone, unless GIT_NO_LAZY_FETCH says not to.
pub(crate) fn install(repo: &Repository) {
    if remote(repo).is_none()
        || std::env::var("GIT_NO_LAZY_FETCH")
            .is_ok_and(|v| git2::Config::parse_bool(v).unwrap_or(false))
    {
        return;
    }
    let Ok(odb) = repo.odb() else {
        return;
    };
    // git lets the index and trees name the objects left out.
    git2::opts::strict_object_creation(false);
    let lazy = Box::new(Lazy {
        parent: unsafe { std::mem::zeroed() },
        gitdir: repo.path().to_path_buf(),
        failed: Mutex::new(HashSet::new()),
    });
    let lazy = Box::into_raw(lazy);
    unsafe {
        let parent = std::ptr::addr_of_mut!((*lazy).parent);
        if raw::git_odb_init_backend(parent, raw::GIT_ODB_BACKEND_VERSION) != 0 {
            drop(Box::from_raw(lazy));
            return;
        }
        (*parent).read = Some(lazy_read);
        (*parent).free = Some(lazy_free);
        // Last, after the packs and loose objects (and their refresh).
        if raw::git_odb_add_backend(odb.raw(), parent, 0) != 0 {
            drop(Box::from_raw(lazy));
        }
    }
}

/// An object database backend that fetches from the promisor remote.
#[repr(C)]
struct Lazy {
    parent: raw::git_odb_backend,
    gitdir: PathBuf,
    /// What a fetch did not bring, not asked for again.
    failed: Mutex<HashSet<Oid>>,
}

impl Lazy {
    fn object(&self, id: Oid) -> Option<(ObjectType, Vec<u8>)> {
        let read = || {
            let repo = Repository::open(&self.gitdir).ok()?;
            let odb = repo.odb().ok()?;
            let obj = odb.read(id).ok()?;
            Some((obj.kind(), obj.data().to_vec()))
        };
        // Written since the repository was opened, perhaps by another fetch.
        if let Some(found) = read() {
            return Some(found);
        }
        let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        if failed.contains(&id) {
            return None;
        }
        let repo = Repository::open(&self.gitdir).ok()?;
        let _ = fetch_missing(&repo, &[id]);
        let found = read();
        if found.is_none() {
            failed.insert(id);
        }
        found
    }
}

extern "C" fn lazy_read(
    data: *mut *mut c_void,
    len: *mut usize,
    kind: *mut raw::git_object_t,
    backend: *mut raw::git_odb_backend,
    oid: *const raw::git_oid,
) -> std::ffi::c_int {
    let found = std::panic::catch_unwind(|| {
        // SAFETY: libgit2 hands back the backend `install` made, and an oid.
        let lazy = unsafe { &*(backend as *const Lazy) };
        lazy.object(unsafe { Oid::from_raw(oid) })
    });
    let Ok(Some((k, bytes))) = found else {
        return raw::GIT_ENOTFOUND as std::ffi::c_int;
    };
    unsafe {
        let buf = raw::git_odb_backend_data_alloc(backend, bytes.len());
        if buf.is_null() {
            return -1;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, bytes.len());
        *data = buf;
        *len = bytes.len();
        *kind = k.raw();
    }
    0
}

extern "C" fn lazy_free(backend: *mut raw::git_odb_backend) {
    // SAFETY: `install` made it with Box::into_raw; libgit2 frees it once.
    drop(unsafe { Box::from_raw(backend as *mut Lazy) });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_specs() {
        assert_eq!(Filter::parse("blob:none").unwrap(), Filter::BlobNone);
        assert_eq!(
            Filter::parse("blob:limit=1k").unwrap(),
            Filter::BlobLimit(1024)
        );
        assert_eq!(Filter::parse("tree:0").unwrap(), Filter::Tree(0));
        assert!(Filter::parse("bogus").is_err());
        assert!(Filter::parse("blob:limit=1x").is_err());
        assert!(Filter::Tree(1).keeps_tree(0) && !Filter::Tree(1).keeps_blob(1, 0));
        assert!(Filter::BlobLimit(10).keeps_blob(3, 9) && !Filter::BlobLimit(10).keeps_blob(3, 10));
    }
}
