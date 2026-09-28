//! A pack writer with git's delta search (pack-objects' sliding window over
//! objects sorted by type, name hash and size, with a maximum chain depth),
//! the `.idx` v2 and `.rev` files beside it, and `.bitmap` reachability
//! bitmaps in git's EWAH format.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use git2::{ObjectType, Oid, Repository};
use sha1::Digest;

use crate::GitError;

/// git's delta search settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeltaOpts {
    pub window: usize,
    pub depth: usize,
    /// Bytes the window may hold (0: no limit).
    pub window_memory: u64,
    /// Search threads (0: one per CPU).
    pub threads: usize,
}

struct Entry {
    oid: Oid,
    kind: ObjectType,
    data: Vec<u8>,
    hash: u32,
}

/// git's pack_name_hash: the last sixteen non-blank characters count most.
fn name_hash(name: &str) -> u32 {
    name.bytes()
        .filter(|c| !c.is_ascii_whitespace())
        .fold(0u32, |h, c| (h >> 2).wrapping_add((c as u32) << 24))
}

fn type_code(kind: ObjectType) -> u8 {
    match kind {
        ObjectType::Commit => 1,
        ObjectType::Tree => 2,
        ObjectType::Blob => 3,
        _ => 4,
    }
}

/// A delta that turns `src` into `trg`, in git's copy/insert opcodes; None
/// when it would not be smaller than `max`.
pub(crate) fn delta(src: &[u8], trg: &[u8], max: usize) -> Option<Vec<u8>> {
    const BLOCK: usize = 16;
    let mut out = Vec::with_capacity(trg.len() / 4 + 16);
    for mut n in [src.len(), trg.len()] {
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
    }
    let mut index: HashMap<&[u8], Vec<u32>> = HashMap::new();
    let mut i = 0;
    while i + BLOCK <= src.len() {
        let v = index.entry(&src[i..i + BLOCK]).or_default();
        if v.len() < 64 {
            v.push(i as u32);
        }
        i += BLOCK;
    }
    let mut insert: Vec<u8> = Vec::new();
    let flush = |out: &mut Vec<u8>, insert: &mut Vec<u8>| {
        for chunk in insert.chunks(127) {
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
        insert.clear();
    };
    let mut t = 0;
    while t < trg.len() {
        let mut best = (0usize, 0usize);
        if t + BLOCK <= trg.len()
            && let Some(cands) = index.get(&trg[t..t + BLOCK])
        {
            for &s in cands.iter().rev() {
                let s = s as usize;
                let len = src[s..]
                    .iter()
                    .zip(&trg[t..])
                    .take_while(|(a, b)| a == b)
                    .count();
                if len > best.1 {
                    best = (s, len);
                }
            }
        }
        if best.1 < BLOCK {
            insert.push(trg[t]);
            t += 1;
            continue;
        }
        // Grow the match back over bytes waiting to be inserted.
        let (mut s, mut len) = best;
        while s > 0 && !insert.is_empty() && src[s - 1] == *insert.last().unwrap_or(&0) {
            insert.pop();
            s -= 1;
            len += 1;
        }
        flush(&mut out, &mut insert);
        t += best.1;
        while len > 0 {
            let n = len.min(0x10000);
            let mut op = 0x80u8;
            let mut args = Vec::with_capacity(7);
            for k in 0..4 {
                let b = (s >> (8 * k)) as u8;
                if b != 0 {
                    op |= 1 << k;
                    args.push(b);
                }
            }
            if n != 0x10000 {
                for k in 0..3 {
                    let b = (n >> (8 * k)) as u8;
                    if b != 0 {
                        op |= 0x10 << k;
                        args.push(b);
                    }
                }
            }
            out.push(op);
            out.extend_from_slice(&args);
            s += n;
            len -= n;
        }
        if out.len() >= max {
            return None;
        }
    }
    flush(&mut out, &mut insert);
    (out.len() < max).then_some(out)
}

/// Apply a git delta to `src` (for checking what [`delta`] writes).
#[cfg(test)]
fn patch(src: &[u8], d: &[u8]) -> Vec<u8> {
    let mut at = 0;
    let mut varint = || {
        let (mut n, mut shift) = (0usize, 0);
        loop {
            let b = d[at];
            at += 1;
            n |= ((b & 0x7f) as usize) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                return n;
            }
        }
    };
    varint();
    let size = varint();
    let mut out = Vec::with_capacity(size);
    while at < d.len() {
        let op = d[at];
        at += 1;
        if op & 0x80 != 0 {
            let (mut off, mut n) = (0usize, 0usize);
            for k in 0..4 {
                if op & (1 << k) != 0 {
                    off |= (d[at] as usize) << (8 * k);
                    at += 1;
                }
            }
            for k in 0..3 {
                if op & (0x10 << k) != 0 {
                    n |= (d[at] as usize) << (8 * k);
                    at += 1;
                }
            }
            if n == 0 {
                n = 0x10000;
            }
            out.extend_from_slice(&src[off..off + n]);
        } else {
            out.extend_from_slice(&d[at..at + op as usize]);
            at += op as usize;
        }
    }
    out
}

