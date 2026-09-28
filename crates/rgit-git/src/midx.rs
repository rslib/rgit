//! objects/pack/multi-pack-index in git's MIDX format (version 1): write,
//! verify, expire and repack, as `git multi-pack-index` does.

use std::collections::HashMap;
use std::path::PathBuf;

use git2::{Oid, Repository};
use sha1::Digest;

use crate::GitError;
use crate::maintenance::{self, Pack};

fn midx_path(repo: &Repository) -> PathBuf {
    repo.commondir().join("objects/pack/multi-pack-index")
}

/// A parsed multi-pack-index: its pack names (`pack-<hash>.idx`) and each
/// object's pack and offset, in id order.
pub(crate) struct Midx {
    pub packs: Vec<String>,
    pub objects: Vec<(Oid, u32, u64)>,
    fanout: Vec<u32>,
    checksum_ok: bool,
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn be64(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// The chunks of a chunk-format file (commit-graph, midx) whose table of
/// contents starts at `toc`: id and byte range.
pub(crate) fn chunks(data: &[u8], toc: usize, n: usize) -> HashMap<[u8; 4], (usize, usize)> {
    let mut out = HashMap::new();
    for i in 0..n {
        let at = toc + i * 12;
        let (Some(id), Some(start), Some(end)) = (
            data.get(at..at + 4),
            be64(data, at + 4),
            be64(data, at + 16),
        ) else {
            break;
        };
        if let Ok(id) = id.try_into() {
            out.insert(id, (start as usize, end as usize));
        }
    }
    out
}

pub(crate) fn read(repo: &Repository) -> Option<Midx> {
    let data = std::fs::read(midx_path(repo)).ok()?;
    if data.len() < 12 + 20 || &data[..4] != b"MIDX" {
        return None;
    }
    let n_chunks = data[6] as usize;
    let n_packs = be32(&data, 8)? as usize;
    let c = chunks(&data, 12, n_chunks);
    let (ps, pe) = *c.get(b"PNAM")?;
    let packs: Vec<String> = data
        .get(ps..pe)?
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .take(n_packs)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let (fs, _) = *c.get(b"OIDF")?;
    let fanout: Vec<u32> = (0..256).filter_map(|i| be32(&data, fs + i * 4)).collect();
    let n = *fanout.last()? as usize;
    let (ls, _) = *c.get(b"OIDL")?;
    let (os, _) = *c.get(b"OOFF")?;
    let large = c.get(b"LOFF").map(|r| r.0);
    let objects = (0..n)
        .filter_map(|i| {
            let id = Oid::from_bytes(data.get(ls + i * 20..ls + i * 20 + 20)?).ok()?;
            let pack = be32(&data, os + i * 8)?;
            let off = be32(&data, os + i * 8 + 4)?;
            let off = if off & 0x8000_0000 != 0 {
                be64(&data, large? + (off & 0x7fff_ffff) as usize * 8)?
            } else {
                off as u64
            };
            Some((id, pack, off))
        })
        .collect();
    let (body, sum) = data.split_at(data.len() - 20);
    Some(Midx {
        packs,
        objects,
        fanout,
        checksum_ok: sha1::Sha1::digest(body).as_slice() == sum,
    })
}

/// Pack files the multi-pack-index lists.
pub(crate) fn listed(repo: &Repository) -> Vec<String> {
    read(repo).map(|m| m.packs).unwrap_or_default()
}

fn idx_name(p: &Pack) -> String {
    format!("{}.idx", p.name)
}

/// `git multi-pack-index write`: index every pack, an object's copy taken
/// from `preferred`, else the newest pack holding it.
pub fn write(repo: &Repository, preferred: Option<&str>) -> Result<(), GitError> {
    let mut packs = maintenance::packs(repo);
    if packs.is_empty() {
        return Err(GitError::Other("no pack files to index.".to_owned()));
    }
    packs.sort_by_key(idx_name);
    let preferred = preferred.map(|p| p.trim_end_matches(".pack").trim_end_matches(".idx"));
    if let Some(p) = preferred
        && !packs.iter().any(|x| x.name == p)
    {
        return Err(GitError::Other(format!(
            "cannot select preferred pack {p}.pack"
        )));
    }
    let mut best: HashMap<Oid, (u32, u64)> = HashMap::new();
    // Sub-second mtimes, so a pack written just now beats older ones.
    let mtimes: Vec<Option<std::time::SystemTime>> = packs
        .iter()
        .map(|p| std::fs::metadata(&p.path).and_then(|m| m.modified()).ok())
        .collect();
    let rank = |i: usize| {
        (
            Some(packs[i].name.as_str()) == preferred,
            mtimes[i],
            std::cmp::Reverse(i),
        )
    };
    for (i, p) in packs.iter().enumerate() {
        for (id, off) in maintenance::idx_offsets(&p.path.with_extension("idx")) {
            match best.get(&id) {
                Some((j, _)) if rank(*j as usize) >= rank(i) => {}
                _ => {
                    best.insert(id, (i as u32, off));
                }
            }
        }
    }
    let mut objects: Vec<(Oid, u32, u64)> = best.into_iter().map(|(k, (p, o))| (k, p, o)).collect();
    objects.sort();
    let mut pnam = Vec::new();
    for p in &packs {
        pnam.extend_from_slice(idx_name(p).as_bytes());
        pnam.push(0);
    }
    while pnam.len() % 4 != 0 {
        pnam.push(0);
    }
    let mut fanout = Vec::with_capacity(1024);
    for b in 0..256usize {
        let n = objects
            .iter()
            .take_while(|(id, ..)| id.as_bytes()[0] as usize <= b)
            .count() as u32;
        fanout.extend_from_slice(&n.to_be_bytes());
    }
    let mut oidl = Vec::with_capacity(objects.len() * 20);
    let mut ooff = Vec::with_capacity(objects.len() * 8);
    let mut loff = Vec::new();
    for (id, pack, off) in &objects {
        oidl.extend_from_slice(id.as_bytes());
        ooff.extend_from_slice(&pack.to_be_bytes());
        if *off < 0x8000_0000 {
            ooff.extend_from_slice(&(*off as u32).to_be_bytes());
        } else {
            ooff.extend_from_slice(&(0x8000_0000 | (loff.len() / 8) as u32).to_be_bytes());
            loff.extend_from_slice(&off.to_be_bytes());
        }
    }
    let mut parts: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"PNAM", pnam),
        (b"OIDF", fanout),
        (b"OIDL", oidl),
        (b"OOFF", ooff),
    ];
    if !loff.is_empty() {
        parts.push((b"LOFF", loff));
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"MIDX");
    out.extend_from_slice(&[1, 1, parts.len() as u8, 0]);
    out.extend_from_slice(&(packs.len() as u32).to_be_bytes());
    out.extend_from_slice(&chunk_file(&parts, out.len()));
    let digest = sha1::Sha1::digest(&out);
    out.extend_from_slice(&digest);
    let path = midx_path(repo);
    let lock = path.with_extension("lock");
    std::fs::write(&lock, out)?;
    std::fs::rename(&lock, &path)?;
    Ok(())
}

