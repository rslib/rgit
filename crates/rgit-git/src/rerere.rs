//! git's rerere: conflicts are recorded in `.git/rr-cache/<id>` and the
//! paths being resolved in `.git/MERGE_RR`, in git's formats, so either tool
//! replays what the other recorded.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use git2::{Index, Repository};
use sha1::{Digest, Sha1};

use crate::GitError;

const MARKER: usize = 7;
const PRE: u8 = 1;
const POST: u8 = 2;

#[derive(Clone)]
struct Id {
    hex: String,
    /// `None` until a variant is picked (git's -1).
    variant: Option<usize>,
}

/// MERGE_RR: each path still to resolve and its conflict; `None` once done.
type MergeRr = BTreeMap<String, Option<Id>>;

fn cache(repo: &Repository) -> PathBuf {
    repo.commondir().join("rr-cache")
}

fn file(repo: &Repository, id: &Id, name: &str) -> PathBuf {
    let dir = cache(repo).join(&id.hex);
    match id.variant.unwrap_or(0) {
        0 => dir.join(name),
        v => dir.join(format!("{name}.{v}")),
    }
}

/// Each variant's PRE/POST bits, from the files in `rr-cache/<hex>`.
fn status(repo: &Repository, hex: &str) -> Vec<u8> {
    let mut st: Vec<u8> = Vec::new();
    for e in std::fs::read_dir(cache(repo).join(hex))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        let (kind, variant) = name.split_once('.').unwrap_or((name.as_str(), "0"));
        let bit = match kind {
            "preimage" => PRE,
            "postimage" => POST,
            _ => continue,
        };
        let Ok(v) = variant.parse::<usize>() else {
            continue;
        };
        if st.len() <= v {
            st.resize(v + 1, 0);
        }
        st[v] |= bit;
    }
    st
}

/// git's is_rerere_enabled: rerere.enabled, else whether rr-cache exists.
fn enabled(repo: &Repository) -> Result<bool, GitError> {
    let on = repo.config()?.get_bool("rerere.enabled").ok();
    let dir = cache(repo);
    match on {
        Some(false) => Ok(false),
        None => Ok(dir.is_dir()),
        Some(true) => {
            std::fs::create_dir_all(&dir)?;
            Ok(true)
        }
    }
}

fn read_rr(repo: &Repository) -> MergeRr {
    let data = std::fs::read(repo.path().join("MERGE_RR")).unwrap_or_default();
    let mut rr = MergeRr::new();
    for rec in data.split(|&b| b == 0) {
        let rec = String::from_utf8_lossy(rec);
        let Some((id, path)) = rec.split_once('\t') else {
            continue;
        };
        let (hex, variant) = match id.split_once('.') {
            Some((h, v)) => (h, v.parse().unwrap_or(0)),
            None => (id, 0),
        };
        let id = Id {
            hex: hex.to_owned(),
            variant: Some(variant),
        };
        rr.insert(path.to_owned(), Some(id));
    }
    rr
}

fn write_rr(repo: &Repository, rr: &MergeRr) -> Result<(), GitError> {
    let mut out = Vec::new();
    for (path, id) in rr {
        let Some(id) = id else { continue };
        match id.variant.unwrap_or(0) {
            0 => out.extend(format!("{}\t{path}\0", id.hex).bytes()),
            v => out.extend(format!("{}.{v}\t{path}\0", id.hex).bytes()),
        }
    }
    std::fs::write(repo.path().join("MERGE_RR"), out)?;
    Ok(())
}

fn is_marker(line: &[u8], ch: u8) -> bool {
    line.len() > MARKER
        && line[..MARKER].iter().all(|&b| b == ch)
        && match ch {
            b'<' | b'>' => line[MARKER] == b' ',
            _ => line[MARKER].is_ascii_whitespace(),
        }
}

fn put_marker(out: &mut Vec<u8>, ch: u8) {
    out.extend(std::iter::repeat_n(ch, MARKER));
    out.push(b'\n');
}

struct Lines<'a>(&'a [u8]);

impl<'a> Iterator for Lines<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        if self.0.is_empty() {
            return None;
        }
        let end = self
            .0
            .iter()
            .position(|&b| b == b'\n')
            .map_or(self.0.len(), |i| i + 1);
        let (line, rest) = self.0.split_at(end);
        self.0 = rest;
        Some(line)
    }
}

