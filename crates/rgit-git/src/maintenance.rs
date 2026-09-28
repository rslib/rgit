//! Object and ref upkeep as git does it: reachability, prune, repack (with
//! cruft packs), pack-refs, reflog expire/delete, rerere gc, worktree prune,
//! gc and its --auto heuristics.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use git2::{ObjectType, Oid, Repository};

use crate::GitError;

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn mtime(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// An expiry date from config or a flag, as a cutoff time.
pub(crate) fn cutoff(value: &str) -> Result<i64, GitError> {
    crate::expiry_date(value)
        .ok_or_else(|| GitError::Other(format!("malformed expiration date '{value}'")))
}

fn cfg(repo: &Repository) -> Option<git2::Config> {
    crate::config::open_config(repo, crate::ConfigScope::Any, false).ok()
}

pub(crate) fn cfg_str(repo: &Repository, key: &str) -> Option<String> {
    cfg(repo)?.get_string(key).ok()
}

pub(crate) fn cfg_int(repo: &Repository, key: &str, default: i64) -> i64 {
    cfg(repo)
        .and_then(|c| c.get_i64(key).ok())
        .unwrap_or(default)
}

pub(crate) fn cfg_bool(repo: &Repository, key: &str, default: bool) -> bool {
    cfg(repo)
        .and_then(|c| c.get_bool(key).ok())
        .unwrap_or(default)
}

fn objects_dir(repo: &Repository) -> PathBuf {
    repo.commondir().join("objects")
}

/// Every loose object: id, path and mtime.
pub(crate) fn loose_objects(repo: &Repository) -> Vec<(Oid, PathBuf, i64)> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(objects_dir(repo)) else {
        return out;
    };
    for d in dirs.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        if name.len() != 2 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
            let rest = f.file_name().to_string_lossy().into_owned();
            if let (Ok(oid), Ok(meta)) = (Oid::from_str(&format!("{name}{rest}")), f.metadata())
                && rest.len() == 38
            {
                out.push((oid, f.path(), mtime(&meta)));
            }
        }
    }
    out.sort();
    out
}

/// A pack in objects/pack.
#[derive(Debug, Clone)]
pub(crate) struct Pack {
    /// `pack-<hash>`, without the extension.
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: i64,
    pub keep: bool,
    pub cruft: bool,
}

impl Pack {
    fn file(&self, ext: &str) -> PathBuf {
        self.path.with_extension(ext)
    }

    /// The ids the pack's .idx lists, sorted.
    pub fn ids(&self) -> Vec<Oid> {
        idx_ids(&self.file("idx"))
    }
}

pub(crate) fn idx_ids(path: &Path) -> Vec<Oid> {
    let Ok(idx) = std::fs::read(path) else {
        return Vec::new();
    };
    let (fanout, stride, first) = if idx.starts_with(b"\xfftOc") {
        (8, 20, 8 + 1024)
    } else {
        (0, 24, 1024)
    };
    let n = idx.get(fanout + 1020..fanout + 1024).map_or(0, |b| {
        u32::from_be_bytes(b.try_into().unwrap_or_default()) as usize
    });
    (0..n)
        .filter_map(|i| {
            let at = first + i * stride + if stride == 24 { 4 } else { 0 };
            Oid::from_bytes(idx.get(at..at + 20)?).ok()
        })
        .collect()
}