/// A chunk table of contents (with its terminating entry) and the chunks,
/// for a file whose header is `header_len` bytes.
pub(crate) fn chunk_file(parts: &[(&[u8; 4], Vec<u8>)], header_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offset = (header_len + (parts.len() + 1) * 12) as u64;
    for (id, data) in parts {
        out.extend_from_slice(*id);
        out.extend_from_slice(&offset.to_be_bytes());
        offset += data.len() as u64;
    }
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&offset.to_be_bytes());
    for (_, data) in parts {
        out.extend_from_slice(data);
    }
    out
}

/// `git multi-pack-index verify`: git's complaints, none when it is sound
/// (or absent).
pub fn verify(repo: &Repository) -> Vec<String> {
    let path = midx_path(repo);
    if !path.exists() {
        return Vec::new();
    }
    let Some(m) = read(repo) else {
        return vec!["multi-pack-index file is corrupt or unreadable".to_owned()];
    };
    let mut out = Vec::new();
    if !m.checksum_ok {
        out.push("incorrect checksum".to_owned());
    }
    let dir = repo.commondir().join("objects/pack");
    let mut offsets: Vec<Option<HashMap<Oid, u64>>> = Vec::new();
    for (i, p) in m.packs.iter().enumerate() {
        let idx = dir.join(p);
        if !idx.exists() || !idx.with_extension("pack").exists() {
            out.push(format!("failed to load pack in position {i}"));
            offsets.push(None);
        } else {
            offsets.push(Some(maintenance::idx_offsets(&idx).into_iter().collect()));
        }
    }
    for i in 0..255 {
        if m.fanout[i] > m.fanout[i + 1] {
            out.push(format!(
                "oid fanout out of order: fanout[{i}] = {:x} > {:x} = fanout[{}]",
                m.fanout[i],
                m.fanout[i + 1],
                i + 1
            ));
        }
    }
    if m.objects.is_empty() {
        out.push("the midx contains no oid".to_owned());
    }
    for (i, w) in m.objects.windows(2).enumerate() {
        if w[0].0 >= w[1].0 {
            out.push(format!(
                "oid lookup out of order: oid[{i}] = {} >= {} = oid[{}]",
                w[0].0,
                w[1].0,
                i + 1
            ));
        }
    }
    for (i, (id, pack, off)) in m.objects.iter().enumerate() {
        let Some(Some(map)) = offsets.get(*pack as usize) else {
            continue;
        };
        match map.get(id) {
            Some(real) if real == off => {}
            real => out.push(format!(
                "incorrect object offset for oid[{i}] = {id}: {off:x} != {:x}",
                real.copied().unwrap_or(0)
            )),
        }
    }
    out
}

