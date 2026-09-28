//! objects/info/commit-graph and split chains under
//! objects/info/commit-graphs, in git's format (version 1 with generation
//! data v2 and changed-path Bloom filters), and `commit-graph verify`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use git2::{ObjectType, Oid, Repository};
use sha1::Digest;

use crate::GitError;
use crate::midx::{chunk_file, chunks};

const NO_PARENT: u32 = 0x7000_0000;
const EDGE_BIT: u32 = 0x8000_0000;
const LEVEL_MAX: u64 = 0x3FFF_FFFF;
const BLOOM_MAX_PATHS: usize = 512;

struct Node {
    tree: Oid,
    parents: Vec<Oid>,
    time: u64,
}

/// How `--split` treats the existing chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    /// Merge layers per --size-multiple and --max-commits.
    Merge,
    /// Never merge (`--split=no-merge`).
    NoMerge,
    /// One new layer holding everything (`--split=replace`).
    Replace,
}

/// What `git commit-graph write` writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitGraphWrite {
    /// Commits the refs reach (--reachable); else those of `commits`,
    /// `packs`, or every pack.
    pub reachable: bool,
    /// Revisions from --stdin-commits.
    pub commits: Option<Vec<String>>,
    /// Pack names from --stdin-packs.
    pub packs: Option<Vec<String>>,
    /// Keep the commits the graph already lists (--append).
    pub append: bool,
    pub split: Option<Split>,
    /// Merge a layer when it is at most this many times the new one (2).
    pub size_multiple: Option<u64>,
    /// Merge while the new layer would pass this many commits.
    pub max_commits: Option<usize>,
    /// Remove unused layer files older than this (default now).
    pub expire_time: Option<i64>,
    /// Write Bloom filters (--changed-paths); None keeps what the graph has.
    pub changed_paths: Option<bool>,
}

fn node(odb: &git2::Odb, id: Oid) -> Option<Node> {
    let obj = odb.read(id).ok()?;
    if obj.kind() != ObjectType::Commit {
        return None;
    }
    let (mut tree, mut parents, mut time) = (None, Vec::new(), 0);
    for line in obj.data().split(|b| *b == b'\n') {
        if line.is_empty() {
            break;
        }
        let text = String::from_utf8_lossy(line);
        if let Some(h) = text.strip_prefix("tree ") {
            tree = Oid::from_str(h).ok();
        } else if let Some(h) = text.strip_prefix("parent ") {
            parents.extend(Oid::from_str(h).ok());
        } else if let Some(who) = text.strip_prefix("committer ") {
            let after = who.rsplit_once('>').map_or(who, |(_, t)| t);
            time = after
                .split_whitespace()
                .next()
                .and_then(|t| t.parse::<i64>().ok())
                .unwrap_or(0)
                .max(0) as u64;
        }
    }
    Some(Node {
        tree: tree?,
        parents,
        time,
    })
}

/// `start` and every commit they reach.
fn close(repo: &Repository, start: Vec<Oid>) -> HashMap<Oid, Node> {
    let mut out = HashMap::new();
    let Ok(odb) = repo.odb() else {
        return out;
    };
    let mut stack = start;
    while let Some(id) = stack.pop() {
        if out.contains_key(&id) {
            continue;
        }
        let Some(n) = node(&odb, id) else {
            continue;
        };
        stack.extend(n.parents.iter().copied());
        out.insert(id, n);
    }
    // Parents that could not be read cannot be listed.
    let known: HashSet<Oid> = out.keys().copied().collect();
    for n in out.values_mut() {
        n.parents.retain(|p| known.contains(p));
    }
    out
}

fn ref_tips(repo: &Repository) -> Vec<Oid> {
    repo.references()
        .map(|refs| {
            refs.flatten()
                .filter_map(|r| r.peel_to_commit().ok().map(|c| c.id()))
                .collect()
        })
        .unwrap_or_default()
}