pub(crate) fn packs(repo: &Repository) -> Vec<Pack> {
    let dir = objects_dir(repo).join("pack");
    let mut out: Vec<Pack> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()? != "pack" || !path.with_extension("idx").exists() {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some(Pack {
                name: path.file_stem()?.to_string_lossy().into_owned(),
                size: meta.len(),
                mtime: mtime(&meta),
                keep: path.with_extension("keep").exists(),
                cruft: path.with_extension("mtimes").exists(),
                path,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// One index entry: path, mode and id.
pub(crate) struct IndexEntry {
    pub path: String,
    pub mode: u32,
    pub oid: Oid,
}

/// Parse the index file at `path`: its entries, and the cache-tree's valid
/// trees in pre-order (git keeps both alive).
pub(crate) fn read_index(path: &Path) -> (Vec<IndexEntry>, Vec<Oid>) {
    let mut entries = Vec::new();
    let mut trees = Vec::new();
    let Ok(data) = std::fs::read(path) else {
        return (entries, trees);
    };
    if data.len() < 12 || &data[..4] != b"DIRC" {
        return (entries, trees);
    }
    let be32 = |at: usize| {
        data.get(at..at + 4)
            .map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap_or_default()))
    };
    let version = be32(4);
    let count = be32(8) as usize;
    let mut at = 12;
    let mut last: Vec<u8> = Vec::new();
    for _ in 0..count {
        let Some(fixed) = data.get(at..at + 62) else {
            return (entries, trees);
        };
        let mode = be32(at + 24);
        let oid = Oid::from_bytes(&fixed[40..60]).unwrap_or(Oid::ZERO_SHA1);
        let flags = u16::from_be_bytes([fixed[60], fixed[61]]);
        let mut p = at + 62;
        if version >= 3 && flags & 0x4000 != 0 {
            p += 2;
        }
        let path = if version == 4 {
            // A varint of bytes to drop from the last path, then the rest.
            let mut drop: usize = 0;
            loop {
                let Some(&b) = data.get(p) else {
                    return (entries, trees);
                };
                p += 1;
                drop = (drop << 7) | (b & 0x7f) as usize;
                if b & 0x80 == 0 {
                    break;
                }
                drop += 1;
            }
            let Some(nul) = data[p.min(data.len())..].iter().position(|b| *b == 0) else {
                return (entries, trees);
            };
            let mut path = last[..last.len().saturating_sub(drop)].to_vec();
            path.extend_from_slice(&data[p..p + nul]);
            at = p + nul + 1;
            path
        } else {
            let Some(nul) = data[p.min(data.len())..].iter().position(|b| *b == 0) else {
                return (entries, trees);
            };
            let path = data[p..p + nul].to_vec();
            let len = p - at + nul;
            at += (len + 8) & !7;
            path
        };
        entries.push(IndexEntry {
            path: String::from_utf8_lossy(&path).into_owned(),
            mode,
            oid,
        });
        last = path;
    }
    // Extensions, up to the trailing checksum.
    while at + 8 <= data.len().saturating_sub(20) {
        let sig = &data[at..at + 4];
        let size = be32(at + 4) as usize;
        let body = data.get(at + 8..at + 8 + size).unwrap_or_default();
        if sig == b"TREE" {
            let mut q = 0;
            while q < body.len() {
                let Some(nul) = body[q..].iter().position(|b| *b == 0) else {
                    break;
                };
                q += nul + 1;
                let Some(nl) = body[q..].iter().position(|b| *b == b'\n') else {
                    break;
                };
                let header = String::from_utf8_lossy(&body[q..q + nl]).into_owned();
                q += nl + 1;
                let valid = header
                    .split(' ')
                    .next()
                    .and_then(|n| n.parse::<i64>().ok())
                    .is_some_and(|n| n >= 0);
                if valid {
                    if let Some(oid) = body.get(q..q + 20).and_then(|b| Oid::from_bytes(b).ok()) {
                        trees.push(oid);
                    }
                    q += 20;
                }
            }
        }
        at += 8 + size;
    }
    (entries, trees)
}

/// Every reflog file: the ref it belongs to and its path.
pub(crate) fn reflog_files(repo: &Repository) -> Vec<(String, PathBuf)> {
    let common = repo.commondir();
    let mut out = Vec::new();
    let logs = common.join("logs");
    if logs.join("HEAD").is_file() {
        out.push(("HEAD".to_owned(), logs.join("HEAD")));
    }
    for e in walkdir::WalkDir::new(logs.join("refs"))
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
    {
        if let Ok(rel) = e.path().strip_prefix(&logs) {
            out.push((
                rel.to_string_lossy().replace('\\', "/"),
                e.path().to_owned(),
            ));
        }
    }
    for wt in worktree_dirs(repo) {
        let head = wt.join("logs/HEAD");
        if head.is_file() {
            out.push(("HEAD".to_owned(), head));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The admin folders of linked worktrees (`<common>/worktrees/<name>`).
pub(crate) fn worktree_dirs(repo: &Repository) -> Vec<PathBuf> {
    std::fs::read_dir(repo.commondir().join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// One reflog line: old, new, time, and the raw line.
pub(crate) struct LogLine {
    pub old: Oid,
    pub new: Oid,
    pub time: i64,
    pub message: String,
    pub raw: String,
}

pub(crate) fn read_reflog(path: &Path) -> Vec<LogLine> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.split_inclusive('\n')
        .filter_map(|raw| {
            let (head, message) = raw.split_once('\t').unwrap_or((raw.trim_end(), ""));
            let mut words = head.splitn(3, ' ');
            let old = Oid::from_str(words.next()?).ok()?;
            let new = Oid::from_str(words.next()?).ok()?;
            let who = words.next()?;
            let after = who.rsplit_once('>').map_or(who, |(_, t)| t);
            let time = after.split_whitespace().next()?.parse().ok()?;
            Some(LogLine {
                old,
                new,
                time,
                message: message.to_owned(),
                raw: raw.to_owned(),
            })
        })
        .collect()
}

/// What keeps objects alive: refs, HEADs, (optionally) reflogs and indexes.
pub(crate) fn roots(repo: &Repository, reflogs: bool, index: bool) -> Vec<Oid> {
    let mut out = Vec::new();
    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            if let Some(oid) = r.target() {
                out.push(oid);
            }
        }
    }
    let mut heads = vec![repo.path().join("HEAD"), repo.commondir().join("HEAD")];
    let mut indexes = vec![repo.path().join("index"), repo.commondir().join("index")];
    for wt in worktree_dirs(repo) {
        heads.push(wt.join("HEAD"));
        indexes.push(wt.join("index"));
    }
    for head in heads {
        if let Ok(text) = std::fs::read_to_string(head)
            && let Ok(oid) = Oid::from_str(text.trim())
        {
            out.push(oid);
        }
    }
    if reflogs {
        for (_, path) in reflog_files(repo) {
            for l in read_reflog(&path) {
                out.extend([l.old, l.new].into_iter().filter(|o| !o.is_zero()));
            }
        }
    }
    if index {
        indexes.sort();
        indexes.dedup();
        for i in indexes {
            let (entries, trees) = read_index(&i);
            out.extend(entries.iter().filter(|e| e.mode != 0o160000).map(|e| e.oid));
            out.extend(trees);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Every object reachable from `start`, missing ones left out.
pub(crate) fn reachable(repo: &Repository, start: &[Oid], seen: &mut HashSet<Oid>) {
    let Ok(odb) = repo.odb() else {
        return;
    };
    let mut stack: Vec<Oid> = start
        .iter()
        .copied()
        .filter(|o| !seen.contains(o))
        .collect();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        let Ok(obj) = odb.read(oid) else {
            continue;
        };
        let mut push = |o: Oid| {
            if !seen.contains(&o) {
                stack.push(o);
            }
        };
        match obj.kind() {
            ObjectType::Commit | ObjectType::Tag => {
                for line in obj.data().split(|b| *b == b'\n') {
                    if line.is_empty() {
                        break;
                    }
                    let text = String::from_utf8_lossy(line);
                    for key in ["tree ", "parent ", "object "] {
                        if let Some(hex) = text.strip_prefix(key)
                            && let Ok(o) = Oid::from_str(hex)
                        {
                            push(o);
                        }
                    }
                }
            }
            ObjectType::Tree => {
                for (mode, id) in tree_entries(obj.data()) {
                    if mode != 0o160000 {
                        push(id);
                    }
                }
            }
            _ => {}
        }
    }
}

/// A raw tree's entries as (mode, id).
pub(crate) fn tree_entries(data: &[u8]) -> Vec<(u32, Oid)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let Some(sp) = data[at..].iter().position(|b| *b == b' ') else {
            break;
        };
        let mode =
            u32::from_str_radix(&String::from_utf8_lossy(&data[at..at + sp]), 8).unwrap_or(0);
        let Some(nul) = data[at..].iter().position(|b| *b == 0) else {
            break;
        };
        let id = at + nul + 1;
        let Some(oid) = data.get(id..id + 20).and_then(|b| Oid::from_bytes(b).ok()) else {
            break;
        };
        out.push((mode, oid));
        at = id + 20;
    }
    out
}

fn all_reachable(repo: &Repository) -> HashSet<Oid> {
    let mut seen = HashSet::new();
    reachable(repo, &roots(repo, true, true), &mut seen);
    seen
}

/// Write `data` as a loose object, stamped `when` (libgit2 skips objects
/// already packed).
fn write_loose(
    repo: &Repository,
    oid: Oid,
    kind: ObjectType,
    data: &[u8],
    when: i64,
) -> Result<(), GitError> {
    let hex = oid.to_string();
    let dir = objects_dir(repo).join(&hex[..2]);
    let path = dir.join(&hex[2..]);
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(format!("{} {}\0", kind.str(), data.len()).as_bytes())?;
    z.write_all(data)?;
    let tmp = dir.join(format!("tmp_obj_{}", std::process::id()));
    std::fs::write(&tmp, z.finish()?)?;
    std::fs::rename(&tmp, &path)?;
    set_mtime(&path, when);
    Ok(())
}

fn set_mtime(path: &Path, when: i64) {
    if let Ok(f) = std::fs::File::options()
        .write(true)
        .open(path)
        .or_else(|_| std::fs::File::open(path))
    {
        let _ = f.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(when.max(0) as u64));
    }
}

/// Remove loose objects that a pack also holds (git's prune-packed).
pub(crate) fn prune_packed(repo: &Repository, dry_run: bool) -> Vec<String> {
    let packed: HashSet<Oid> = packs(repo).iter().flat_map(Pack::ids).collect();
    let mut out = Vec::new();
    for (oid, path, _) in loose_objects(repo) {
        if packed.contains(&oid) {
            if dry_run {
                out.push(format!("rm -f {}", path.display()));
            } else {
                let _ = std::fs::remove_file(&path);
                if let Some(dir) = path.parent() {
                    let _ = std::fs::remove_dir(dir);
                }
            }
        }
    }
    out
}

/// `git prune`: delete unreachable loose objects older than `expire` (kept
/// when reachable from a recent one), then the loose copies of packed ones.
/// Returns `<id> <type>` for each (all with `dry_run` or `verbose`).
pub fn prune(
    repo: &Repository,
    expire: i64,
    dry_run: bool,
    verbose: bool,
) -> Result<Vec<String>, GitError> {
    let mut keep = all_reachable(repo);
    let loose = loose_objects(repo);
    let recent: Vec<Oid> = loose
        .iter()
        .filter(|(oid, _, t)| *t > expire && !keep.contains(oid))
        .map(|(oid, ..)| *oid)
        .chain(
            packs(repo)
                .iter()
                .filter(|p| p.mtime > expire)
                .flat_map(Pack::ids)
                .filter(|o| !keep.contains(o)),
        )
        .collect();
    reachable(repo, &recent, &mut keep);
    let odb = repo.odb()?;
    let mut out = Vec::new();
    for (oid, path, t) in &loose {
        if keep.contains(oid) || *t > expire {
            continue;
        }
        if dry_run || verbose {
            let kind = odb.read_header(*oid).map_or("unknown", |(_, k)| k.str());
            out.push(format!("{oid} {kind}"));
        }
        if !dry_run {
            std::fs::remove_file(path)?;
        }
    }
    prune_packed(repo, dry_run);
    if !dry_run {
        // Stale temporary files, and fan-out folders left empty.
        for d in std::fs::read_dir(objects_dir(repo))
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = d.file_name().to_string_lossy().into_owned();
            if name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
                    if f.file_name().to_string_lossy().starts_with("tmp_")
                        && f.metadata().is_ok_and(|m| mtime(&m) <= expire)
                    {
                        let _ = std::fs::remove_file(f.path());
                    }
                }
                let _ = std::fs::remove_dir(d.path());
            }
        }
    }
    Ok(out)
}

