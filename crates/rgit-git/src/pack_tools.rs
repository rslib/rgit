//! Pack file plumbing: index-pack, verify-pack, unpack-objects,
//! pack-objects and show-index, over a pack parser of our own (libgit2
//! keeps its own hidden).

use crate::rev::RevParse;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{ObjectType, Oid, Repository};
use sha1::Digest;

use crate::GitError;

fn err(message: impl Into<String>) -> GitError {
    GitError::Other(message.into())
}

/// One object of a pack, resolved.
pub struct PackEntry {
    pub offset: u64,
    /// The pack's own type: 1-4, 6 (offset delta) or 7 (ref delta).
    kind: u8,
    /// The size in the entry header (the delta's, for deltas).
    pub size: u64,
    pub id: Oid,
    pub real: ObjectType,
    pub depth: u32,
    pub base: Option<Oid>,
    pub crc: u32,
}

impl PackEntry {
    pub fn is_delta(&self) -> bool {
        self.kind >= 6
    }
}

/// A parsed pack: its entries in pack order, each object's content, and
/// the pack's trailing checksum.
pub struct Pack {
    pub entries: Vec<PackEntry>,
    pub contents: Vec<Vec<u8>>,
    pub checksum: [u8; 20],
    /// Where the objects end (the checksum starts).
    pub end: u64,
}

fn kind_of(k: u8) -> Option<ObjectType> {
    match k {
        1 => Some(ObjectType::Commit),
        2 => Some(ObjectType::Tree),
        3 => Some(ObjectType::Blob),
        4 => Some(ObjectType::Tag),
        _ => None,
    }
}

#[cfg(test)]
fn kind_num(k: ObjectType) -> u8 {
    match k {
        ObjectType::Commit => 1,
        ObjectType::Tree => 2,
        ObjectType::Blob => 3,
        _ => 4,
    }
}