/// Name hashes for trees and blobs, from the tree entries naming them.
fn name_hashes(entries: &[Entry]) -> HashMap<Oid, u32> {
    let mut out = HashMap::new();
    for e in entries.iter().filter(|e| e.kind == ObjectType::Tree) {
        let data = &e.data;
        let mut at = 0;
        while let Some(nul) = data[at..].iter().position(|b| *b == 0) {
            let head = &data[at..at + nul];
            let name = head
                .iter()
                .position(|b| *b == b' ')
                .map_or(&head[..0], |sp| &head[sp + 1..]);
            let Some(id) = data
                .get(at + nul + 1..at + nul + 21)
                .and_then(|b| Oid::from_bytes(b).ok())
            else {
                break;
            };
            out.entry(id)
                .or_insert_with(|| name_hash(&String::from_utf8_lossy(name)));
            at += nul + 21;
        }
    }
    out
}

/// Per object: its delta base (an index) and the delta.
type Deltas = Vec<Option<(usize, Vec<u8>)>>;

/// git's find_deltas over one run of the sorted list.
fn find_deltas(entries: &[Entry], list: &[usize], o: &DeltaOpts) -> Vec<(usize, usize, Vec<u8>)> {
    let mut found = Vec::new();
    let mut depth: HashMap<usize, usize> = HashMap::new();
    let mut window: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut held = 0u64;
    for &t in list {
        let trg = &entries[t];
        let mut best: Option<(usize, Vec<u8>)> = None;
        for &s in window.iter().rev() {
            let src = &entries[s];
            if src.kind != trg.kind {
                continue;
            }
            let src_depth = depth.get(&s).copied().unwrap_or(0);
            if src_depth >= o.depth {
                continue;
            }
            let (max, ref_depth) = match &best {
                None => ((trg.data.len() / 2).saturating_sub(20), 1),
                Some((b, d)) => (d.len(), depth.get(b).copied().unwrap_or(0) + 1),
            };
            let max = max * (o.depth - src_depth) / (o.depth + 1 - ref_depth.min(o.depth));
            if max == 0 {
                continue;
            }
            if trg.data.len().saturating_sub(src.data.len()) >= max
                || trg.data.len() < src.data.len() / 32
            {
                continue;
            }
            if let Some(d) = delta(&src.data, &trg.data, max) {
                best = Some((s, d));
            }
        }
        if let Some((s, d)) = best {
            depth.insert(t, depth.get(&s).copied().unwrap_or(0) + 1);
            found.push((t, s, d));
        }
        window.push_back(t);
        held += trg.data.len() as u64;
        while window.len() > o.window
            || o.window_memory > 0 && held > o.window_memory && window.len() > 1
        {
            if let Some(old) = window.pop_front() {
                held -= entries[old].data.len() as u64;
            }
        }
    }
    found
}