/// Topological levels and corrected commit dates, parents first.
fn generations(nodes: &HashMap<Oid, Node>) -> HashMap<Oid, (u64, u64)> {
    let mut done: HashMap<Oid, (u64, u64)> = HashMap::new();
    for start in nodes.keys() {
        let mut stack = vec![*start];
        while let Some(&id) = stack.last() {
            if done.contains_key(&id) {
                stack.pop();
                continue;
            }
            let n = &nodes[&id];
            let pending: Vec<Oid> = n
                .parents
                .iter()
                .copied()
                .filter(|p| !done.contains_key(p))
                .collect();
            if !pending.is_empty() {
                stack.extend(pending);
                continue;
            }
            let mut level = 1;
            let mut date = n.time;
            for p in &n.parents {
                let (l, d) = done[p];
                level = level.max(l + 1);
                date = date.max(d + 1);
            }
            done.insert(id, (level.min(LEVEL_MAX), date));
            stack.pop();
        }
    }
    done
}

/// One graph file: the whole graph, or one layer of a chain.
struct Layer {
    path: PathBuf,
    hash: String,
    ids: Vec<Oid>,
    bloom: Option<u32>,
}

fn be32(d: &[u8], at: usize) -> u32 {
    d.get(at..at + 4)
        .map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap_or_default()))
}

fn read_layer(path: &Path) -> Option<Layer> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 8 + 20 || &data[..4] != b"CGPH" {
        return None;
    }
    let c = chunks(&data, 8, data[6] as usize);
    let (fan, _) = *c.get(b"OIDF")?;
    let (oidl, _) = *c.get(b"OIDL")?;
    let n = be32(&data, fan + 1020) as usize;
    let ids = (0..n)
        .filter_map(|i| Oid::from_bytes(data.get(oidl + i * 20..oidl + i * 20 + 20)?).ok())
        .collect();
    let bloom = c.get(b"BDAT").map(|(at, _)| be32(&data, *at));
    Some(Layer {
        path: path.to_owned(),
        hash: crate::pack::hex(&data[data.len() - 20..]),
        ids,
        bloom,
    })
}

fn info_dir(repo: &Repository) -> PathBuf {
    repo.commondir().join("objects/info")
}

/// The graph as git loads it: the single file if there is one, else the
/// chain, base first.
fn layers(repo: &Repository) -> Vec<Layer> {
    let info = info_dir(repo);
    if let Some(l) = read_layer(&info.join("commit-graph")) {
        return vec![l];
    }
    let dir = info.join("commit-graphs");
    std::fs::read_to_string(dir.join("commit-graph-chain"))
        .unwrap_or_default()
        .lines()
        .filter_map(|h| read_layer(&dir.join(format!("graph-{}.graph", h.trim()))))
        .collect()
}

/// Write the commit-graph for everything the refs reach (git's
/// `commit-graph write --reachable`), replacing a split chain.
pub fn write(repo: &Repository) -> Result<(), GitError> {
    write_with(
        repo,
        &CommitGraphWrite {
            reachable: true,
            ..Default::default()
        },
    )
}

/// murmur3 as git's Bloom filters use it; version 1 keeps git's
/// sign-extension of bytes over 0x7f.
fn murmur3(seed: u32, data: &[u8], v2: bool) -> u32 {
    let byte = |b: u8| if v2 { b as u32 } else { b as i8 as i32 as u32 };
    let (c1, c2) = (0xcc9e_2d51u32, 0x1b87_3593u32);
    let mut h = seed;
    let blocks = data.chunks_exact(4);
    let tail = blocks.remainder();
    for b in blocks {
        let mut k = byte(b[0]) | byte(b[1]) << 8 | byte(b[2]) << 16 | byte(b[3]) << 24;
        k = k.wrapping_mul(c1).rotate_left(15).wrapping_mul(c2);
        h ^= k;
        h = h.rotate_left(13).wrapping_mul(5).wrapping_add(0xe654_6b64);
    }
    let mut k = 0u32;
    if tail.len() >= 3 {
        k ^= byte(tail[2]) << 16;
    }
    if tail.len() >= 2 {
        k ^= byte(tail[1]) << 8;
    }
    if !tail.is_empty() {
        k ^= byte(tail[0]);
        k = k.wrapping_mul(c1).rotate_left(15).wrapping_mul(c2);
        h ^= k;
    }
    h ^= data.len() as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^ (h >> 16)
}