/// One conflict hunk after its `<<<<<<<` line, written with bare markers and
/// the sides in sorted order (git's handle_conflict); -1 when malformed.
fn conflict(lines: &mut Lines, out: &mut Vec<u8>, hash: Option<&mut Sha1>) -> i32 {
    let (mut one, mut two) = (Vec::new(), Vec::new());
    // 0: ours, 1: theirs, 2: the diff3 base, which is dropped.
    let mut hunk = 0;
    while let Some(line) = lines.next() {
        if is_marker(line, b'<') {
            let mut nested = Vec::new();
            if conflict(lines, &mut nested, None) < 0 {
                break;
            }
            if hunk == 0 { &mut one } else { &mut two }.extend(nested);
        } else if is_marker(line, b'|') {
            if hunk != 0 {
                break;
            }
            hunk = 2;
        } else if is_marker(line, b'=') {
            if hunk == 1 {
                break;
            }
            hunk = 1;
        } else if is_marker(line, b'>') {
            if hunk != 1 {
                break;
            }
            if one > two {
                std::mem::swap(&mut one, &mut two);
            }
            put_marker(out, b'<');
            out.extend(&one);
            put_marker(out, b'=');
            out.extend(&two);
            put_marker(out, b'>');
            if let Some(h) = hash {
                h.update(&one);
                h.update([0]);
                h.update(&two);
                h.update([0]);
            }
            return 1;
        } else if hunk == 0 {
            one.extend(line);
        } else if hunk == 1 {
            two.extend(line);
        }
    }
    -1
}

/// A conflicted file with its hunks normalized, its conflict id, and whether
/// it has conflicts (-1 for unparsable hunks), as git's handle_path.
fn normalize(data: &[u8]) -> (i32, Vec<u8>, String) {
    let mut hash = Sha1::new();
    let mut out = Vec::new();
    let mut has = 0;
    let mut lines = Lines(data);
    while let Some(line) = lines.next() {
        if is_marker(line, b'<') {
            has = conflict(&mut lines, &mut out, Some(&mut hash));
            if has < 0 {
                break;
            }
        } else {
            out.extend(line);
        }
    }
    let hex = hash.finalize().iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    (has, out, hex)
}

fn workfile(repo: &Repository, path: &str) -> PathBuf {
    repo.workdir().unwrap_or(repo.path()).join(path)
}

/// git's handle_file: normalize the working tree file, optionally into `to`.
fn handle_file(repo: &Repository, path: &str, to: Option<&Path>) -> (i32, String) {
    let Ok(data) = std::fs::read(workfile(repo, path)) else {
        return (-1, String::new());
    };
    let (has, out, hex) = normalize(&data);
    if has < 0 {
        eprintln!("error: could not parse conflict hunks in '{path}'");
    } else if let Some(to) = to {
        let _ = std::fs::write(to, out);
    }
    (has, hex)
}

/// git's handle_cache: the conflict recreated from the index stages.
fn handle_cache(repo: &Repository, index: &Index, path: &str, to: Option<&Path>) -> (i32, String) {
    let mut sides: [Option<git2::IndexEntry>; 3] = [None, None, None];
    for e in index.iter().filter(|e| e.path == path.as_bytes()) {
        let stage = ((e.flags >> 12) & 3) as usize;
        if stage > 0 {
            sides[stage - 1] = Some(e);
        }
    }
    if sides.iter().all(Option::is_none) {
        return (-1, String::new());
    }
    let text: Vec<Vec<u8>> = sides
        .iter()
        .map(|e| {
            e.as_ref()
                .and_then(|e| repo.find_blob(e.id).ok())
                .map(|b| b.content().to_vec())
                .unwrap_or_default()
        })
        .collect();
    let mut o = git2::MergeFileOptions::new();
    o.our_label("ours").their_label("theirs");
    let Ok(merged) = git2::merge_file(
        &input(&text[0], path),
        &input(&text[1], path),
        &input(&text[2], path),
        Some(&mut o),
    ) else {
        return (-1, String::new());
    };
    let (has, out, hex) = normalize(merged.content());
    if has > 0
        && let Some(to) = to
    {
        let _ = std::fs::write(to, out);
    }
    (has, hex)
}

fn input<'a>(text: &'a [u8], path: &str) -> git2::MergeFileInput<'a> {
    let mut i = git2::MergeFileInput::new();
    i.content(text).path(path);
    i
}

/// Replay variant `id` onto `cur`: preimage as base, the current conflict
/// as ours, postimage as theirs (git's try_merge).
fn try_merge(repo: &Repository, id: &Id, path: &str, cur: &[u8]) -> Option<Vec<u8>> {
    let base = std::fs::read(file(repo, id, "preimage")).ok()?;
    let other = std::fs::read(file(repo, id, "postimage")).ok()?;
    let merged = git2::merge_file(
        &input(&base, path),
        &input(cur, path),
        &input(&other, path),
        None,
    )
    .ok()?;
    merged.is_automergeable().then(|| merged.content().to_vec())
}