/// What `git repack` does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepackOptions {
    /// Everything in one pack (-a).
    pub all: bool,
    /// Like `all`, loosening unreachable packed objects on `delete` (-A).
    pub all_loosen: bool,
    /// Delete what the new pack makes redundant (-d).
    pub delete: bool,
    /// Keep unreachable objects in the new pack (-k).
    pub keep_unreachable: bool,
    /// Put unreachable objects in a cruft pack (--cruft).
    pub cruft: bool,
    /// With `cruft`, drop unreachable objects older than this instead.
    pub cruft_expiration: Option<i64>,
    /// With `all_loosen`, only loosen objects newer than this
    /// (--unpack-unreachable).
    pub unpack_unreachable: Option<i64>,
    /// Packs (`pack-<hash>.pack`) to leave as they are (--keep-pack).
    pub keep_pack: Vec<String>,
    /// Don't write objects/info/packs (-n).
    pub no_update_server_info: bool,
}

/// Pack exactly `ids` into a new pack; its `pack-<hash>` name, if any.
// ponytail: libgit2 picks deltas itself, so -f, -F, --window, --depth and
// --aggressive do not change the pack.
fn new_pack(repo: &Repository, ids: &[Oid]) -> Result<Option<String>, GitError> {
    if ids.is_empty() {
        return Ok(None);
    }
    let mut pb = repo.packbuilder()?;
    for id in ids {
        pb.insert_object(*id, None)?;
    }
    let dir = objects_dir(repo).join("pack");
    std::fs::create_dir_all(&dir)?;
    pb.write(&dir, 0)?;
    let name = pb
        .name()?
        .map(|n| format!("pack-{n}"))
        .ok_or_else(|| GitError::Other("the pack was not written".to_owned()))?;
    if cfg_bool(repo, "pack.writeReverseIndex", true) {
        write_rev(&dir.join(format!("{name}.pack")))?;
    }
    Ok(Some(name))
}

/// The objects of a v2 `.idx` with their pack offsets, in index order.
pub(crate) fn idx_offsets(idx_path: &Path) -> Vec<(Oid, u64)> {
    let Ok(idx) = std::fs::read(idx_path) else {
        return Vec::new();
    };
    if !idx.starts_with(b"\xfftOc") || idx.len() < 8 + 1024 {
        return Vec::new();
    }
    let be32 = |at: usize| {
        idx.get(at..at + 4)
            .map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap_or_default()))
    };
    let n = be32(8 + 1020) as usize;
    let offsets_at = 8 + 1024 + n * 24;
    let large_at = offsets_at + n * 4;
    (0..n)
        .filter_map(|i| {
            let at = 8 + 1024 + i * 20;
            let oid = Oid::from_bytes(idx.get(at..at + 20)?).ok()?;
            let v = be32(offsets_at + i * 4);
            let off = if v & 0x8000_0000 == 0 {
                v as u64
            } else {
                let at = large_at + (v & 0x7fff_ffff) as usize * 8;
                u64::from_be_bytes(idx.get(at..at + 8)?.try_into().ok()?)
            };
            Some((oid, off))
        })
        .collect()
}

/// `<pack>.rev`, git's reverse index: each object's index position in
/// pack order.
fn write_rev(pack: &Path) -> Result<(), GitError> {
    use sha1::Digest;
    let offsets = idx_offsets(&pack.with_extension("idx"));
    let mut order: Vec<usize> = (0..offsets.len()).collect();
    order.sort_by_key(|i| offsets[*i].1);
    let data = std::fs::read(pack)?;
    let mut out = Vec::with_capacity(12 + offsets.len() * 4 + 40);
    out.extend_from_slice(b"RIDX");
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    for i in order {
        out.extend_from_slice(&(i as u32).to_be_bytes());
    }
    out.extend_from_slice(&data[data.len().saturating_sub(20)..]);
    let digest = sha1::Sha1::digest(&out);
    out.extend_from_slice(&digest);
    std::fs::write(pack.with_extension("rev"), out)?;
    Ok(())
}