fn pack_files(repo: &Repository, m: &Midx) -> Vec<Option<Pack>> {
    let all = maintenance::packs(repo);
    m.packs
        .iter()
        .map(|n| all.iter().find(|p| idx_name(p) == *n).cloned())
        .collect()
}

fn referenced(m: &Midx) -> Vec<usize> {
    let mut counts = vec![0; m.packs.len()];
    for (_, p, _) in &m.objects {
        if let Some(c) = counts.get_mut(*p as usize) {
            *c += 1;
        }
    }
    counts
}

/// `git multi-pack-index expire`: delete the packs the multi-pack-index no
/// longer takes any object from (kept ones stay), then rewrite it.
pub fn expire(repo: &Repository) -> Result<(), GitError> {
    let Some(m) = read(repo) else {
        return Ok(());
    };
    let counts = referenced(&m);
    let mut dropped = false;
    for (p, n) in pack_files(repo, &m).into_iter().zip(counts) {
        if let Some(p) = p.filter(|p| n == 0 && !p.keep) {
            for ext in ["pack", "idx", "rev", "bitmap", "mtimes"] {
                let _ = std::fs::remove_file(p.path.with_extension(ext));
            }
            dropped = true;
        }
    }
    if dropped {
        write(repo, None)?;
    }
    Ok(())
}

/// `git multi-pack-index repack --batch-size=<size>`: pack the objects the
/// multi-pack-index takes from the oldest packs whose share of their size
/// adds up to `batch_size` (every pack with 0) into one, then rewrite it.
pub fn repack(repo: &Repository, batch_size: u64) -> Result<(), GitError> {
    let Some(m) = read(repo) else {
        return Ok(());
    };
    let counts = referenced(&m);
    let files = pack_files(repo, &m);
    let mut order: Vec<usize> = (0..m.packs.len()).collect();
    order.sort_by_key(|i| files[*i].as_ref().map_or(0, |p| p.mtime));
    let mut include = vec![false; m.packs.len()];
    let mut total = 0u64;
    let mut chosen = 0;
    for i in order {
        if batch_size > 0 && total >= batch_size {
            break;
        }
        let Some(p) = files[i].as_ref().filter(|p| !p.keep) else {
            continue;
        };
        if batch_size > 0 {
            let objects = maintenance::idx_ids(&p.path.with_extension("idx"))
                .len()
                .max(1) as u64;
            let expected = p.size * counts[i] as u64 / objects;
            if expected >= batch_size {
                continue;
            }
            total += expected;
        }
        include[i] = true;
        chosen += 1;
    }
    if chosen < 2 {
        return Ok(());
    }
    let ids: Vec<Oid> = m
        .objects
        .iter()
        .filter(|(_, p, _)| include[*p as usize])
        .map(|(id, ..)| *id)
        .collect();
    maintenance::new_pack(repo, &ids)?;
    write(repo, None)
}

/// A `git multi-pack-index` subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidxOp {
    /// Index every pack, preferring this one's copies.
    Write {
        preferred_pack: Option<String>,
    },
    Verify,
    Expire,
    /// Repack a batch of this many bytes (0: every pack).
    Repack {
        batch_size: u64,
    },
}

/// Run `op`; verify's complaints.
pub fn run(repo: &Repository, op: &MidxOp) -> Result<Vec<String>, GitError> {
    match op {
        MidxOp::Write { preferred_pack } => write(repo, preferred_pack.as_deref())?,
        MidxOp::Verify => return Ok(verify(repo)),
        MidxOp::Expire => expire(repo)?,
        MidxOp::Repack { batch_size } => repack(repo, *batch_size)?,
    }
    Ok(Vec::new())
}

/// The incremental-repack maintenance task as git runs it: write, expire,
/// then repack a batch the size of the second largest pack (at most 2g),
/// verifying the multi-pack-index (and rewriting it when bad) after each.
pub fn incremental_repack(repo: &Repository) -> Result<(), GitError> {
    if maintenance::packs(repo).is_empty() {
        return Ok(());
    }
    let checked = |repo: &Repository| -> Result<(), GitError> {
        if !verify(repo).is_empty() {
            let _ = std::fs::remove_file(midx_path(repo));
            write(repo, None)?;
        }
        Ok(())
    };
    write(repo, None)?;
    checked(repo)?;
    expire(repo)?;
    checked(repo)?;
    let (mut max, mut second) = (1u64, 1u64);
    for p in maintenance::packs(repo) {
        if p.size > max {
            second = max;
            max = p.size;
        } else if p.size > second {
            second = p.size;
        }
    }
    repack(repo, (second + 1).min(2 << 30))?;
    checked(repo)
}