/// Files that differ between two trees (recursively), up to `limit + 1`.
fn changed(
    odb: &git2::Odb,
    a: Option<Oid>,
    b: Option<Oid>,
    prefix: &str,
    out: &mut Vec<String>,
    limit: usize,
) {
    let entries = |t: Option<Oid>| -> HashMap<Vec<u8>, (u32, Oid)> {
        let Some(obj) = t.and_then(|t| odb.read(t).ok()) else {
            return HashMap::new();
        };
        let data = obj.data();
        let mut m = HashMap::new();
        let mut at = 0;
        while let Some(nul) = data[at..].iter().position(|x| *x == 0) {
            let head = &data[at..at + nul];
            let Some(sp) = head.iter().position(|x| *x == b' ') else {
                break;
            };
            let mode = u32::from_str_radix(&String::from_utf8_lossy(&head[..sp]), 8).unwrap_or(0);
            let Some(id) = data
                .get(at + nul + 1..at + nul + 21)
                .and_then(|x| Oid::from_bytes(x).ok())
            else {
                break;
            };
            m.insert(head[sp + 1..].to_vec(), (mode, id));
            at += nul + 21;
        }
        m
    };
    let (ea, eb) = (entries(a), entries(b));
    let mut names: Vec<&Vec<u8>> = ea.keys().chain(eb.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        if out.len() > limit {
            return;
        }
        let (x, y) = (ea.get(name), eb.get(name));
        if x == y {
            continue;
        }
        let path = format!("{prefix}{}", String::from_utf8_lossy(name));
        let tree = |e: Option<&(u32, Oid)>| e.filter(|(m, _)| *m == 0o040000).map(|(_, id)| *id);
        let (ta, tb) = (tree(x), tree(y));
        if ta.is_some() || tb.is_some() {
            changed(odb, ta, tb, &format!("{path}/"), out, limit);
        }
        let file = |e: Option<&(u32, Oid)>| e.is_some_and(|(m, _)| *m != 0o040000);
        if file(x) || file(y) {
            out.push(path);
        }
    }
}

/// A commit's changed-path Bloom filter against its first parent.
fn bloom_filter(odb: &git2::Odb, n: &Node, parent_tree: Option<Oid>, version: u32) -> Vec<u8> {
    let mut files = Vec::new();
    changed(
        odb,
        parent_tree,
        Some(n.tree),
        "",
        &mut files,
        BLOOM_MAX_PATHS,
    );
    if files.len() > BLOOM_MAX_PATHS {
        return vec![0xff];
    }
    let mut paths: HashSet<&str> = HashSet::new();
    for f in &files {
        let mut p = f.as_str();
        paths.insert(p);
        while let Some((dir, _)) = p.rsplit_once('/') {
            paths.insert(dir);
            p = dir;
        }
    }
    let len = (paths.len() * 10).div_ceil(8).max(1);
    let mut data = vec![0u8; len];
    let bits = (len * 8) as u64;
    for p in paths {
        let h0 = murmur3(0x293a_e76f, p.as_bytes(), version == 2);
        let h1 = murmur3(0x7e64_6e2c, p.as_bytes(), version == 2);
        for i in 0..7u32 {
            let m = h0.wrapping_add(i.wrapping_mul(h1)) as u64 % bits;
            data[(m / 8) as usize] |= 1 << (m % 8);
        }
    }
    data
}