/// Write `<pack>.mtimes` for a cruft pack, as git's cruft-pack format.
fn write_mtimes(pack: &Path, times: &HashMap<Oid, i64>) -> Result<(), GitError> {
    use sha1::Digest;
    let ids = idx_ids(&pack.with_extension("idx"));
    let data = std::fs::read(pack)?;
    let checksum = &data[data.len().saturating_sub(20)..];
    let mut out = Vec::new();
    out.extend_from_slice(b"MTME");
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    for id in ids {
        out.extend_from_slice(&(times.get(&id).copied().unwrap_or(0) as u32).to_be_bytes());
    }
    out.extend_from_slice(checksum);
    let digest = sha1::Sha1::digest(&out);
    out.extend_from_slice(&digest);
    std::fs::write(pack.with_extension("mtimes"), out)?;
    Ok(())
}

/// `git repack`; returns what it reports (nothing unless there is nothing to
/// pack).
pub fn repack(repo: &Repository, o: &RepackOptions) -> Result<String, GitError> {
    let existing = packs(repo);
    let kept = |p: &Pack| {
        p.keep
            || o.keep_pack
                .iter()
                .any(|k| k.trim_end_matches(".pack") == p.name || k == &p.name)
    };
    let kept_ids: HashSet<Oid> = existing
        .iter()
        .filter(|p| kept(p))
        .flat_map(Pack::ids)
        .collect();
    let everything = o.all || o.all_loosen || o.cruft;
    let reach = all_reachable(repo);
    let loose = loose_objects(repo);
    let packed: HashSet<Oid> = existing.iter().flat_map(Pack::ids).collect();
    let loose_ids: HashSet<Oid> = loose.iter().map(|(id, ..)| *id).collect();
    let mut ids: Vec<Oid> = if everything {
        reach
            .iter()
            .copied()
            .filter(|id| !kept_ids.contains(id) && (packed.contains(id) || loose_ids.contains(id)))
            .collect()
    } else {
        loose
            .iter()
            .map(|(id, ..)| *id)
            .filter(|id| reach.contains(id) && !packed.contains(id))
            .collect()
    };
    if o.keep_unreachable && everything {
        ids.extend(
            existing
                .iter()
                .filter(|p| !kept(p))
                .flat_map(Pack::ids)
                .chain(loose.iter().map(|(id, ..)| *id))
                .filter(|id| !reach.contains(id) && !kept_ids.contains(id)),
        );
    }
    ids.sort();
    ids.dedup();
    let odb = repo.odb()?;
    ids.retain(|id| odb.exists(*id));
    let name = new_pack(repo, &ids)?;
    let mut report = String::new();
    if name.is_none() && !everything {
        report.push_str("Nothing new to pack.");
    }
    let dir = objects_dir(repo).join("pack");
    if everything && o.delete {
        let old: Vec<&Pack> = existing
            .iter()
            .filter(|p| !kept(p) && Some(&p.name) != name.as_ref())
            .collect();
        let in_new: HashSet<Oid> = name
            .as_ref()
            .map(|n| idx_ids(&dir.join(format!("{n}.idx"))).into_iter().collect())
            .unwrap_or_default();
        if o.cruft {
            let limit = o.cruft_expiration.unwrap_or(i64::MIN);
            let mut times: HashMap<Oid, i64> = HashMap::new();
            for p in &old {
                let from = if p.cruft {
                    read_mtimes(p)
                } else {
                    HashMap::new()
                };
                for id in p.ids() {
                    let t = from.get(&id).copied().unwrap_or(p.mtime);
                    times
                        .entry(id)
                        .and_modify(|v| *v = (*v).max(t))
                        .or_insert(t);
                }
            }
            for (id, _, t) in &loose {
                times
                    .entry(*id)
                    .and_modify(|v| *v = (*v).max(*t))
                    .or_insert(*t);
            }
            // Objects reachable from recent unreachable ones stay too.
            let mut keep_cruft: HashSet<Oid> = HashSet::new();
            let recent: Vec<Oid> = times
                .iter()
                .filter(|(id, t)| **t > limit && !in_new.contains(id) && !kept_ids.contains(id))
                .map(|(id, _)| *id)
                .collect();
            reachable(repo, &recent, &mut keep_cruft);
            let mut cruft: Vec<Oid> = keep_cruft
                .into_iter()
                .filter(|id| !in_new.contains(id) && !kept_ids.contains(id) && odb.exists(*id))
                .collect();
            cruft.sort();
            if let Some(cname) = new_pack(repo, &cruft)? {
                let path = dir.join(format!("{cname}.pack"));
                let stamps: HashMap<Oid, i64> = cruft
                    .iter()
                    .map(|id| (*id, times.get(id).copied().unwrap_or_else(now)))
                    .collect();
                write_mtimes(&path, &stamps)?;
            }
        } else if o.all_loosen && !o.keep_unreachable {
            let limit = o.unpack_unreachable.unwrap_or(i64::MIN);
            for p in &old {
                if p.mtime <= limit {
                    continue;
                }
                for id in p.ids() {
                    if in_new.contains(&id) || kept_ids.contains(&id) {
                        continue;
                    }
                    if let Ok(obj) = odb.read(id) {
                        write_loose(repo, id, obj.kind(), obj.data(), p.mtime)?;
                    }
                }
            }
        }
        for p in old {
            for ext in ["pack", "idx", "rev", "bitmap", "mtimes", "promisor"] {
                let f = p.file(ext);
                if f.exists() {
                    let _ = std::fs::remove_file(f);
                }
            }
        }
    }
    if o.delete {
        prune_packed(repo, false);
    }
    if !o.no_update_server_info && cfg_bool(repo, "repack.updateServerInfo", true) {
        update_server_info(repo)?;
    }
    Ok(report)
}

fn read_mtimes(p: &Pack) -> HashMap<Oid, i64> {
    let Ok(data) = std::fs::read(p.file("mtimes")) else {
        return HashMap::new();
    };
    p.ids()
        .into_iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let at = 12 + i * 4;
            let t = u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?);
            Some((id, t as i64))
        })
        .collect()
}

/// objects/info/packs, as `git update-server-info` writes it.
fn update_server_info(repo: &Repository) -> Result<(), GitError> {
    let info = objects_dir(repo).join("info");
    std::fs::create_dir_all(&info)?;
    let mut text = String::new();
    for p in packs(repo) {
        text.push_str(&format!("P {}.pack\n", p.name));
    }
    text.push('\n');
    std::fs::write(info.join("packs"), text)?;
    Ok(())
}

/// Refs that belong to one worktree and never go into packed-refs.
fn per_worktree(name: &str) -> bool {
    ["refs/bisect/", "refs/worktree/", "refs/rewritten/"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// The loose ref files under refs/: name and path.
fn loose_refs(repo: &Repository) -> Vec<(String, PathBuf)> {
    let common = repo.commondir();
    let mut out: Vec<(String, PathBuf)> = walkdir::WalkDir::new(common.join("refs"))
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let rel = e.path().strip_prefix(common).ok()?;
            Some((
                rel.to_string_lossy().replace('\\', "/"),
                e.path().to_owned(),
            ))
        })
        .filter(|(n, _)| !n.ends_with(".lock") && !per_worktree(n))
        .collect();
    out.sort();
    out
}