/// Write `ids` as a new pack with git's delta search, its `.idx` (v2) and,
/// with pack.writeReverseIndex, its `.rev`; its `pack-<hash>` name.
// ponytail: every object is held in memory while packing; stream them if
// packs outgrow RAM.
pub(crate) fn write_pack(
    repo: &Repository,
    ids: &[Oid],
    o: &DeltaOpts,
) -> Result<Option<String>, GitError> {
    if ids.is_empty() {
        return Ok(None);
    }
    let odb = repo.odb()?;
    let mut entries: Vec<Entry> = Vec::with_capacity(ids.len());
    for id in ids {
        let obj = odb.read(*id)?;
        entries.push(Entry {
            oid: *id,
            kind: obj.kind(),
            data: obj.data().to_vec(),
            hash: 0,
        });
    }
    let hashes = name_hashes(&entries);
    for e in &mut entries {
        e.hash = hashes.get(&e.oid).copied().unwrap_or(0);
    }
    // git's type_size_sort: type, name hash, then larger first.
    let mut sorted: Vec<usize> = (0..entries.len())
        .filter(|i| entries[*i].data.len() >= 50)
        .collect();
    sorted.sort_by(|a, b| {
        let (x, y) = (&entries[*a], &entries[*b]);
        type_code(y.kind)
            .cmp(&type_code(x.kind))
            .then(y.hash.cmp(&x.hash))
            .then(y.data.len().cmp(&x.data.len()))
            .then(a.cmp(b))
    });
    let mut deltas: Deltas = (0..entries.len()).map(|_| None).collect();
    if o.window > 0 && o.depth > 0 {
        let threads = if o.threads == 0 {
            rayon::current_num_threads()
        } else {
            o.threads
        };
        let per = sorted.len().div_ceil(threads.max(1)).max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads.max(1))
            .build()
            .map_err(|e| GitError::Other(e.to_string()))?;
        let found: Vec<Vec<(usize, usize, Vec<u8>)>> = pool.install(|| {
            use rayon::prelude::*;
            sorted
                .par_chunks(per)
                .map(|run| find_deltas(&entries, run, o))
                .collect()
        });
        for (t, s, d) in found.into_iter().flatten() {
            deltas[t] = Some((s, d));
        }
    }
    // Recency order as git writes it: commits, tags, trees, blobs, each
    // delta after its base.
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|i| match entries[*i].kind {
        ObjectType::Commit => 0,
        ObjectType::Tag => 1,
        ObjectType::Tree => 2,
        _ => 3,
    });
    let mut pack = Vec::new();
    pack.extend_from_slice(b"PACK");
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    let mut offsets: Vec<Option<u64>> = vec![None; entries.len()];
    let mut crcs = vec![0u32; entries.len()];
    for start in order {
        let mut chain = vec![start];
        while let Some((base, _)) = &deltas[*chain.last().unwrap_or(&start)] {
            if offsets[*base].is_some() {
                break;
            }
            chain.push(*base);
        }
        for i in chain.into_iter().rev() {
            if offsets[i].is_some() {
                continue;
            }
            let at = pack.len() as u64;
            let mut raw = Vec::new();
            let (code, body): (u8, &[u8]) = match &deltas[i] {
                Some((_, d)) => (6, d),
                None => (type_code(entries[i].kind), &entries[i].data),
            };
            let mut size = body.len();
            let mut b = (code << 4) | (size & 15) as u8;
            size >>= 4;
            while size > 0 {
                raw.push(b | 0x80);
                b = (size & 0x7f) as u8;
                size >>= 7;
            }
            raw.push(b);
            if let Some((base, _)) = &deltas[i] {
                let mut ofs = at - offsets[*base].unwrap_or(0);
                let mut enc = vec![(ofs & 0x7f) as u8];
                ofs >>= 7;
                while ofs > 0 {
                    ofs -= 1;
                    enc.push(0x80 | (ofs & 0x7f) as u8);
                    ofs >>= 7;
                }
                enc.reverse();
                raw.extend_from_slice(&enc);
            }
            let mut z = flate2::write::ZlibEncoder::new(raw, flate2::Compression::default());
            z.write_all(body)?;
            let raw = z.finish()?;
            let mut crc = flate2::Crc::new();
            crc.update(&raw);
            crcs[i] = crc.sum();
            offsets[i] = Some(at);
            pack.extend_from_slice(&raw);
        }
    }
    let checksum = sha1::Sha1::digest(&pack);
    pack.extend_from_slice(&checksum);
    let name = format!("pack-{}", hex(&checksum));
    let dir = repo.commondir().join("objects/pack");
    std::fs::create_dir_all(&dir)?;
    let mut idx_order: Vec<usize> = (0..entries.len()).collect();
    idx_order.sort_by_key(|i| entries[*i].oid);
    let idx = write_idx(
        &idx_order
            .iter()
            .map(|i| (entries[*i].oid, offsets[*i].unwrap_or(0), crcs[*i]))
            .collect::<Vec<_>>(),
        &checksum,
    );
    let tmp = dir.join(format!("tmp_pack_{}", std::process::id()));
    std::fs::write(&tmp, &pack)?;
    std::fs::rename(&tmp, dir.join(format!("{name}.pack")))?;
    std::fs::write(&tmp, &idx)?;
    std::fs::rename(&tmp, dir.join(format!("{name}.idx")))?;
    Ok(Some(name))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A v2 `.idx` for (id, offset, crc32) sorted by id.