/// One graph file for `ids` (sorted), positions after `base_count`
/// commits of the `bases` layers.
fn graph_file(
    repo: &Repository,
    ids: &[Oid],
    nodes: &HashMap<Oid, Node>,
    gens: &HashMap<Oid, (u64, u64)>,
    base_pos: &HashMap<Oid, u32>,
    bases: &[String],
    bloom: Option<u32>,
) -> Result<Vec<u8>, GitError> {
    let base_count = base_pos.len() as u32;
    let pos: HashMap<Oid, u32> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, base_count + i as u32))
        .collect();
    let at = |p: &Oid| {
        pos.get(p)
            .or_else(|| base_pos.get(p))
            .copied()
            .unwrap_or(NO_PARENT)
    };
    let mut fanout = Vec::with_capacity(1024);
    for b in 0..256usize {
        let n = ids
            .iter()
            .take_while(|id| id.as_bytes()[0] as usize <= b)
            .count() as u32;
        fanout.extend_from_slice(&n.to_be_bytes());
    }
    let mut oidl = Vec::with_capacity(ids.len() * 20);
    let mut cdat = Vec::with_capacity(ids.len() * 36);
    let mut gda2 = Vec::with_capacity(ids.len() * 4);
    let mut gdo2 = Vec::new();
    let mut edge = Vec::new();
    for id in ids {
        let n = &nodes[id];
        let (level, date) = gens[id];
        oidl.extend_from_slice(id.as_bytes());
        cdat.extend_from_slice(n.tree.as_bytes());
        let p1 = n.parents.first().map_or(NO_PARENT, at);
        let p2 = match n.parents.len() {
            0 | 1 => NO_PARENT,
            2 => at(&n.parents[1]),
            _ => {
                let start = (edge.len() / 4) as u32;
                let rest = &n.parents[1..];
                for (i, p) in rest.iter().enumerate() {
                    let mut v = at(p);
                    if i == rest.len() - 1 {
                        v |= EDGE_BIT;
                    }
                    edge.extend_from_slice(&v.to_be_bytes());
                }
                EDGE_BIT | start
            }
        };
        cdat.extend_from_slice(&p1.to_be_bytes());
        cdat.extend_from_slice(&p2.to_be_bytes());
        let hi = ((level << 2) | ((n.time >> 32) & 3)) as u32;
        cdat.extend_from_slice(&hi.to_be_bytes());
        cdat.extend_from_slice(&(n.time as u32).to_be_bytes());
        let offset = date - n.time;
        if offset > 0x7FFF_FFFF {
            let slot = (gdo2.len() / 8) as u32;
            gda2.extend_from_slice(&(EDGE_BIT | slot).to_be_bytes());
            gdo2.extend_from_slice(&offset.to_be_bytes());
        } else {
            gda2.extend_from_slice(&(offset as u32).to_be_bytes());
        }
    }
    let mut parts: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"OIDF", fanout),
        (b"OIDL", oidl),
        (b"CDAT", cdat),
        (b"GDA2", gda2),
    ];
    if !gdo2.is_empty() {
        parts.push((b"GDO2", gdo2));
    }
    if !edge.is_empty() {
        parts.push((b"EDGE", edge));
    }
    if let Some(version) = bloom {
        let odb = repo.odb()?;
        let mut bidx = Vec::with_capacity(ids.len() * 4);
        let mut bdat = Vec::new();
        for v in [version, 7, 10] {
            bdat.extend_from_slice(&v.to_be_bytes());
        }
        for id in ids {
            let n = &nodes[id];
            let parent_tree = n.parents.first().and_then(|p| nodes.get(p)).map(|p| p.tree);
            bdat.extend(bloom_filter(&odb, n, parent_tree, version));
            bidx.extend_from_slice(&((bdat.len() - 12) as u32).to_be_bytes());
        }
        parts.push((b"BIDX", bidx));
        parts.push((b"BDAT", bdat));
    }
    if !bases.is_empty() {
        let mut base = Vec::new();
        for h in bases {
            base.extend(
                (0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()),
            );
        }
        parts.push((b"BASE", base));
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"CGPH");
    out.extend_from_slice(&[1, 1, parts.len() as u8, bases.len() as u8]);
    out.extend_from_slice(&chunk_file(&parts, 8));
    let digest = sha1::Sha1::digest(&out);
    out.extend_from_slice(&digest);
    Ok(out)
}

fn put(path: &Path, data: &[u8]) -> Result<(), GitError> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let lock = path.with_extension("lock");
    std::fs::write(&lock, data)?;
    std::fs::rename(&lock, path)?;
    Ok(())
}