/// git's merge(): replay a recorded resolution into the working tree file.
fn replay(repo: &Repository, id: &Id, path: &str) -> bool {
    let this = file(repo, id, "thisimage");
    if handle_file(repo, path, Some(&this)).0 < 0 {
        return false;
    }
    let Ok(cur) = std::fs::read(&this) else {
        return false;
    };
    let Some(result) = try_merge(repo, id, path, &cur) else {
        return false;
    };
    if let Ok(f) = std::fs::File::options()
        .append(true)
        .open(file(repo, id, "postimage"))
    {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    std::fs::write(workfile(repo, path), result).is_ok()
}

fn remove_variant(repo: &Repository, id: &Id) {
    let _ = std::fs::remove_file(file(repo, id, "postimage"));
    let _ = std::fs::remove_file(file(repo, id, "preimage"));
}

/// The paths with both our and their side staged as regular files (git's
/// find_conflict), sorted.
fn conflicts(index: &Index) -> Result<Vec<String>, GitError> {
    let regular =
        |e: &Option<git2::IndexEntry>| e.as_ref().is_some_and(|e| e.mode & 0o170000 == 0o100000);
    let mut out = Vec::new();
    for c in index.conflicts()? {
        let c = c?;
        if regular(&c.our) && regular(&c.their) {
            out.push(String::from_utf8_lossy(&c.our.expect("checked").path).into_owned());
        }
    }
    out.sort();
    Ok(out)
}

fn load_index(repo: &Repository) -> Result<Index, GitError> {
    let mut index = repo.index()?;
    index.read(false)?;
    Ok(index)
}

fn autoupdate(repo: &Repository, flag: Option<bool>) -> bool {
    flag.or_else(|| repo.config().ok()?.get_bool("rerere.autoUpdate").ok())
        .unwrap_or(false)
}

/// git's do_rerere_one_path: record a resolution, replay one, or record the
/// preimage of a new conflict. `true` when the path should be staged.
fn one_path(
    repo: &Repository,
    path: &str,
    id: &mut Option<Id>,
    update: bool,
    say: &mut Vec<String>,
) -> bool {
    let Some(mut cur) = id.clone() else {
        return false;
    };
    if cur.variant.is_some() && handle_file(repo, path, None).0 == 0 {
        let _ = std::fs::copy(workfile(repo, path), file(repo, &cur, "postimage"));
        say.push(format!("Recorded resolution for '{path}'."));
        *id = None;
        return false;
    }
    let st = status(repo, &cur.hex);
    for (v, bits) in st.iter().enumerate() {
        if bits & (PRE | POST) != PRE | POST {
            continue;
        }
        let vid = Id {
            hex: cur.hex.clone(),
            variant: Some(v),
        };
        if !replay(repo, &vid, path) {
            continue;
        }
        if cur.variant.is_some_and(|c| c != v) {
            remove_variant(repo, &cur);
        }
        if !update {
            say.push(format!("Resolved '{path}' using previous resolution."));
        }
        *id = None;
        return update;
    }
    let v = cur
        .variant
        .unwrap_or_else(|| st.iter().position(|&s| s == 0).unwrap_or(st.len()));
    cur.variant = Some(v);
    let _ = std::fs::create_dir_all(cache(repo).join(&cur.hex));
    handle_file(repo, path, Some(&file(repo, &cur, "preimage")));
    let _ = std::fs::remove_file(file(repo, &cur, "postimage"));
    say.push(format!("Recorded preimage for '{path}'"));
    *id = Some(cur);
    false
}

/// `git rerere`: record the preimages of new conflicts, replay known
/// resolutions and record the ones the user made. Hooked in wherever git runs
/// it; `flag` is `--[no-]rerere-autoupdate`. Returns git's messages (for
/// stderr), so a caller can place them among its own.
pub(crate) fn run(repo: &Repository, flag: Option<bool>) -> Result<Vec<String>, GitError> {
    let mut say = Vec::new();
    if repo.workdir().is_none() || !enabled(repo)? {
        return Ok(say);
    }
    let update = autoupdate(repo, flag);
    let mut rr = read_rr(repo);
    let mut index = load_index(repo)?;
    for path in conflicts(&index)? {
        let (has, hex) = handle_file(repo, &path, None);
        if has != 0
            && let Some(Some(old)) = rr.remove(&path)
        {
            remove_variant(repo, &old);
        }
        if has < 1 {
            continue;
        }
        rr.insert(path, Some(Id { hex, variant: None }));
    }
    let mut staged = Vec::new();
    for (path, id) in rr.iter_mut() {
        if one_path(repo, path, id, update, &mut say) {
            staged.push(path.clone());
        }
    }
    if !staged.is_empty() {
        for path in &staged {
            index.add_path(Path::new(path))?;
            say.push(format!("Staged '{path}' using previous resolution."));
        }
        index.write()?;
    }
    write_rr(repo, &rr)?;
    Ok(say)
}

/// [`run`], its messages on stderr; a failure is only reported, as git's
/// callers ignore it.
pub(crate) fn say(repo: &Repository, flag: Option<bool>) {
    match run(repo, flag) {
        Ok(say) => say.iter().for_each(|l| eprintln!("{l}")),
        Err(e) => eprintln!("error: {e}"),
    }
}

/// [`run`] for a stop: its messages joined for the stop's report.
pub(crate) fn report(repo: &Repository, flag: Option<bool>) -> String {
    match run(repo, flag) {
        Ok(say) => say.iter().map(|l| format!("{l}\n")).collect(),
        Err(e) => format!("error: {e}\n"),
    }
}

/// `git rerere clear`: forget the preimages of conflicts not yet resolved.
pub(crate) fn clear(repo: &Repository) -> Result<(), GitError> {
    if !enabled(repo)? {
        return Ok(());
    }
    for id in read_rr(repo).into_values().flatten() {
        if status(repo, &id.hex)
            .get(id.variant.unwrap_or(0))
            .is_some_and(|s| s & POST != 0)
        {
            continue;
        }
        let _ = std::fs::remove_file(file(repo, &id, "thisimage"));
        remove_variant(repo, &id);
        let _ = std::fs::remove_dir(cache(repo).join(&id.hex));
    }
    let _ = std::fs::remove_file(repo.path().join("MERGE_RR"));
    Ok(())
}

/// `git rerere forget <paths>`: drop the resolutions recorded for these
/// conflicts, so the next resolution is recorded afresh.
fn forget(repo: &Repository, paths: &[String]) -> Result<(), GitError> {
    if paths.is_empty() {
        eprintln!("warning: 'git rerere forget' without paths is deprecated");
    }
    let hit = |p: &str| {
        paths.is_empty()
            || paths.iter().any(|s| {
                let s = s.trim_end_matches('/');
                s == "." || p == s || p.starts_with(&format!("{s}/"))
            })
    };
    let mut index = load_index(repo)?;
    if !enabled(repo)? {
        return Ok(());
    }
    let mut rr = read_rr(repo);
    crate::git_repo::unmerge_index(&mut index, &hit)?;
    for path in conflicts(&index)?.into_iter().filter(|p| hit(p)) {
        let (has, hex) = handle_cache(repo, &index, &path, None);
        if has < 1 {
            eprintln!("error: could not parse conflict hunks in '{path}'");
            continue;
        }
        let mut id = Id { hex, variant: None };
        let st = status(repo, &id.hex);
        let found = (0..st.len()).find(|&v| {
            if st[v] & POST == 0 {
                return false;
            }
            id.variant = Some(v);
            let this = file(repo, &id, "thisimage");
            handle_cache(repo, &index, &path, Some(&this));
            std::fs::read(&this).is_ok_and(|cur| try_merge(repo, &id, &path, &cur).is_some())
        });
        let Some(v) = found else {
            eprintln!("error: no remembered resolution for '{path}'");
            continue;
        };
        id.variant = Some(v);
        if std::fs::remove_file(file(repo, &id, "postimage")).is_err() {
            eprintln!("error: no remembered resolution for '{path}'");
            continue;
        }
        handle_cache(repo, &index, &path, Some(&file(repo, &id, "preimage")));
        eprintln!("Updated preimage for '{path}'");
        rr.insert(path.clone(), Some(id));
        eprintln!("Forgot resolution for '{path}'");
    }
    index.write()?;
    write_rr(repo, &rr)
}

/// `git rerere remaining`: MERGE_RR's paths not yet resolved and the
/// conflicts rerere cannot handle.
fn remaining(repo: &Repository) -> Result<Vec<String>, GitError> {
    let mut rr: BTreeMap<String, bool> = read_rr(repo).into_keys().map(|p| (p, true)).collect();
    let index = load_index(repo)?;
    let three = conflicts(&index)?;
    for e in index.iter() {
        let path = String::from_utf8_lossy(&e.path).into_owned();
        if (e.flags >> 12) & 3 == 0 {
            if let Some(open) = rr.get_mut(&path) {
                *open = false;
            }
        } else if !three.contains(&path) {
            rr.insert(path, true);
        }
    }
    Ok(rr
        .into_iter()
        .filter(|(_, open)| *open)
        .map(|(p, _)| p)
        .collect())
}

/// `git rerere diff`: each MERGE_RR path against its preimage.
fn diff(repo: &Repository) -> Result<String, GitError> {
    let mut out = String::new();
    for (path, id) in read_rr(repo) {
        let Some(id) = id else { continue };
        let pre = std::fs::read(file(repo, &id, "preimage")).unwrap_or_default();
        let cur = std::fs::read(workfile(repo, &path)).map_err(|_| {
            GitError::Other(format!(
                "unable to generate diff for '{}'",
                cache(repo).join(&id.hex).display()
            ))
        })?;
        let _ = writeln!(out, "--- a/{path}\n+++ b/{path}");
        let patch = git2::Patch::from_buffers(&pre, None, &cur, None, None)?;
        for h in 0..patch.num_hunks() {
            let (hunk, lines) = patch.hunk(h)?;
            out.push_str(&String::from_utf8_lossy(hunk.header()));
            for l in 0..lines {
                let line = patch.line_in_hunk(h, l)?;
                if matches!(line.origin(), ' ' | '+' | '-') {
                    out.push(line.origin());
                }
                out.push_str(&String::from_utf8_lossy(line.content()));
            }
        }
    }
    Ok(out)
}

/// `git rerere gc`: forget resolutions unused for gc.rerereResolved (60
/// days) and unresolved conflicts older than gc.rerereUnresolved (15 days).
pub(crate) fn gc(repo: &Repository) -> Result<(), GitError> {
    if !enabled(repo)? {
        return Ok(());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let expiry = |key: &str, days: i64| -> Result<i64, GitError> {
        match crate::maintenance::cfg_str(repo, key) {
            None => Ok(now - days * 86400),
            Some(v) => match v.trim().parse::<i64>() {
                Ok(n) => Ok(now - n * 86400),
                Err(_) => crate::maintenance::cutoff(&v),
            },
        }
    };
    let resolved = expiry("gc.rerereResolved", 60)?;
    let unresolved = expiry("gc.rerereUnresolved", 15)?;
    let mtime = |p: PathBuf| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
    };
    for e in std::fs::read_dir(cache(repo))?.flatten() {
        let hex = e.file_name().to_string_lossy().into_owned();
        if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let mut empty = true;
        for (v, bits) in status(repo, &hex).into_iter().enumerate() {
            let id = Id {
                hex: hex.clone(),
                variant: Some(v),
            };
            let gone = match (
                mtime(file(repo, &id, "postimage")),
                mtime(file(repo, &id, "preimage")),
            ) {
                (Some(t), _) => t < resolved,
                (None, Some(t)) => t < unresolved,
                (None, None) => false,
            };
            if gone {
                let _ = std::fs::remove_file(file(repo, &id, "thisimage"));
                remove_variant(repo, &id);
            } else if bits != 0 {
                empty = false;
            }
        }
        if empty {
            let _ = std::fs::remove_dir(e.path());
        }
    }
    Ok(())
}

/// The `git rerere` command: no argument, `clear`, `forget <paths>`,
/// `status`, `remaining`, `diff` or `gc`. Returns what goes to stdout.
pub(crate) fn command(
    repo: &Repository,
    args: &[String],
    flag: Option<bool>,
) -> Result<String, GitError> {
    let lines = |v: Vec<String>| v.iter().map(|p| format!("{p}\n")).collect::<String>();
    match args.first().map(String::as_str) {
        None => run(repo, flag).map(|say| {
            say.iter().for_each(|l| eprintln!("{l}"));
            String::new()
        }),
        Some("forget") => forget(repo, &args[1..]).map(|()| String::new()),
        Some("clear") => clear(repo).map(|()| String::new()),
        Some("gc") => gc(repo).map(|()| String::new()),
        Some(_) if !enabled(repo)? => Ok(String::new()),
        Some("status") => Ok(lines(read_rr(repo).into_keys().collect())),
        Some("remaining") => Ok(lines(remaining(repo)?)),
        Some("diff") => diff(repo),
        Some(other) => Err(GitError::Other(format!(
            "unknown rerere subcommand '{other}'; use clear, forget, status, remaining, diff or gc"
        ))),
    }
}