pub(crate) fn write_idx(objs: &[(Oid, u64, u32)], pack_checksum: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\xfftOc");
    out.extend_from_slice(&2u32.to_be_bytes());
    for b in 0..256usize {
        let n = objs
            .iter()
            .take_while(|(id, ..)| id.as_bytes()[0] as usize <= b)
            .count() as u32;
        out.extend_from_slice(&n.to_be_bytes());
    }
    for (id, ..) in objs {
        out.extend_from_slice(id.as_bytes());
    }
    for (_, _, crc) in objs {
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut large = Vec::new();
    for (_, off, _) in objs {
        if *off < 0x8000_0000 {
            out.extend_from_slice(&(*off as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&(0x8000_0000 | (large.len() / 8) as u32).to_be_bytes());
            large.extend_from_slice(&off.to_be_bytes());
        }
    }
    out.extend_from_slice(&large);
    out.extend_from_slice(pack_checksum);
    let digest = sha1::Sha1::digest(&out);
    out.extend_from_slice(&digest);
    out
}

/// A bitmap in git's EWAH form: bit size, words, last run-length word.
pub(crate) fn ewah(bits: &[u64], bit_size: usize) -> Vec<u8> {
    let mut buf: Vec<u64> = Vec::new();
    let mut rlw;
    let mut i = 0;
    loop {
        rlw = buf.len();
        buf.push(0);
        let clean = |w: u64| w == 0 || w == u64::MAX;
        let run_bit = bits.get(i).is_some_and(|w| *w == u64::MAX);
        let mut run = 0u64;
        while let Some(&w) = bits.get(i) {
            if !clean(w) || (w == u64::MAX) != run_bit || run == 0xffff_ffff {
                break;
            }
            run += 1;
            i += 1;
        }
        let mut lits = 0u64;
        while let Some(&w) = bits.get(i) {
            if clean(w) || lits == 0x7fff_ffff {
                break;
            }
            buf.push(w);
            lits += 1;
            i += 1;
        }
        buf[rlw] = u64::from(run_bit) | (run << 1) | (lits << 33);
        if i >= bits.len() {
            break;
        }
    }
    let mut out = Vec::with_capacity(12 + buf.len() * 8);
    out.extend_from_slice(&(bit_size as u32).to_be_bytes());
    out.extend_from_slice(&(buf.len() as u32).to_be_bytes());
    for w in &buf {
        out.extend_from_slice(&w.to_be_bytes());
    }
    out.extend_from_slice(&(rlw as u32).to_be_bytes());
    out
}

fn links(kind: ObjectType, data: &[u8]) -> Vec<Oid> {
    match kind {
        ObjectType::Tree => crate::maintenance::tree_entries(data)
            .into_iter()
            .filter(|(m, _)| *m != 0o160000)
            .map(|(_, id)| id)
            .collect(),
        ObjectType::Commit | ObjectType::Tag => data
            .split(|b| *b == b'\n')
            .take_while(|l| !l.is_empty())
            .filter_map(|l| {
                let l = std::str::from_utf8(l).ok()?;
                ["tree ", "parent ", "object "]
                    .iter()
                    .find_map(|k| l.strip_prefix(k))
                    .and_then(|h| Oid::from_str(h).ok())
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Write `<pack>.bitmap` for a pack holding everything `tips` reach: one
/// bitmap per selected commit (the ref tips and every 100th commit), as
/// git's pack-bitmap v1 with the full-DAG option. False (and nothing
/// written) when the pack lacks a reachable object.
// ponytail: each selected commit's trees are walked afresh (only commit
// bitmaps are reused); memoize tree bitmaps if large repos need it.
pub(crate) fn write_bitmap(repo: &Repository, pack: &Path, tips: &[Oid]) -> Result<bool, GitError> {
    let idx = crate::maintenance::idx_offsets(&pack.with_extension("idx"));
    let data = std::fs::read(pack)?;
    let checksum = &data[data.len().saturating_sub(20)..];
    let mut by_offset: Vec<usize> = (0..idx.len()).collect();
    by_offset.sort_by_key(|i| idx[*i].1);
    let pos: HashMap<Oid, usize> = by_offset
        .iter()
        .enumerate()
        .map(|(p, i)| (idx[*i].0, p))
        .collect();
    let idx_pos: HashMap<Oid, usize> = idx
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();
    let n = idx.len();
    let words = n.div_ceil(64);
    let odb = repo.odb()?;
    let mut kinds: HashMap<Oid, ObjectType> = HashMap::new();
    let mut type_bits = vec![vec![0u64; words]; 4];
    for (id, _) in &idx {
        let (_, k) = odb.read_header(*id)?;
        kinds.insert(*id, k);
        let t = match k {
            ObjectType::Commit => 0,
            ObjectType::Tree => 1,
            ObjectType::Blob => 2,
            _ => 3,
        };
        type_bits[t][pos[id] / 64] |= 1 << (pos[id] % 64);
    }
    // Commits to bitmap: the tips, then every 100th commit, oldest first.
    let mut tip_commits: Vec<Oid> = Vec::new();
    for t in tips {
        let mut id = *t;
        while kinds.get(&id) == Some(&ObjectType::Tag) {
            let Ok(obj) = odb.read(id) else { break };
            match links(ObjectType::Tag, obj.data()).first() {
                Some(next) => id = *next,
                None => break,
            }
        }
        if kinds.get(&id) == Some(&ObjectType::Commit) {
            tip_commits.push(id);
        }
    }
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
    for t in &tip_commits {
        walk.push(*t)?;
    }
    let tip_set: HashSet<Oid> = tip_commits.iter().copied().collect();
    let selected: Vec<Oid> = walk
        .flatten()
        .enumerate()
        .filter(|(i, id)| i % 100 == 99 || tip_set.contains(id))
        .map(|(_, id)| id)
        .collect();
    let mut done: HashMap<Oid, Vec<u64>> = HashMap::new();
    for c in &selected {
        let mut bits = vec![0u64; words];
        let mut seen: HashSet<Oid> = HashSet::new();
        let mut stack = vec![*c];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if id != *c
                && let Some(b) = done.get(&id)
            {
                for (w, x) in bits.iter_mut().zip(b) {
                    *w |= x;
                }
                continue;
            }
            let Some(p) = pos.get(&id) else {
                return Ok(false);
            };
            bits[p / 64] |= 1 << (p % 64);
            let obj = odb.read(id)?;
            stack.extend(links(obj.kind(), obj.data()));
        }
        done.insert(*c, bits);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"BITM");
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(selected.len() as u32).to_be_bytes());
    out.extend_from_slice(checksum);
    for t in &type_bits {
        out.extend_from_slice(&ewah(t, n));
    }
    for c in &selected {
        out.extend_from_slice(&(idx_pos[c] as u32).to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&ewah(&done[c], n));
    }
    let digest = sha1::Sha1::digest(&out);
    out.extend_from_slice(&digest);
    let path: PathBuf = pack.with_extension("bitmap");
    let tmp = path.with_extension("bitmap.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, &path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_rebuild_the_target() {
        let src: Vec<u8> = (0..5000u32).flat_map(|i| (i * 7).to_le_bytes()).collect();
        let mut trg = src.clone();
        trg.splice(100..120, b"something new here".iter().copied());
        trg.extend_from_slice(b"tail");
        trg.drain(9000..9100);
        let d = delta(&src, &trg, trg.len()).expect("a delta");
        assert!(d.len() < 200, "{}", d.len());
        assert_eq!(patch(&src, &d), trg);
        assert!(delta(&src, b"unrelated", 5).is_none());
    }

    #[test]
    fn ewah_runs_and_literals() {
        let e = ewah(&[0, 0, 5, u64::MAX], 256);
        // bit size, 3 words: [run 2 zeros + 1 literal], literal, [run 1 ones].
        assert_eq!(&e[..8], &[0, 0, 1, 0, 0, 0, 0, 3]);
        let w = |i: usize| u64::from_be_bytes(e[8 + i * 8..16 + i * 8].try_into().unwrap());
        assert_eq!(w(0), (2 << 1) | (1 << 33));
        assert_eq!(w(1), 5);
        assert_eq!(w(2), 1 | (1 << 1));
        assert_eq!(&e[32..], &2u32.to_be_bytes());
    }
}