/// Whether enough loose refs piled up for `pack-refs --auto` (git's files
/// backend heuristic: more than 5 per doubling of packed-refs, at least 16).
pub(crate) fn refs_need_packing(repo: &Repository) -> bool {
    let size = std::fs::metadata(repo.commondir().join("packed-refs")).map_or(0, |m| m.len());
    let limit = if size > 100 {
        (5 * (64 - (size / 100).leading_zeros()) as usize).max(16)
    } else {
        16
    };
    loose_refs(repo).len() >= limit
}

/// `git pack-refs`: write packed-refs with peeled lines as git does, and
/// (unless `no_prune`) drop the loose files it now holds.
pub fn pack_refs(repo: &Repository, all: bool, no_prune: bool, auto: bool) -> Result<(), GitError> {
    if auto && !refs_need_packing(repo) {
        return Ok(());
    }
    let common = repo.commondir();
    let packed_path = common.join("packed-refs");
    let mut refs: std::collections::BTreeMap<String, Oid> = std::fs::read_to_string(&packed_path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with('^'))
        .filter_map(|l| {
            let (id, name) = l.split_once(' ')?;
            Some((name.to_owned(), Oid::from_str(id).ok()?))
        })
        .collect();
    let mut packed_now = Vec::new();
    for (name, path) in loose_refs(repo) {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let text = text.trim();
        if text.starts_with("ref:") {
            continue;
        }
        let Ok(oid) = Oid::from_str(text) else {
            continue;
        };
        let already = refs.contains_key(&name);
        if !(all || already || name.starts_with("refs/tags/")) {
            continue;
        }
        refs.insert(name.clone(), oid);
        packed_now.push((path, oid, name));
    }
    let mut out = String::from("# pack-refs with: peeled fully-peeled sorted \n");
    for (name, oid) in &refs {
        out.push_str(&format!("{oid} {name}\n"));
        if let Ok(obj) = repo.find_object(*oid, None)
            && obj.kind() == Some(ObjectType::Tag)
            && let Ok(peeled) = obj.peel(ObjectType::Any)
        {
            let mut target = peeled;
            while target.kind() == Some(ObjectType::Tag) {
                match target.peel(ObjectType::Any) {
                    Ok(t) if t.id() != target.id() => target = t,
                    _ => break,
                }
            }
            out.push_str(&format!("^{}\n", target.id()));
        }
    }
    let lock = common.join("packed-refs.lock");
    std::fs::write(&lock, out)?;
    std::fs::rename(&lock, &packed_path)?;
    if !no_prune {
        let refs_dir = common.join("refs");
        for (path, oid, name) in packed_now {
            let same = std::fs::read_to_string(&path).is_ok_and(|t| t.trim() == oid.to_string());
            if same {
                let _ = std::fs::remove_file(&path);
                let mut dir = path.parent();
                while let Some(d) = dir.filter(|d| {
                    d.strip_prefix(&refs_dir)
                        .is_ok_and(|r| r.components().count() >= 2)
                }) {
                    if std::fs::remove_dir(d).is_err() {
                        break;
                    }
                    dir = d.parent();
                }
            }
            let _ = name;
        }
    }
    Ok(())
}

/// `git reflog expire` settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflogExpire {
    /// Entries older than this go (default gc.reflogExpire, 90 days).
    pub expire: Option<String>,
    /// Entries not reachable from the tip and older than this go (default
    /// gc.reflogExpireUnreachable, 30 days).
    pub expire_unreachable: Option<String>,
    /// Every reflog (--all).
    pub all: bool,
    /// Only this worktree's reflogs with --all (--single-worktree).
    pub single_worktree: bool,
    /// Point each kept entry's old id at the one before it (--rewrite).
    pub rewrite: bool,
    /// Move the ref to the newest kept entry (--updateref).
    pub updateref: bool,
    /// Drop entries whose commits are missing or broken (--stale-fix).
    pub stale_fix: bool,
    pub dry_run: bool,
    pub verbose: bool,
    /// The refs whose reflogs to expire, without --all.
    pub refs: Vec<String>,
}

/// The full ref name for a reflog argument (`main`, `HEAD`, `refs/...`).
fn log_ref(repo: &Repository, name: &str) -> String {
    if name == "HEAD" || name.starts_with("refs/") {
        return name.to_owned();
    }
    repo.resolve_reference_from_short_name(name)
        .ok()
        .and_then(|r| r.name().ok().map(str::to_owned))
        .unwrap_or_else(|| name.to_owned())
}

fn log_path(repo: &Repository, name: &str) -> PathBuf {
    if name == "HEAD" {
        repo.path().join("logs/HEAD")
    } else {
        repo.commondir().join("logs").join(name)
    }
}

fn commits_from(repo: &Repository, tips: &[Oid]) -> HashSet<Oid> {
    let mut seen = HashSet::new();
    let mut stack: Vec<Oid> = tips.to_vec();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Ok(c) = repo.find_commit(id) {
            stack.extend(c.parent_ids());
        }
    }
    seen
}

/// Rewrite one reflog file, keeping the entries `keep` says; `--rewrite`
/// chains the old ids, `--updateref` moves the ref.
fn rewrite_log(
    repo: &Repository,
    name: &str,
    path: &Path,
    lines: &[LogLine],
    keep: &[bool],
    o: &ReflogExpire,
) -> Result<(), GitError> {
    if o.dry_run {
        return Ok(());
    }
    let mut out = String::new();
    let mut last: Option<Oid> = None;
    for (l, k) in lines.iter().zip(keep) {
        if !k {
            continue;
        }
        match last.filter(|_| o.rewrite) {
            Some(prev) if prev != l.old => {
                out.push_str(&format!("{prev}{}", &l.raw[40..]));
            }
            _ => out.push_str(&l.raw),
        }
        last = Some(l.new);
    }
    std::fs::write(path, out)?;
    if o.updateref
        && let Some(new) = last
        && !new.is_zero()
    {
        let target = if name == "HEAD" {
            match repo.find_reference("HEAD")?.symbolic_target()? {
                Some(t) => t.to_owned(),
                None => "HEAD".to_owned(),
            }
        } else {
            name.to_owned()
        };
        let current = repo.refname_to_id(&target).ok();
        if current != Some(new) {
            // Direct update without a new reflog line, as git's updateref.
            if target == "HEAD" {
                std::fs::write(repo.path().join("HEAD"), format!("{new}\n"))?;
            } else {
                let file = repo.commondir().join(&target);
                if let Some(d) = file.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::write(file, format!("{new}\n"))?;
            }
        }
    }
    Ok(())
}