/// Apply git's delta format to `base`.
fn apply_delta(base: &[u8], delta: &[u8]) -> Option<Vec<u8>> {
    let mut p = 0;
    let varint = |p: &mut usize| {
        let (mut v, mut shift) = (0usize, 0);
        loop {
            let b = *delta.get(*p)?;
            *p += 1;
            v |= ((b & 0x7f) as usize) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
    };
    let src = varint(&mut p)?;
    let dst = varint(&mut p)?;
    if src != base.len() {
        return None;
    }
    let mut out = Vec::with_capacity(dst);
    while p < delta.len() {
        let op = delta[p];
        p += 1;
        if op & 0x80 != 0 {
            let (mut off, mut size) = (0usize, 0usize);
            for i in 0..4 {
                if op & (1 << i) != 0 {
                    off |= (*delta.get(p)? as usize) << (8 * i);
                    p += 1;
                }
            }
            for i in 0..3 {
                if op & (0x10 << i) != 0 {
                    size |= (*delta.get(p)? as usize) << (8 * i);
                    p += 1;
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            out.extend_from_slice(base.get(off..off + size)?);
        } else if op != 0 {
            out.extend_from_slice(delta.get(p..p + op as usize)?);
            p += op as usize;
        } else {
            return None;
        }
    }
    (out.len() == dst).then_some(out)
}

enum Base {
    None,
    Offset(u64),
    Ref(Oid),
}

/// Parse and resolve the pack in `data`. Ref-delta bases missing from the
/// pack come from `repo` when given.
pub fn parse_pack(data: &[u8], repo: Option<&Repository>) -> Result<Pack, GitError> {
    if data.len() < 32 || &data[..4] != b"PACK" {
        return Err(err("bad pack file"));
    }
    let be32 = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap_or_default());
    let version = be32(4);
    if version != 2 && version != 3 {
        return Err(err(format!("unknown pack file version {version}")));
    }
    let count = be32(8) as usize;
    let mut raw: Vec<(u64, u8, u64, Base, Vec<u8>, u32)> = Vec::with_capacity(count);
    let mut at = 12usize;
    for _ in 0..count {
        let start = at;
        let mut c = *data.get(at).ok_or_else(|| err("early EOF"))?;
        at += 1;
        let kind = (c >> 4) & 7;
        let mut size = u64::from(c & 0x0f);
        let mut shift = 4;
        while c & 0x80 != 0 {
            c = *data.get(at).ok_or_else(|| err("early EOF"))?;
            at += 1;
            size |= u64::from(c & 0x7f) << shift;
            shift += 7;
        }
        let base = match kind {
            6 => {
                let mut c = *data.get(at).ok_or_else(|| err("early EOF"))?;
                at += 1;
                let mut back = u64::from(c & 0x7f);
                while c & 0x80 != 0 {
                    c = *data.get(at).ok_or_else(|| err("early EOF"))?;
                    at += 1;
                    back = ((back + 1) << 7) | u64::from(c & 0x7f);
                }
                Base::Offset(
                    (start as u64)
                        .checked_sub(back)
                        .ok_or_else(|| err("offset value out of bound for delta base object"))?,
                )
            }
            7 => {
                let id = Oid::from_bytes(data.get(at..at + 20).ok_or_else(|| err("early EOF"))?)?;
                at += 20;
                Base::Ref(id)
            }
            1..=4 => Base::None,
            k => return Err(err(format!("bad object type {k}"))),
        };
        let mut z = flate2::Decompress::new(true);
        let mut out = Vec::with_capacity(size as usize);
        loop {
            out.reserve(4096);
            let before = z.total_in();
            let status = z
                .decompress_vec(&data[at..], &mut out, flate2::FlushDecompress::Finish)
                .map_err(|e| err(format!("inflate returned {e}")))?;
            at += (z.total_in() - before) as usize;
            match status {
                flate2::Status::StreamEnd => break,
                _ if at >= data.len() => return Err(err("early EOF")),
                _ => {}
            }
        }
        if out.len() as u64 != size {
            return Err(err("inflated size mismatch"));
        }
        let mut crc = flate2::Crc::new();
        crc.update(&data[start..at]);
        raw.push((start as u64, kind, size, base, out, crc.sum()));
    }
    let end = at as u64;
    let trailer = data.get(at..at + 20).ok_or_else(|| err("early EOF"))?;
    if sha1::Sha1::digest(&data[..at]).as_slice() != trailer {
        return Err(err("final sha1 did not match"));
    }
    let checksum: [u8; 20] = trailer.try_into().unwrap_or_default();

    let by_offset: HashMap<u64, usize> = raw.iter().enumerate().map(|(i, r)| (r.0, i)).collect();
    let n = raw.len();
    let mut resolved: Vec<Option<(ObjectType, Vec<u8>, u32)>> = (0..n).map(|_| None).collect();
    let mut by_id: HashMap<Oid, usize> = HashMap::new();
    for (i, r) in raw.iter().enumerate() {
        if let Some(k) = kind_of(r.1) {
            let id = Oid::hash_object(k, &r.4)?;
            by_id.insert(id, i);
            resolved[i] = Some((k, r.4.clone(), 0));
        }
    }
    let mut outside: HashMap<Oid, (ObjectType, Vec<u8>)> = HashMap::new();
    // Resolve deltas until nothing changes; bases can come in any order.
    loop {
        let mut progress = false;
        for i in 0..n {
            if resolved[i].is_some() {
                continue;
            }
            let (base_kind, base_data, base_depth) = match &raw[i].3 {
                Base::Offset(o) => {
                    let j = *by_offset
                        .get(o)
                        .ok_or_else(|| err("offset value out of bound for delta base object"))?;
                    match &resolved[j] {
                        Some((k, d, depth)) => (*k, d.clone(), *depth),
                        None => continue,
                    }
                }
                Base::Ref(id) => match by_id.get(id) {
                    Some(&j) => match &resolved[j] {
                        Some((k, d, depth)) => (*k, d.clone(), *depth),
                        None => continue,
                    },
                    None => {
                        let found = match outside.get(id) {
                            Some(x) => Some(x.clone()),
                            None => repo.and_then(|r| {
                                let odb = r.odb().ok()?;
                                let o = odb.read(*id).ok()?;
                                Some((o.kind(), o.data().to_vec()))
                            }),
                        };
                        let Some((k, d)) = found else { continue };
                        outside.insert(*id, (k, d.clone()));
                        (k, d, 0)
                    }
                },
                Base::None => unreachable!("whole objects are resolved"),
            };
            let out =
                apply_delta(&base_data, &raw[i].4).ok_or_else(|| err("failed to apply delta"))?;
            let id = Oid::hash_object(base_kind, &out)?;
            by_id.insert(id, i);
            resolved[i] = Some((base_kind, out, base_depth + 1));
            progress = true;
        }
        if !progress {
            break;
        }
    }
    let mut entries = Vec::with_capacity(n);
    let mut contents = Vec::with_capacity(n);
    for (i, r) in raw.iter().enumerate() {
        let Some((real, content, depth)) = resolved[i].take() else {
            return Err(err("unresolved deltas left after unpacking"));
        };
        entries.push(PackEntry {
            offset: r.0,
            kind: r.1,
            size: r.2,
            id: Oid::hash_object(real, &content)?,
            real,
            depth,
            base: None,
            crc: r.5,
        });
        contents.push(content);
    }
    // Base ids, now that every id is known.
    for i in 0..n {
        if let Base::Offset(o) = raw[i].3 {
            entries[i].base = Some(entries[by_offset[&o]].id);
        } else if let Base::Ref(id) = raw[i].3 {
            entries[i].base = Some(id);
        }
    }
    Ok(Pack {
        entries,
        contents,
        checksum,
        end,
    })
}

/// A version 2 `.idx` for `pack`.
pub fn write_idx(pack: &Pack) -> Vec<u8> {
    let mut order: Vec<usize> = (0..pack.entries.len()).collect();
    order.sort_by_key(|&i| pack.entries[i].id);
    let mut out = Vec::new();
    out.extend_from_slice(b"\xfftOc");
    out.extend_from_slice(&2u32.to_be_bytes());
    let mut fanout = [0u32; 256];
    for e in &pack.entries {
        fanout[e.id.as_bytes()[0] as usize] += 1;
    }
    let mut sum = 0;
    for f in fanout {
        sum += f;
        out.extend_from_slice(&sum.to_be_bytes());
    }
    for &i in &order {
        out.extend_from_slice(pack.entries[i].id.as_bytes());
    }
    for &i in &order {
        out.extend_from_slice(&pack.entries[i].crc.to_be_bytes());
    }
    let mut large = Vec::new();
    for &i in &order {
        let off = pack.entries[i].offset;
        if off < 0x8000_0000 {
            out.extend_from_slice(&(off as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&(0x8000_0000 | large.len() as u32).to_be_bytes());
            large.push(off);
        }
    }
    for off in large {
        out.extend_from_slice(&off.to_be_bytes());
    }
    out.extend_from_slice(&pack.checksum);
    let digest = sha1::Sha1::digest(&out);
    out.extend_from_slice(&digest);
    out
}

/// A `.rev` reverse index for `pack`.
fn write_rev(pack: &Pack) -> Vec<u8> {
    let mut order: Vec<usize> = (0..pack.entries.len()).collect();
    order.sort_by_key(|&i| pack.entries[i].id);
    let mut pos = vec![0u32; order.len()];
    for (idx_pos, &i) in order.iter().enumerate() {
        pos[i] = idx_pos as u32;
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIDX");
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    for p in pos {
        out.extend_from_slice(&p.to_be_bytes());
    }
    out.extend_from_slice(&pack.checksum);
    let digest = sha1::Sha1::digest(&out);
    out.extend_from_slice(&digest);
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// index-pack's --verify-stat listing and histogram.
fn stats(pack: &Pack, stat_only: bool) -> String {
    let mut out = String::new();
    let mut histogram: Vec<usize> = Vec::new();
    let mut base_objects = 0;
    for (i, e) in pack.entries.iter().enumerate() {
        if e.is_delta() {
            let d = e.depth as usize;
            if histogram.len() < d {
                histogram.resize(d, 0);
            }
            histogram[d - 1] += 1;
        } else {
            base_objects += 1;
        }
        if stat_only {
            continue;
        }
        let next = pack.entries.get(i + 1).map_or(pack.end, |n| n.offset);
        out.push_str(&format!(
            "{} {:<6} {} {} {}",
            e.id,
            e.real.str(),
            e.size,
            next - e.offset,
            e.offset
        ));
        if let (true, Some(b)) = (e.is_delta(), e.base) {
            out.push_str(&format!(" {} {b}", e.depth));
        }
        out.push('\n');
    }
    let s = |n: usize| if n == 1 { "" } else { "s" };
    if base_objects > 0 {
        out.push_str(&format!(
            "non delta: {base_objects} object{}\n",
            s(base_objects)
        ));
    }
    for (i, n) in histogram.iter().enumerate() {
        if *n > 0 {
            out.push_str(&format!("chain length = {}: {n} object{}\n", i + 1, s(*n)));
        }
    }
    out
}

/// What `git index-pack` does.
#[derive(Default)]
pub struct IndexPackOpts {
    /// The pack file (with --stdin, where to write it).
    pub pack: Option<PathBuf>,
    /// The index to write (-o).
    pub index: Option<PathBuf>,
    /// Read the pack from this (--stdin).
    pub stdin: Option<Vec<u8>>,
    /// --keep[=<msg>].
    pub keep: Option<String>,
    /// Check an existing index instead of writing one.
    pub verify: bool,
    /// With `verify`, list the objects (--verify-stat), or only the
    /// histogram (--verify-stat-only).
    pub stat: bool,
    pub stat_only: bool,
    pub rev_index: Option<bool>,
}

/// `git index-pack`: what it prints on stdout.
pub fn index_pack(git_dir: Option<&Path>, o: &IndexPackOpts) -> Result<String, GitError> {
    let repo = git_dir.map(Repository::open).transpose()?;
    let index_path = o
        .index
        .clone()
        .or_else(|| o.pack.as_ref().map(|p| p.with_extension("idx")));
    if let (true, Some(i)) = (o.verify, &index_path)
        && !i.exists()
    {
        let pack_there = o.pack.as_ref().is_some_and(|p| p.exists());
        return Err(err(if pack_there {
            format!("Cannot open existing pack idx file for '{}'", i.display())
        } else {
            format!("Cannot open existing pack file '{}'", i.display())
        }));
    }
    let data = match (&o.stdin, &o.pack) {
        (Some(d), _) => d.clone(),
        (None, Some(p)) => std::fs::read(p)
            .map_err(|e| err(format!("cannot open packfile '{}': {e}", p.display())))?,
        (None, None) => return Err(err("no pack given")),
    };
    let pack = parse_pack(&data, repo.as_ref())?;
    let mut out = String::new();
    if o.stat {
        out.push_str(&stats(&pack, o.stat_only));
    }
    let idx = write_idx(&pack);
    let name = hex(&pack.checksum);
    if o.verify {
        let Some(path) = index_path else {
            return Err(err("--verify with no packfile name given"));
        };
        let old = std::fs::read(&path)
            .map_err(|e| err(format!("cannot open packfile '{}': {e}", path.display())))?;
        if old != idx {
            return Err(err(format!(
                "the index file '{}' does not match the pack",
                path.display()
            )));
        }
        return Ok(out);
    }
    let (pack_path, index_path) = match (&o.pack, index_path) {
        (Some(p), Some(i)) => (p.clone(), i),
        _ => {
            let repo = repo
                .as_ref()
                .ok_or_else(|| err("--stdin requires a git repository"))?;
            let dir = repo.commondir().join("objects/pack");
            std::fs::create_dir_all(&dir)?;
            let base = dir.join(format!("pack-{name}"));
            (base.with_extension("pack"), base.with_extension("idx"))
        }
    };
    let mut report = "pack";
    if let Some(msg) = &o.keep {
        let keep = pack_path.with_extension("keep");
        if !keep.exists() {
            let text = if msg.is_empty() {
                String::new()
            } else {
                format!("{msg}\n")
            };
            std::fs::write(keep, text)?;
            report = "keep";
        }
    }
    if o.stdin.is_some() && !pack_path.exists() {
        std::fs::write(&pack_path, &data)?;
    }
    let rev = o.rev_index.unwrap_or_else(|| {
        repo.as_ref()
            .and_then(|r| r.config().ok()?.get_bool("pack.writeReverseIndex").ok())
            .unwrap_or(true)
    });
    if rev {
        put(&index_path.with_extension("rev"), &write_rev(&pack))?;
    }
    put(&index_path, &idx)?;
    if o.stdin.is_some() {
        out.push_str(&format!("{report}\t{name}\n"));
    } else {
        out.push_str(&format!("{name}\n"));
    }
    Ok(out)
}

/// Write `data` to `path`, leaving an identical (read-only) file alone.
fn put(path: &Path, data: &[u8]) -> Result<(), GitError> {
    if std::fs::read(path).is_ok_and(|old| old == data) {
        return Ok(());
    }
    let _ = std::fs::remove_file(path);
    std::fs::write(path, data)?;
    Ok(())
}

/// `git verify-pack` of one `<pack>`, `<pack>.idx` or `<pack>.pack`: the
/// report (with `verbose` or `stat_only`) and whether it checked out.
pub fn verify_pack(path: &str, verbose: bool, stat_only: bool) -> (String, String, bool) {
    let base = path.strip_suffix(".idx").unwrap_or(path);
    let pack = if base.ends_with(".pack") {
        base.to_owned()
    } else {
        format!("{base}.pack")
    };
    let o = IndexPackOpts {
        pack: Some(PathBuf::from(&pack)),
        verify: true,
        stat: verbose || stat_only,
        stat_only,
        ..IndexPackOpts::default()
    };
    let (mut out, mut errs, ok) = match index_pack(None, &o) {
        Ok(text) => (text, String::new(), true),
        Err(e) => (String::new(), format!("fatal: {e}\n"), false),
    };
    if verbose || stat_only {
        if !ok {
            out.push_str(&format!("{pack}: bad\n"));
        } else if !stat_only {
            out.push_str(&format!("{pack}: ok\n"));
        }
    }
    if !ok && errs.is_empty() {
        errs.push_str("error: verify failed\n");
    }
    (out, errs, ok)
}

/// `git unpack-objects`: write each object of the pack in `data` as a
/// loose object, unless the repository has it. `dry_run` only checks.
pub fn unpack_objects(git_dir: &Path, data: &[u8], dry_run: bool) -> Result<usize, GitError> {
    let repo = Repository::open(git_dir)?;
    let pack = parse_pack(data, Some(&repo))?;
    let odb = repo.odb()?;
    let mut n = 0;
    for (e, content) in pack.entries.iter().zip(&pack.contents) {
        n += 1;
        if dry_run || odb.exists(e.id) {
            continue;
        }
        odb.write(e.real, content)?;
    }
    Ok(n)
}

/// `git show-index`: the entries of the `.idx` in `data`, as
/// `<offset> <id> (<crc>)` (v2) or `<offset> <id>` (v1).
pub fn show_index(data: &[u8]) -> Result<String, GitError> {
    let be32 = |at: usize| -> Result<u32, GitError> {
        Ok(u32::from_be_bytes(
            data.get(at..at + 4)
                .ok_or_else(|| err("unable to read index"))?
                .try_into()
                .unwrap_or_default(),
        ))
    };
    if data.len() < 8 {
        return Err(err("unable to read header"));
    }
    let v2 = data.starts_with(b"\xfftOc");
    if v2 && be32(4)? != 2 {
        return Err(err("unknown index version"));
    }
    let fan = if v2 { 8 } else { 0 };
    let mut nr = 0;
    for i in 0..256 {
        let n = be32(fan + i * 4)?;
        if n < nr {
            return Err(err("corrupt index file"));
        }
        nr = n;
    }
    let nr = nr as usize;
    let mut out = String::new();
    let first = fan + 1024;
    if !v2 {
        for i in 0..nr {
            let at = first + i * 24;
            let off = be32(at).map_err(|_| err(format!("unable to read entry {i}/{nr}")))?;
            let id = data
                .get(at + 4..at + 24)
                .ok_or_else(|| err(format!("unable to read entry {i}/{nr}")))?;
            out.push_str(&format!("{off} {}\n", hex(id)));
        }
        return Ok(out);
    }
    let crcs = first + nr * 20;
    let offs = crcs + nr * 4;
    let large = offs + nr * 4;
    let mut large_n = 0;
    for i in 0..nr {
        let id = data
            .get(first + i * 20..first + i * 20 + 20)
            .ok_or_else(|| err(format!("unable to read sha1 {i}/{nr}")))?;
        let crc = be32(crcs + i * 4).map_err(|_| err(format!("unable to read crc {i}/{nr}")))?;
        let off =
            be32(offs + i * 4).map_err(|_| err(format!("unable to read 32b offset {i}/{nr}")))?;
        let off = if off & 0x8000_0000 == 0 {
            u64::from(off)
        } else {
            if (off & 0x7fff_ffff) as usize != large_n {
                return Err(err("inconsistent 64b offset index"));
            }
            let at = large + large_n * 8;
            large_n += 1;
            u64::from_be_bytes(
                data.get(at..at + 8)
                    .ok_or_else(|| err(format!("unable to read 64b offset {}", large_n - 1)))?
                    .try_into()
                    .unwrap_or_default(),
            )
        };
        out.push_str(&format!("{off} {} ({crc:08x})\n", hex(id)));
    }
    Ok(out)
}

/// What `git pack-objects` packs.
#[derive(Default)]
pub struct PackObjectsOpts {
    /// Object ids (one per stdin line, without --revs).
    pub objects: Vec<String>,
    /// Revisions to walk (--revs: stdin lines, and --all).
    pub revs: Option<Vec<String>>,
    pub all: bool,
}

/// `git pack-objects`: the pack's bytes and name.
pub fn pack_objects(git_dir: &Path, o: &PackObjectsOpts) -> Result<(Vec<u8>, String), GitError> {
    let repo = Repository::open(git_dir)?;
    let mut pb = repo.packbuilder()?;
    match &o.revs {
        None => {
            for line in &o.objects {
                let hexid = line.split_whitespace().next().unwrap_or("");
                if hexid.is_empty() {
                    continue;
                }
                let id = Oid::from_str(hexid)
                    .map_err(|_| err(format!("expected object ID, got garbage:\n {line}")))?;
                pb.insert_object(id, None)?;
            }
        }
        Some(revs) => {
            let mut walk = repo.revwalk()?;
            let mut not = false;
            let mut tags = Vec::new();
            if o.all {
                walk.push_glob("refs/*")?;
                for r in repo.references()?.flatten() {
                    if let (Some(id), Ok(tag)) = (r.target(), r.peel(ObjectType::Tag)) {
                        let _ = id;
                        tags.push(tag.id());
                    }
                }
            }
            for rev in revs {
                if rev == "--not" {
                    not = !not;
                    continue;
                }
                let (hide, spec) = match rev.strip_prefix('^') {
                    Some(r) => (!not, r),
                    None => (not, rev.as_str()),
                };
                let obj = repo.rev_single(spec)?;
                if obj.kind() == Some(ObjectType::Tag) && !hide {
                    tags.push(obj.id());
                }
                let c = obj.peel_to_commit()?.id();
                if hide {
                    walk.hide(c)?;
                } else {
                    walk.push(c)?;
                }
            }
            pb.insert_walk(&mut walk)?;
            for t in tags {
                pb.insert_object(t, None)?;
            }
        }
    }
    let mut buf = git2::Buf::new();
    pb.write_buf(&mut buf)?;
    let data = buf.to_vec();
    let name = hex(&data[data.len().saturating_sub(20)..]);
    Ok((data, name))
}

/// Write the pack `data` as `<base>-<name>.pack` with its index; git's
/// pack-objects without --stdout.
pub fn write_pack_files(base: &str, data: &[u8], name: &str) -> Result<(), GitError> {
    let pack = parse_pack(data, None)?;
    let stem = PathBuf::from(format!("{base}-{name}"));
    if let Some(dir) = stem.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(stem.with_extension("pack"), data)?;
    std::fs::write(stem.with_extension("idx"), write_idx(&pack))?;
    Ok(())
}

/// `git prune-packed`: remove loose objects a pack holds; with `dry_run`
/// the `rm -f` lines instead.
pub fn prune_packed_objects(git_dir: &Path, dry_run: bool) -> Result<String, GitError> {
    let repo = Repository::open(git_dir)?;
    let top = repo
        .workdir()
        .map(|w| format!("{}/", w.display().to_string().trim_end_matches('/')));
    Ok(crate::maintenance::prune_packed(&repo, dry_run)
        .into_iter()
        .map(|l| match &top {
            Some(t) => l.replacen(t.as_str(), "", 1) + "\n",
            None => l + "\n",
        })
        .collect())
}

/// `git update-server-info`.
pub fn update_server_info(git_dir: &Path, force: bool) -> Result<(), GitError> {
    crate::maintenance::update_server_info(&Repository::open(git_dir)?, force)
}

/// `git unpack-file`: the blob `name` in a new `.merge_file_XXXXXX` in the
/// current folder; the file's name.
pub fn unpack_file(git_dir: &Path, name: &str) -> Result<String, GitError> {
    let repo = Repository::open(git_dir)?;
    let id = repo
        .rev_single(name)
        .map_err(|_| err(format!("Not a valid object name {name}")))?
        .id();
    let blob = repo
        .find_blob(id)
        .map_err(|_| err(format!("unable to read blob object {id}")))?;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // git works from the top of the work tree.
    let top = repo.workdir().unwrap_or_else(|| repo.path()).to_path_buf();
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
        ^ u64::from(std::process::id());
    loop {
        let mut file = String::from(".merge_file_");
        for _ in 0..6 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            file.push(alphabet[(seed >> 33) as usize % alphabet.len()] as char);
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(top.join(&file))
        {
            Ok(mut f) => {
                f.write_all(blob.content())?;
                return Ok(file);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// A whole object's entry header, for tests.
#[cfg(test)]
fn entry_header(kind: ObjectType, size: usize) -> Vec<u8> {
    let mut out = vec![(kind_num(kind) << 4) | (size & 0x0f) as u8];
    let mut rest = size >> 4;
    while rest > 0 {
        *out.last_mut().unwrap() |= 0x80;
        out.push((rest & 0x7f) as u8);
        rest >>= 7;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_a_pack_with_an_offset_delta() {
        let base = b"hello world, hello world, hello world\n".to_vec();
        let mut delta = vec![base.len() as u8, (base.len() + 4) as u8];
        delta.extend([0x90, base.len() as u8, 4]);
        delta.extend(b"more");
        let mut pack = b"PACK\0\0\0\x02\0\0\0\x02".to_vec();
        let z = |d: &[u8]| {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(d).unwrap();
            e.finish().unwrap()
        };
        pack.extend(entry_header(ObjectType::Blob, base.len()));
        pack.extend(z(&base));
        let at = pack.len();
        pack.push((6 << 4) | delta.len() as u8);
        pack.push((at - 12) as u8);
        pack.extend(z(&delta));
        let sum = sha1::Sha1::digest(&pack);
        pack.extend(sum);
        let p = parse_pack(&pack, None).unwrap();
        assert_eq!(p.entries.len(), 2);
        let mut want = base.clone();
        want.extend(b"more");
        assert_eq!(p.contents[1], want);
        assert_eq!(p.entries[1].depth, 1);
        assert_eq!(p.entries[1].base, Some(p.entries[0].id));
    }
}