/// The commits a write starts from, before closing over parents.
fn sources(repo: &Repository, o: &CommitGraphWrite) -> Result<Vec<Oid>, GitError> {
    if o.reachable {
        return Ok(ref_tips(repo));
    }
    if let Some(revs) = &o.commits {
        return revs
            .iter()
            .map(|r| {
                repo.revparse_single(r)
                    .and_then(|obj| obj.peel_to_commit())
                    .map(|c| c.id())
                    .map_err(|_| GitError::Other(format!("unexpected non-hex object ID: {r}")))
            })
            .collect();
    }
    let odb = repo.odb()?;
    let dir = repo.commondir().join("objects/pack");
    let idx_files: Vec<PathBuf> = match &o.packs {
        Some(names) => names
            .iter()
            .map(|n| {
                let n = Path::new(n)
                    .file_name()
                    .map_or(n.as_str(), |f| f.to_str().unwrap_or(n));
                dir.join(n).with_extension("idx")
            })
            .collect(),
        None => crate::maintenance::packs(repo)
            .into_iter()
            .map(|p| p.path.with_extension("idx"))
            .collect(),
    };
    let mut out = Vec::new();
    for idx in idx_files {
        if !idx.exists() {
            return Err(GitError::Other(format!(
                "error adding pack {}",
                idx.with_extension("pack").display()
            )));
        }
        out.extend(crate::maintenance::idx_ids(&idx).into_iter().filter(|id| {
            odb.read_header(*id)
                .is_ok_and(|(_, k)| k == ObjectType::Commit)
        }));
    }
    Ok(out)
}

/// `git commit-graph write`: one file, or with `split` a new layer on the
/// chain, merging layers as git does.
pub fn write_with(repo: &Repository, o: &CommitGraphWrite) -> Result<(), GitError> {
    let info = info_dir(repo);
    let chain_dir = info.join("commit-graphs");
    let existing = layers(repo);
    let mut start = sources(repo, o)?;
    if o.append || o.split.is_some() {
        start.extend(existing.iter().flat_map(|l| l.ids.iter().copied()));
    }
    let nodes = close(repo, start);
    let version = match crate::maintenance::cfg_int(repo, "commitGraph.changedPathsVersion", -1) {
        2 => 2,
        1 => 1,
        _ => existing.iter().find_map(|l| l.bloom).unwrap_or(1),
    };
    let bloom = match o.changed_paths {
        Some(true) => Some(version),
        Some(false) => None,
        None => existing
            .iter()
            .any(|l| l.bloom.is_some())
            .then_some(version),
    };
    let gens = generations(&nodes);
    let Some(split) = o.split else {
        if nodes.is_empty() {
            return Ok(());
        }
        let mut ids: Vec<Oid> = nodes.keys().copied().collect();
        ids.sort();
        let data = graph_file(repo, &ids, &nodes, &gens, &HashMap::new(), &[], bloom)?;
        put(&info.join("commit-graph"), &data)?;
        let _ = std::fs::remove_dir_all(&chain_dir);
        return Ok(());
    };
    let listed: HashSet<Oid> = existing
        .iter()
        .flat_map(|l| l.ids.iter().copied())
        .collect();
    let mut fresh: Vec<Oid> = nodes
        .keys()
        .filter(|id| !listed.contains(id))
        .copied()
        .collect();
    let mut kept = existing.len();
    if split == Split::Replace {
        kept = 0;
    } else if split == Split::Merge {
        let multiple = o.size_multiple.unwrap_or(2);
        let mut count = fresh.len();
        while kept > 0
            && ((existing[kept - 1].ids.len() as u64) <= multiple * count as u64
                || o.max_commits.is_some_and(|m| count > m))
        {
            count += existing[kept - 1].ids.len();
            kept -= 1;
        }
    }
    let merged = &existing[kept..];
    fresh.extend(merged.iter().flat_map(|l| l.ids.iter().copied()));
    fresh.retain(|id| nodes.contains_key(id));
    fresh.sort();
    fresh.dedup();
    if fresh.is_empty() && split != Split::Replace {
        return Ok(());
    }
    let single = info.join("commit-graph");
    let mut chain: Vec<String> = Vec::new();
    let mut base_pos: HashMap<Oid, u32> = HashMap::new();
    for l in &existing[..kept] {
        let dest = chain_dir.join(format!("graph-{}.graph", l.hash));
        if l.path != dest {
            std::fs::create_dir_all(&chain_dir)?;
            std::fs::rename(&l.path, &dest)?;
        }
        for id in &l.ids {
            let p = base_pos.len() as u32;
            base_pos.insert(*id, p);
        }
        chain.push(l.hash.clone());
    }
    let data = graph_file(repo, &fresh, &nodes, &gens, &base_pos, &chain, bloom)?;
    let hash = crate::pack::hex(&data[data.len() - 20..]);
    put(&chain_dir.join(format!("graph-{hash}.graph")), &data)?;
    chain.push(hash);
    let mut text = chain.join("\n");
    text.push('\n');
    put(&chain_dir.join("commit-graph-chain"), text.as_bytes())?;
    let _ = std::fs::remove_file(&single);
    let expire = o.expire_time.unwrap_or_else(crate::maintenance::now);
    for e in std::fs::read_dir(&chain_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(h) = name
            .strip_prefix("graph-")
            .and_then(|n| n.strip_suffix(".graph"))
        else {
            continue;
        };
        let old = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|d| d.as_secs() as i64 <= expire);
        if !chain.iter().any(|c| c == h) && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(())
}