/// `git reflog expire`; returns git's --verbose/--dry-run lines.
pub fn reflog_expire(repo: &Repository, o: &ReflogExpire) -> Result<Vec<String>, GitError> {
    let total = match &o.expire {
        Some(v) => cutoff(v)?,
        None => cutoff(&cfg_str(repo, "gc.reflogExpire").unwrap_or_else(|| "90.days.ago".into()))?,
    };
    let unreach = match &o.expire_unreachable {
        Some(v) => cutoff(v)?,
        None => cutoff(
            &cfg_str(repo, "gc.reflogExpireUnreachable").unwrap_or_else(|| "30.days.ago".into()),
        )?,
    };
    let targets: Vec<(String, PathBuf)> = if o.all {
        reflog_files(repo)
            .into_iter()
            .filter(|(_, p)| {
                !o.single_worktree || !p.starts_with(repo.commondir().join("worktrees"))
            })
            .collect()
    } else {
        o.refs
            .iter()
            .map(|r| {
                let name = log_ref(repo, r);
                let path = log_path(repo, &name);
                (name, path)
            })
            .collect()
    };
    let odb = repo.odb()?;
    let mut all_tips: Option<HashSet<Oid>> = None;
    let mut out = Vec::new();
    for (name, path) in targets {
        let lines = read_reflog(&path);
        let tip = repo.refname_to_id(&name).ok();
        let reach: Option<HashSet<Oid>> = if unreach <= total {
            None
        } else if name == "HEAD" {
            if all_tips.is_none() {
                let tips: Vec<Oid> = repo
                    .references()?
                    .flatten()
                    .filter_map(|r| r.peel_to_commit().ok().map(|c| c.id()))
                    .collect();
                all_tips = Some(commits_from(repo, &tips));
            }
            all_tips.clone()
        } else {
            // A ref that is gone reaches nothing.
            Some(tip.map(|t| commits_from(repo, &[t])).unwrap_or_default())
        };
        let unreachable = |id: &Oid| {
            reach
                .as_ref()
                .is_some_and(|r| !id.is_zero() && repo.find_commit(*id).is_ok() && !r.contains(id))
        };
        let keep: Vec<bool> = lines
            .iter()
            .map(|l| {
                let stale = o.stale_fix
                    && [l.old, l.new]
                        .iter()
                        .any(|id| !id.is_zero() && !odb.exists(*id));
                let expired = l.time < total
                    || l.time < unreach && (unreachable(&l.old) || unreachable(&l.new));
                let gone = stale || expired;
                if o.verbose {
                    let msg = if l.message.is_empty() {
                        "\n"
                    } else {
                        &l.message
                    };
                    let verb = if gone { "prune" } else { "keep" };
                    out.push(format!("{verb} {}", msg.trim_end_matches('\n')));
                }
                !gone
            })
            .collect();
        if keep.iter().all(|k| *k) && !o.rewrite {
            continue;
        }
        rewrite_log(repo, &name, &path, &lines, &keep, o)?;
    }
    Ok(out)
}

/// `git reflog delete <ref>@{<n>}...`.
pub fn reflog_delete(
    repo: &Repository,
    entries: &[String],
    o: &ReflogExpire,
) -> Result<(), GitError> {
    let mut by_ref: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for e in entries {
        let (name, n) = e
            .strip_suffix('}')
            .and_then(|e| e.rsplit_once("@{"))
            .and_then(|(r, n)| Some((r, n.parse::<usize>().ok()?)))
            .ok_or_else(|| GitError::Other(format!("not a reflog: {e}")))?;
        let name = if name.is_empty() { "HEAD" } else { name };
        by_ref.entry(log_ref(repo, name)).or_default().push(n);
    }
    for (name, ns) in by_ref {
        let path = log_path(repo, &name);
        let lines = read_reflog(&path);
        if lines.is_empty() {
            return Err(GitError::Other(format!(
                "reflog could not be found: '{name}'"
            )));
        }
        let mut keep = vec![true; lines.len()];
        for n in ns {
            // @{0} is the newest entry, the file's last line.
            let Some(i) = lines.len().checked_sub(n + 1) else {
                return Err(GitError::Other(format!(
                    "reflog entry '{name}@{{{n}}}' not found"
                )));
            };
            keep[i] = false;
        }
        rewrite_log(repo, &name, &path, &lines, &keep, o)?;
    }
    Ok(())
}

/// Whether `name` has a reflog.
pub fn reflog_exists(repo: &Repository, name: &str) -> bool {
    log_path(repo, name).is_file()
}

