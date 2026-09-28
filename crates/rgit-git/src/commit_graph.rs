//! objects/info/commit-graph, in git's format (version 1 with generation
//! data v2), for every commit reachable from the refs.

use std::collections::HashMap;

use git2::{Oid, Repository};
use sha1::Digest;

use crate::GitError;

const NO_PARENT: u32 = 0x7000_0000;
const EDGE_BIT: u32 = 0x8000_0000;
const LEVEL_MAX: u64 = 0x3FFF_FFFF;

struct Node {
    tree: Oid,
    parents: Vec<Oid>,
    time: u64,
}

/// Every commit reachable from the refs, with what the graph records.
fn commits(repo: &Repository) -> HashMap<Oid, Node> {
    let mut out = HashMap::new();
    let mut stack: Vec<Oid> = Vec::new();
    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            if let Ok(c) = r.peel_to_commit() {
                stack.push(c.id());
            }
        }
    }
    while let Some(id) = stack.pop() {
        if out.contains_key(&id) {
            continue;
        }
        let Ok(c) = repo.find_commit(id) else {
            continue;
        };
        let parents: Vec<Oid> = c.parent_ids().collect();
        stack.extend(parents.iter().copied());
        out.insert(
            id,
            Node {
                tree: c.tree_id(),
                parents,
                time: c.committer().when().seconds().max(0) as u64,
            },
        );
    }
    // Parents that could not be read cannot be listed.
    let known: std::collections::HashSet<Oid> = out.keys().copied().collect();
    for n in out.values_mut() {
        n.parents.retain(|p| known.contains(p));
    }
    out
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

/// Write the commit-graph for everything the refs reach (git's
/// `commit-graph write --reachable`), replacing a split chain.
pub fn write(repo: &Repository) -> Result<(), GitError> {
    let nodes = commits(repo);
    let info = repo.commondir().join("objects/info");
    if nodes.is_empty() {
        return Ok(());
    }
    let gens = generations(&nodes);
    let mut ids: Vec<Oid> = nodes.keys().copied().collect();
    ids.sort();
    let pos: HashMap<Oid, u32> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i as u32))
        .collect();

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
    for id in &ids {
        let n = &nodes[id];
        let (level, date) = gens[id];
        oidl.extend_from_slice(id.as_bytes());
        cdat.extend_from_slice(n.tree.as_bytes());
        let p1 = n.parents.first().map_or(NO_PARENT, |p| pos[p]);
        let p2 = match n.parents.len() {
            0 | 1 => NO_PARENT,
            2 => pos[&n.parents[1]],
            _ => {
                let at = (edge.len() / 4) as u32;
                let rest = &n.parents[1..];
                for (i, p) in rest.iter().enumerate() {
                    let mut v = pos[p];
                    if i == rest.len() - 1 {
                        v |= EDGE_BIT;
                    }
                    edge.extend_from_slice(&v.to_be_bytes());
                }
                EDGE_BIT | at
            }
        };
        cdat.extend_from_slice(&p1.to_be_bytes());
        cdat.extend_from_slice(&p2.to_be_bytes());
        let hi = ((level << 2) | ((n.time >> 32) & 3)) as u32;
        cdat.extend_from_slice(&hi.to_be_bytes());
        cdat.extend_from_slice(&(n.time as u32).to_be_bytes());
        let offset = date - n.time;
        if offset > 0x7FFF_FFFF {
            let at = (gdo2.len() / 8) as u32;
            gda2.extend_from_slice(&(EDGE_BIT | at).to_be_bytes());
            gdo2.extend_from_slice(&offset.to_be_bytes());
        } else {
            gda2.extend_from_slice(&(offset as u32).to_be_bytes());
        }
    }
    let mut chunks: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"OIDF", fanout),
        (b"OIDL", oidl),
        (b"CDAT", cdat),
        (b"GDA2", gda2),
    ];
    if !gdo2.is_empty() {
        chunks.push((b"GDO2", gdo2));
    }
    if !edge.is_empty() {
        chunks.push((b"EDGE", edge));
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"CGPH");
    out.extend_from_slice(&[1, 1, chunks.len() as u8, 0]);
    let mut offset = (8 + (chunks.len() + 1) * 12) as u64;
    for (id, data) in &chunks {
        out.extend_from_slice(*id);
        out.extend_from_slice(&offset.to_be_bytes());
        offset += data.len() as u64;
    }
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&offset.to_be_bytes());
    for (_, data) in &chunks {
        out.extend_from_slice(data);
    }
    let digest = sha1::Sha1::digest(&out);
    out.extend_from_slice(&digest);

    std::fs::create_dir_all(&info)?;
    let lock = info.join("commit-graph.lock");
    std::fs::write(&lock, out)?;
    std::fs::rename(&lock, info.join("commit-graph"))?;
    let _ = std::fs::remove_dir_all(info.join("commit-graphs"));
    Ok(())
}

/// How many commits the refs reach that the commit-graph lacks.
pub fn missing(repo: &Repository) -> usize {
    let listed = listed(repo);
    commits(repo)
        .keys()
        .filter(|id| !listed.contains(id))
        .count()
}

/// The commits the single-file commit-graph lists.
fn listed(repo: &Repository) -> std::collections::HashSet<Oid> {
    let Ok(data) = std::fs::read(repo.commondir().join("objects/info/commit-graph")) else {
        return Default::default();
    };
    let n_chunks = data.get(6).copied().unwrap_or(0) as usize;
    let chunk = |want: &[u8]| {
        (0..n_chunks).find_map(|i| {
            let at = 8 + i * 12;
            if data.get(at..at + 4)? != want {
                return None;
            }
            Some(u64::from_be_bytes(data.get(at + 4..at + 12)?.try_into().ok()?) as usize)
        })
    };
    let (Some(fan), Some(oidl)) = (chunk(b"OIDF"), chunk(b"OIDL")) else {
        return Default::default();
    };
    let n = data.get(fan + 1020..fan + 1024).map_or(0, |b| {
        u32::from_be_bytes(b.try_into().unwrap_or_default()) as usize
    });
    (0..n)
        .filter_map(|i| Oid::from_bytes(data.get(oidl + i * 20..oidl + i * 20 + 20)?).ok())
        .collect()
}