/// How many commits the refs reach that the commit-graph lacks.
pub fn missing(repo: &Repository) -> usize {
    let listed: HashSet<Oid> = layers(repo).into_iter().flat_map(|l| l.ids).collect();
    close(repo, ref_tips(repo))
        .keys()
        .filter(|id| !listed.contains(id))
        .count()
}

/// A `git commit-graph` subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitGraphOp {
    Write(CommitGraphWrite),
    /// Check the graph (`shallow`: only its top layer).
    Verify {
        shallow: bool,
    },
}

/// Run `op`; verify's complaints.
pub fn run(repo: &Repository, op: &CommitGraphOp) -> Result<Vec<String>, GitError> {
    match op {
        CommitGraphOp::Write(o) => write_with(repo, o).map(|()| Vec::new()),
        CommitGraphOp::Verify { shallow } => Ok(verify(repo, *shallow)),
    }
}

/// `git commit-graph verify` (`shallow`: the top layer only): git's
/// complaints, none when the graph is sound or absent.
pub fn verify(repo: &Repository, shallow: bool) -> Vec<String> {
    let all = layers(repo);
    let mut out = Vec::new();
    let Ok(odb) = repo.odb() else {
        return out;
    };
    // Every layer's commits, in graph position order.
    let global: Vec<Oid> = all.iter().flat_map(|l| l.ids.iter().copied()).collect();
    let mut gen_of: HashMap<Oid, (u64, u64)> = HashMap::new();
    for (li, l) in all.iter().enumerate() {
        let Ok(data) = std::fs::read(&l.path) else {
            continue;
        };
        let check = !shallow || li + 1 == all.len();
        let (body, sum) = data.split_at(data.len() - 20);
        if check && sha1::Sha1::digest(body).as_slice() != sum {
            out.push(
                "the commit-graph file has incorrect checksum and is likely corrupt".to_owned(),
            );
        }
        let c = chunks(&data, 8, data[6] as usize);
        let (Some((fan, _)), Some((cdat, _))) = (c.get(b"OIDF"), c.get(b"CDAT")) else {
            out.push("commit-graph is missing required chunks".to_owned());
            return out;
        };
        let gda2 = c.get(b"GDA2").map(|r| r.0);
        let gdo2 = c.get(b"GDO2").map(|r| r.0);
        let edge = c.get(b"EDGE").map(|r| r.0);
        if check {
            for w in l.ids.windows(2) {
                if w[0] >= w[1] {
                    out.push(format!(
                        "commit-graph has incorrect OID order: {} then {}",
                        w[0], w[1]
                    ));
                }
            }
            for b in 0..256usize {
                let want = l
                    .ids
                    .iter()
                    .filter(|id| id.as_bytes()[0] as usize <= b)
                    .count();
                let got = be32(&data, fan + b * 4) as usize;
                if want != got {
                    out.push(format!(
                        "commit-graph has incorrect fanout value: fanout[{b}] = {got} != {want}"
                    ));
                }
            }
        }
        let pos_id = |p: u32| global.get(p as usize).copied();
        for (i, id) in l.ids.iter().enumerate() {
            let at = cdat + i * 36;
            let tree = data.get(at..at + 20).and_then(|b| Oid::from_bytes(b).ok());
            let (p1, p2) = (be32(&data, at + 20), be32(&data, at + 24));
            let hi = be32(&data, at + 28) as u64;
            let time = ((hi & 3) << 32) | be32(&data, at + 32) as u64;
            let level = hi >> 2;
            let mut parents: Vec<Option<Oid>> = Vec::new();
            if p1 != NO_PARENT {
                parents.push(pos_id(p1));
            }
            if p2 & EDGE_BIT != 0 {
                let mut e = edge.unwrap_or(0) + (p2 & !EDGE_BIT) as usize * 4;
                loop {
                    let v = be32(&data, e);
                    parents.push(pos_id(v & !EDGE_BIT));
                    if v & EDGE_BIT != 0 || e >= data.len() {
                        break;
                    }
                    e += 4;
                }
            } else if p2 != NO_PARENT {
                parents.push(pos_id(p2));
            }
            let date = match gda2 {
                Some(g) => {
                    let v = be32(&data, g + i * 4);
                    let off = if v & EDGE_BIT != 0 {
                        let at = gdo2.unwrap_or(0) + (v & !EDGE_BIT) as usize * 8;
                        data.get(at..at + 8)
                            .map_or(0, |b| u64::from_be_bytes(b.try_into().unwrap_or_default()))
                    } else {
                        v as u64
                    };
                    time + off
                }
                None => level,
            };
            gen_of.insert(*id, (level, date));
            if !check {
                continue;
            }
            let Some(real) = node(&odb, *id) else {
                out.push(format!(
                    "failed to parse commit {id} from object database for commit-graph"
                ));
                continue;
            };
            if tree != Some(real.tree) {
                out.push(format!(
                    "root tree OID for commit {id} in commit-graph is {} != {}",
                    tree.map_or_else(|| "?".to_owned(), |t| t.to_string()),
                    real.tree
                ));
            }
            for (k, want) in real.parents.iter().enumerate() {
                match parents.get(k) {
                    None => {
                        out.push(format!(
                            "commit-graph parent list for commit {id} terminates early"
                        ));
                        break;
                    }
                    Some(got) if *got != Some(*want) => out.push(format!(
                        "commit-graph parent for {id} is {} != {want}",
                        got.map_or_else(|| "?".to_owned(), |g| g.to_string())
                    )),
                    _ => {}
                }
            }
            if parents.len() > real.parents.len() {
                out.push(format!(
                    "commit-graph parent list for commit {id} is too long"
                ));
            }
            let (mut max_level, mut max_date) = (0, 0);
            for p in parents.iter().flatten() {
                if let Some((l, d)) = gen_of.get(p) {
                    max_level = max_level.max(*l);
                    max_date = max_date.max(*d);
                }
            }
            if level < (max_level + 1).min(LEVEL_MAX) {
                out.push(format!(
                    "commit-graph generation for commit {id} is {level} < {}",
                    max_level + 1
                ));
            } else if gda2.is_some() && date < max_date + 1 && !parents.is_empty() {
                out.push(format!(
                    "commit-graph generation for commit {id} is {date} < {}",
                    max_date + 1
                ));
            }
            if time != real.time {
                out.push(format!(
                    "commit date for commit {id} in commit-graph is {time} != {}",
                    real.time
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn murmur3_matches_gits_test_vectors() {
        // t/helper/test-bloom.c's murmur3 cases.
        assert_eq!(murmur3(0, b"", true), 0);
        assert_eq!(murmur3(0, b"Hello world!", true), 0x627b_0c2c);
        assert_eq!(
            murmur3(0, b"The quick brown fox jumps over the lazy dog", true),
            0x2e4f_f723
        );
    }
}