/// `git rerere gc`: forget resolutions older than gc.rerereResolved (60
/// days) and unresolved records older than gc.rerereUnresolved (15 days).
pub fn rerere_gc(repo: &Repository) -> Result<(), GitError> {
    let dir = repo.commondir().join("rr-cache");
    let resolved =
        cutoff(&cfg_str(repo, "gc.rerereResolved").unwrap_or_else(|| "60.days.ago".into()))?;
    let unresolved =
        cutoff(&cfg_str(repo, "gc.rerereUnresolved").unwrap_or_else(|| "15.days.ago".into()))?;
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        let (file, limit) = if p.join("postimage").exists() {
            ("postimage", resolved)
        } else {
            ("preimage", unresolved)
        };
        let t = std::fs::metadata(p.join(file)).map_or(0, |m| mtime(&m));
        if t < limit {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
    Ok(())
}

/// `git worktree prune --expire`: drop the records of worktrees whose folder
/// is gone, once their gitdir file is older than `expire`.
pub fn worktree_prune(repo: &Repository, expire: i64) -> Result<Vec<String>, GitError> {
    let mut out = Vec::new();
    for wt in worktree_dirs(repo) {
        if wt.join("locked").exists() {
            continue;
        }
        let gitdir = std::fs::read_to_string(wt.join("gitdir")).unwrap_or_default();
        let gone = gitdir.trim().is_empty() || !Path::new(gitdir.trim()).exists();
        let t = std::fs::metadata(wt.join("gitdir")).map_or(0, |m| mtime(&m));
        if gone && t <= expire {
            std::fs::remove_dir_all(&wt)?;
            out.push(
                wt.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    let _ = std::fs::remove_dir(repo.commondir().join("worktrees"));
    Ok(out)
}

/// What `git gc` does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcOptions {
    /// Prune loose objects older than this (default gc.pruneExpire, 2 weeks
    /// ago); `never` keeps them.
    pub prune: Option<String>,
    pub no_prune: bool,
    pub aggressive: bool,
    /// Only when the gc.auto / gc.autoPackLimit thresholds are passed.
    pub auto: bool,
    /// Run even if another gc holds gc.pid.
    pub force: bool,
    pub keep_largest_pack: bool,
    /// Cruft packs (default gc.cruftPacks, true).
    pub cruft: Option<bool>,
    pub quiet: bool,
}

fn too_many_loose(repo: &Repository) -> bool {
    let auto = cfg_int(repo, "gc.auto", 6700);
    if auto <= 0 {
        return false;
    }
    let threshold = (auto + 255) / 256;
    let n = std::fs::read_dir(objects_dir(repo).join("17"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.len() == 38 && n.bytes().all(|b| b.is_ascii_hexdigit())
        })
        .count() as i64;
    n > threshold
}

fn too_many_packs(repo: &Repository) -> bool {
    let limit = cfg_int(repo, "gc.autoPackLimit", 50);
    limit > 0 && packs(repo).iter().filter(|p| !p.keep).count() as i64 >= limit
}

/// Whether `gc --auto` would do anything.
pub(crate) fn gc_needed(repo: &Repository) -> bool {
    cfg_int(repo, "gc.auto", 6700) > 0 && (too_many_loose(repo) || too_many_packs(repo))
}

/// gc.pid, as git's lock: `<pid> <host>`.
struct GcLock(PathBuf);

impl Drop for GcLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn host() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn lock_gc(repo: &Repository, force: bool) -> Result<GcLock, GitError> {
    let path = repo.commondir().join("gc.pid");
    let me = host();
    if !force
        && let Ok(text) = std::fs::read_to_string(&path)
        && let Some((pid, who)) = text.trim().split_once(' ')
        && who == me
        && std::fs::metadata(&path).is_ok_and(|m| now() - mtime(&m) < 12 * 3600)
        && pid.parse::<u32>().is_ok_and(|p| {
            p != std::process::id()
                && std::process::Command::new("kill")
                    .args(["-0", &p.to_string()])
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
        })
    {
        return Err(GitError::Other(format!(
            "gc is already running on machine '{who}' pid {pid} (use --force if not)"
        )));
    }
    let lock = repo.commondir().join("gc.pid.lock");
    std::fs::write(&lock, format!("{} {me}", std::process::id()))?;
    std::fs::rename(&lock, &path)?;
    Ok(GcLock(path))
}

/// `git gc`: pack-refs, reflog expire, repack, prune, worktree prune, rerere
/// gc and the commit-graph, as git runs them. Returns what it reports.
pub fn gc(repo: &Repository, o: &GcOptions) -> Result<String, GitError> {
    let mut report = Vec::new();
    let mut all_loosen_or_cruft = true;
    if o.auto {
        if !gc_needed(repo) {
            return Ok(String::new());
        }
        if !o.quiet {
            report.push(
                "Auto packing the repository for optimum performance.\nSee \"git help gc\" for \
                 manual housekeeping."
                    .to_owned(),
            );
        }
        all_loosen_or_cruft = too_many_packs(repo);
    }
    let _lock = lock_gc(repo, o.force)?;
    let bare = repo.is_bare();
    let pack_refs_on = match cfg_str(repo, "gc.packRefs").as_deref() {
        Some("notbare") | None => !bare,
        Some(v) => !matches!(v.to_lowercase().as_str(), "false" | "no" | "off" | "0"),
    };
    if pack_refs_on {
        pack_refs(repo, true, false, false)?;
    }
    reflog_expire(
        repo,
        &ReflogExpire {
            all: true,
            ..Default::default()
        },
    )?;
    let prune_expire = if o.no_prune {
        None
    } else {
        match o.prune.as_deref() {
            Some("") | None => {
                Some(cfg_str(repo, "gc.pruneExpire").unwrap_or_else(|| "2.weeks.ago".into()))
            }
            Some(v) => Some(v.to_owned()),
        }
    }
    .filter(|v| v != "never");
    let prune_cut = prune_expire.as_deref().map(cutoff).transpose()?;
    let cruft = o
        .cruft
        .unwrap_or_else(|| cfg_bool(repo, "gc.cruftPacks", true));
    let mut keep_pack = Vec::new();
    let big = cfg_int(repo, "gc.bigPackThreshold", 0);
    let existing = packs(repo);
    if o.keep_largest_pack {
        if let Some(p) = existing.iter().filter(|p| !p.keep).max_by_key(|p| p.size) {
            keep_pack.push(p.name.clone());
        }
    } else if big > 0 {
        keep_pack.extend(
            existing
                .iter()
                .filter(|p| p.size as i64 >= big)
                .map(|p| p.name.clone()),
        );
    }
    let now_prune = prune_expire.as_deref() == Some("now");
    let mut ro = RepackOptions {
        delete: true,
        keep_pack,
        ..Default::default()
    };
    if all_loosen_or_cruft {
        if now_prune {
            ro.all = true;
        } else if cruft {
            ro.cruft = true;
            ro.cruft_expiration = prune_cut;
        } else {
            ro.all_loosen = true;
            ro.unpack_unreachable = prune_cut;
        }
    }
    repack(repo, &ro)?;
    if let Some(cut) = prune_cut {
        prune(repo, cut, false, false)?;
    }
    let wt_expire =
        cutoff(&cfg_str(repo, "gc.worktreePruneExpire").unwrap_or_else(|| "3.months.ago".into()))?;
    worktree_prune(repo, wt_expire)?;
    rerere_gc(repo)?;
    if cfg_bool(repo, "gc.writeCommitGraph", true) && cfg_bool(repo, "core.commitGraph", true) {
        crate::commit_graph::write(repo)?;
    }
    if o.auto && too_many_loose(repo) && !o.quiet {
        report.push(
            "warning: There are too many unreachable loose objects; run 'git prune' to remove \
             them."
                .to_owned(),
        );
    }
    Ok(report.join("\n"))
}

/// Pack every loose object into a new pack (batches of
/// maintenance.loose-objects.batchSize), after dropping those already
/// packed: the loose-objects maintenance task.
pub fn pack_loose(repo: &Repository) -> Result<(), GitError> {
    prune_packed(repo, false);
    let batch = cfg_int(repo, "maintenance.loose-objects.batchSize", 50000).max(1) as usize;
    let ids: Vec<Oid> = loose_objects(repo)
        .into_iter()
        .map(|(id, ..)| id)
        .take(batch)
        .collect();
    new_pack(repo, &ids)?;
    Ok(())
}

/// The incremental-repack task without a multi-pack-index: fold every pack
/// but the largest (and kept ones) into one.
// ponytail: no multi-pack-index; packs are merged directly, not via
// `multi-pack-index repack --batch-size`.
pub fn incremental_repack(repo: &Repository) -> Result<(), GitError> {
    let all = packs(repo);
    let mut small: Vec<&Pack> = all.iter().filter(|p| !p.keep && !p.cruft).collect();
    small.sort_by_key(|p| p.size);
    small.pop();
    if small.len() < 2 {
        return Ok(());
    }
    let mut ids: Vec<Oid> = small.iter().flat_map(|p| p.ids()).collect();
    ids.sort();
    ids.dedup();
    let name = new_pack(repo, &ids)?;
    for p in small {
        if Some(&p.name) == name.as_ref() {
            continue;
        }
        for ext in ["pack", "idx", "rev", "bitmap"] {
            let _ = std::fs::remove_file(p.file(ext));
        }
    }
    update_server_info(repo)
}

/// How many loose objects there are.
pub(crate) fn loose_count(repo: &Repository) -> usize {
    loose_objects(repo).len()
}

/// Packs outside kept and cruft ones.
pub(crate) fn pack_count(repo: &Repository) -> usize {
    packs(repo).iter().filter(|p| !p.keep && !p.cruft).count()
}

/// `git maintenance run`'s tasks, in git's order.
pub const TASKS: [&str; 9] = [
    "prefetch",
    "loose-objects",
    "incremental-repack",
    "gc",
    "commit-graph",
    "pack-refs",
    "reflog-expire",
    "worktree-prune",
    "rerere-gc",
];

/// `hourly`, `daily`, `weekly` as git ranks them (more often is higher).
fn schedule_rank(s: &str) -> Option<u8> {
    match s.to_lowercase().as_str() {
        "hourly" => Some(3),
        "daily" => Some(2),
        "weekly" => Some(1),
        _ => None,
    }
}

/// What `git maintenance run` does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaintenanceRun {
    /// Only these tasks, in this order (--task).
    pub tasks: Vec<String>,
    /// Only tasks whose auto condition holds (--auto).
    pub auto: bool,
    /// Only tasks scheduled at least this often (--schedule).
    pub schedule: Option<String>,
    pub quiet: bool,
}

fn task_due(repo: &Repository, task: &str) -> bool {
    let over = |n: usize, default: i64| {
        let l = cfg_int(repo, &format!("maintenance.{task}.auto"), default);
        l < 0 || l > 0 && n as i64 >= l
    };
    match task {
        "gc" => gc_needed(repo),
        "loose-objects" => over(loose_count(repo), 100),
        "incremental-repack" => over(pack_count(repo), 10),
        "commit-graph" => over(crate::commit_graph::missing(repo), 100),
        "pack-refs" => refs_need_packing(repo),
        "reflog-expire" => {
            let cut = cfg_str(repo, "gc.reflogExpire")
                .and_then(|v| crate::expiry_date(&v))
                .unwrap_or_else(|| now() - 90 * 86400);
            let old = reflog_files(repo)
                .iter()
                .map(|(_, p)| read_reflog(p).iter().filter(|l| l.time < cut).count())
                .sum();
            over(old, 100)
        }
        "worktree-prune" => {
            let gone = worktree_dirs(repo)
                .iter()
                .filter(|wt| {
                    let gitdir = std::fs::read_to_string(wt.join("gitdir")).unwrap_or_default();
                    !wt.join("locked").exists() && !Path::new(gitdir.trim()).exists()
                })
                .count();
            over(gone, 1)
        }
        "rerere-gc" => {
            let n = std::fs::read_dir(repo.commondir().join("rr-cache"))
                .into_iter()
                .flatten()
                .count();
            over(n, 1)
        }
        _ => false,
    }
}

/// The schedule a task runs on under maintenance.strategy=incremental.
fn incremental_schedule(task: &str) -> Option<&'static str> {
    match task {
        "commit-graph" | "prefetch" => Some("hourly"),
        "incremental-repack" | "loose-objects" => Some("daily"),
        "pack-refs" => Some("weekly"),
        _ => None,
    }
}

struct Unlock(PathBuf);

impl Drop for Unlock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `git maintenance run`: the selected, enabled or scheduled tasks; `prefetch`
/// fetches the remotes into refs/prefetch. Returns what it reports.
pub fn run(
    repo: &Repository,
    o: &MaintenanceRun,
    prefetch: &dyn Fn() -> Result<(), GitError>,
) -> Result<String, GitError> {
    for t in &o.tasks {
        if !TASKS.contains(&t.as_str()) {
            return Err(GitError::Other(format!("'{t}' is not a valid task")));
        }
        if o.tasks.iter().filter(|x| *x == t).count() > 1 {
            return Err(GitError::Other(format!(
                "task '{t}' cannot be selected multiple times"
            )));
        }
    }
    let run_rank =
        match &o.schedule {
            Some(s) => Some(schedule_rank(s).ok_or_else(|| {
                GitError::Other(format!("unrecognized --schedule argument '{s}'"))
            })?),
            None => None,
        };
    let lock = repo.commondir().join("objects/maintenance.lock");
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .is_err()
    {
        return Ok(if o.auto || o.quiet {
            String::new()
        } else {
            format!(
                "warning: lock file '{}' exists, skipping maintenance",
                lock.display()
            )
        });
    }
    let _unlock = Unlock(lock);
    let incremental = run_rank.is_some()
        && cfg_str(repo, "maintenance.strategy")
            .is_some_and(|s| s.eq_ignore_ascii_case("incremental"));
    let order: Vec<&str> = if o.tasks.is_empty() {
        TASKS.to_vec()
    } else {
        o.tasks.iter().map(String::as_str).collect()
    };
    let mut report = Vec::new();
    let mut failed = Vec::new();
    for task in order {
        let strategy = incremental.then(|| incremental_schedule(task)).flatten();
        if o.tasks.is_empty() {
            let enabled = cfg(repo)
                .and_then(|c| c.get_bool(&format!("maintenance.{task}.enabled")).ok())
                .unwrap_or(task == "gc" && !incremental || strategy.is_some());
            if !enabled {
                continue;
            }
        }
        if o.auto && (task == "prefetch" || !task_due(repo, task)) {
            continue;
        }
        if let Some(rank) = run_rank {
            let own = cfg_str(repo, &format!("maintenance.{task}.schedule"));
            let task_rank = own
                .as_deref()
                .or(strategy)
                .and_then(schedule_rank)
                .unwrap_or(0);
            if task_rank < rank {
                continue;
            }
        }
        let result = match task {
            "prefetch" => prefetch(),
            "loose-objects" => pack_loose(repo),
            "incremental-repack" => incremental_repack(repo),
            "gc" => gc(
                repo,
                &GcOptions {
                    auto: o.auto,
                    quiet: o.quiet,
                    ..Default::default()
                },
            )
            .map(|r| report.extend((!r.is_empty()).then_some(r))),
            "commit-graph" if cfg_bool(repo, "core.commitGraph", true) => {
                crate::commit_graph::write(repo)
            }
            "pack-refs" => pack_refs(repo, true, false, false),
            "reflog-expire" => reflog_expire(
                repo,
                &ReflogExpire {
                    all: true,
                    ..Default::default()
                },
            )
            .map(drop),
            "worktree-prune" => cutoff(
                &cfg_str(repo, "gc.worktreePruneExpire").unwrap_or_else(|| "3.months.ago".into()),
            )
            .and_then(|cut| worktree_prune(repo, cut).map(drop)),
            "rerere-gc" => rerere_gc(repo),
            _ => Ok(()),
        };
        if let Err(e) = result {
            failed.push(format!("error: {e}\nerror: task '{task}' failed"));
        }
    }
    if !failed.is_empty() {
        report.extend(failed);
        return Err(GitError::Cli(report.join("\n")));
    }
    Ok(report.join("\n"))
}
