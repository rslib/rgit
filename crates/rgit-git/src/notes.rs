//! git's notes merge (notes-merge.c) and the note copying of rewriting
//! commands (notes.rewriteRef), over libgit2's objects.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use git2::{Oid, Repository, Tree};

use crate::GitError;

/// The notes of a notes tree: annotated object to note blob, fanout folded.
fn note_map(tree: Option<&Tree>) -> Result<BTreeMap<Oid, Oid>, GitError> {
    let mut map = BTreeMap::new();
    let Some(tree) = tree else {
        return Ok(map);
    };
    tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
        if e.kind() == Some(git2::ObjectType::Blob) {
            let name = format!("{}{}", root.replace('/', ""), e.name().unwrap_or(""));
            if let Some(obj) = Oid::from_str(&name).ok().filter(|_| name.len() == 40) {
                map.insert(obj, e.id());
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(map)
}

/// Write the notes as git's write_notes_tree lays them out: a note's fanout
/// grows by one directory level while every one of the 16 nibbles at the
/// next even 16-tree level holds two or more notes (notes.c determine_fanout).
// ponytail: drops non-note entries, and does not keep a fanout that only
// unread subtrees of the old tree would have held up.
fn write_map(repo: &Repository, map: &BTreeMap<Oid, Oid>) -> Result<Oid, GitError> {
    let hexes: Vec<(String, Oid)> = map.iter().map(|(o, b)| (o.to_string(), *b)).collect();
    let full = |prefix: &str| {
        let under: Vec<&str> = hexes
            .iter()
            .map(|(h, _)| h.as_str())
            .filter(|h| h.starts_with(prefix))
            .collect();
        (0..16u32).all(|n| {
            let c = char::from_digit(n, 16).unwrap_or('0');
            under
                .iter()
                .filter(|h| h.as_bytes()[prefix.len()] == c as u8)
                .nth(1)
                .is_some()
        })
    };
    let mut fanouts = std::collections::HashMap::new();
    let mut paths = Vec::new();
    for (hex, blob) in &hexes {
        let mut fanout = 0;
        while *fanouts
            .entry(hex[..fanout * 2].to_owned())
            .or_insert_with(|| full(&hex[..fanout * 2]))
        {
            fanout += 1;
        }
        let mut path = String::new();
        for i in 0..fanout {
            path.push_str(&hex[i * 2..i * 2 + 2]);
            path.push('/');
        }
        path.push_str(&hex[fanout * 2..]);
        paths.push((path, *blob));
    }
    write_paths(repo, &paths)
}

/// The tree of blobs at `/`-separated `paths`.
fn write_paths(repo: &Repository, paths: &[(String, Oid)]) -> Result<Oid, GitError> {
    let mut b = repo.treebuilder(None)?;
    let mut dirs: BTreeMap<&str, Vec<(String, Oid)>> = BTreeMap::new();
    for (path, blob) in paths {
        match path.split_once('/') {
            Some((dir, rest)) => dirs.entry(dir).or_default().push((rest.to_owned(), *blob)),
            None => {
                b.insert(path, *blob, 0o100644)?;
            }
        }
    }
    for (dir, sub) in dirs {
        b.insert(dir, write_paths(repo, &sub)?, 0o040000)?;
    }
    Ok(b.write()?)
}

fn blob(repo: &Repository, id: Option<Oid>) -> Vec<u8> {
    id.and_then(|id| repo.find_blob(id).ok())
        .map_or(Vec::new(), |b| b.content().to_vec())
}

/// git's combine_notes_* for a note `new` added over an existing `cur`;
/// None keeps `cur`.
fn combine(mode: &str, cur: &[u8], new: &[u8]) -> Option<Vec<u8>> {
    match mode {
        "ignore" => None,
        "overwrite" => Some(new.to_vec()),
        "cat_sort_uniq" => {
            let text = [cur, b"\n", new].concat();
            let mut lines: Vec<&[u8]> = text
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty())
                .collect();
            lines.sort_unstable();
            lines.dedup();
            Some(lines.iter().flat_map(|l| [*l, b"\n"].concat()).collect())
        }
        _ if cur.is_empty() => Some(new.to_vec()),
        _ if new.is_empty() => None,
        _ => {
            let cur = cur.strip_suffix(b"\n").unwrap_or(cur);
            Some([cur, b"\n\n", new].concat())
        }
    }
}

fn worktree_dir(repo: &Repository) -> PathBuf {
    repo.path().join("NOTES_MERGE_WORKTREE")
}

/// The worktree as git names it in messages: `.git/NOTES_MERGE_WORKTREE`
/// from the top level.
fn shown(repo: &Repository, path: &std::path::Path) -> String {
    repo.workdir()
        .and_then(|w| path.strip_prefix(w).ok())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// `git notes merge`: merge `remote_ref` into `local_ref` with the merge
/// base, as git prints at `verbosity` (2 is the default, -q 1, -v 3). Its
/// stdout, and git's stderr message and exit code when it stops: 1 with
/// conflicts left in NOTES_MERGE_WORKTREE, 128 when an earlier one is
/// still there.
pub(crate) fn merge(
    repo: &Repository,
    local_ref: &str,
    remote_ref: &str,
    strategy: &str,
    verbosity: u8,
) -> Result<(String, Option<(String, i32)>), GitError> {
    let mut out = String::new();
    let say = |out: &mut String, level: u8, line: String| {
        if verbosity >= level {
            out.push_str(&line);
            out.push('\n');
        }
    };
    let reflog = format!("notes: Merged notes from {remote_ref} into {local_ref}");
    let Ok(remote) = repo.refname_to_id(remote_ref) else {
        return Ok((out, None));
    };
    let Ok(local) = repo.refname_to_id(local_ref) else {
        repo.reference(local_ref, remote, true, &reflog)?;
        return Ok((out, None));
    };
    let bases = repo.merge_bases(local, remote).ok();
    let base = bases.as_ref().and_then(|b| b.iter().next().copied());
    match (base, bases.as_ref().map_or(0, |b| b.len())) {
        (None, _) => say(
            &mut out,
            4,
            "No merge base found; doing history-less merge".into(),
        ),
        (Some(b), 1) => say(&mut out, 4, format!("One merge base found ({:.7})", b)),
        (Some(b), _) => say(
            &mut out,
            3,
            format!("Multiple merge bases found. Using the first ({:.7})", b),
        ),
    }
    let short = |o: Option<Oid>| o.map_or_else(|| "0".repeat(7), |o| format!("{o:.7}"));
    say(
        &mut out,
        4,
        format!(
            "Merging remote commit {} into local commit {} with merge-base {}",
            short(Some(remote)),
            short(Some(local)),
            short(base)
        ),
    );
    if base == Some(remote) {
        say(&mut out, 2, "Already up to date.".into());
        return Ok((out, None));
    }
    if base == Some(local) {
        say(&mut out, 2, "Fast-forward".into());
        repo.reference(local_ref, remote, true, &reflog)?;
        return Ok((out, None));
    }
    let tree_of = |id: Oid| repo.find_commit(id).and_then(|c| c.tree());
    let base_notes = match base {
        Some(b) => note_map(Some(&tree_of(b)?))?,
        None => BTreeMap::new(),
    };
    let local_notes = note_map(Some(&tree_of(local)?))?;
    let remote_notes = note_map(Some(&tree_of(remote)?))?;
    let mut result = local_notes.clone();
    let mut conflicts = Vec::new();
    let objs: std::collections::BTreeSet<Oid> = base_notes
        .keys()
        .chain(remote_notes.keys())
        .copied()
        .collect();
    let dir = worktree_dir(repo);
    for obj in objs {
        let (b, l, r) = (
            base_notes.get(&obj).copied(),
            local_notes.get(&obj).copied(),
            remote_notes.get(&obj).copied(),
        );
        if b == r || l == r {
            continue;
        }
        if b == l {
            match r {
                Some(r) => result.insert(obj, r),
                None => result.remove(&obj),
            };
            continue;
        }
        let (lt, rt) = (blob(repo, l), blob(repo, r));
        let (verb, mode) = match strategy {
            "ours" => ("Using local notes for", "ours"),
            "theirs" => ("Using remote notes for", "theirs"),
            "union" => ("Concatenating local and remote notes for", "concatenate"),
            "cat_sort_uniq" => (
                "Concatenating unique lines in local and remote notes for",
                "cat_sort_uniq",
            ),
            _ => ("Auto-merging notes for", "manual"),
        };
        say(&mut out, 2, format!("{verb} {obj}"));
        if mode != "manual" {
            let merged = match (mode, l, r) {
                ("ours", ..) => continue,
                ("theirs", _, None) => {
                    result.remove(&obj);
                    continue;
                }
                ("theirs", ..) | (_, None, _) => rt,
                (_, _, None) => continue,
                _ => combine(mode, &lt, &rt).unwrap_or(lt),
            };
            result.insert(obj, repo.blob(&merged)?);
            continue;
        }
        if conflicts.is_empty() && std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
            let message = format!(
                "fatal: You have not concluded your previous notes merge ({} exists).\nPlease, \
                 use 'git notes merge --commit' or 'git notes merge --abort' to commit/abort the \
                 previous merge before you start a new notes merge.",
                shown(repo, &repo.path().join("NOTES_MERGE_*"))
            );
            return Ok((out, Some((message, 128))));
        }
        std::fs::create_dir_all(&dir)?;
        let text = match (l, r) {
            (None, _) => {
                say(
                    &mut out,
                    1,
                    format!(
                        "CONFLICT (delete/modify): Notes for object {obj} deleted in {local_ref} and modified in {remote_ref}. Version from {remote_ref} left in tree."
                    ),
                );
                rt
            }
            (_, None) => {
                say(
                    &mut out,
                    1,
                    format!(
                        "CONFLICT (delete/modify): Notes for object {obj} deleted in {remote_ref} and modified in {local_ref}. Version from {local_ref} left in tree."
                    ),
                );
                lt
            }
            _ => {
                let reason = if b.is_none() { "add/add" } else { "content" };
                say(
                    &mut out,
                    1,
                    format!("CONFLICT ({reason}): Merge conflict in notes for object {obj}"),
                );
                let bt = blob(repo, b);
                let input = |data| {
                    let mut i = git2::MergeFileInput::new();
                    i.content(data);
                    i
                };
                let mut o = git2::MergeFileOptions::new();
                o.our_label(local_ref).their_label(remote_ref);
                git2::merge_file(&input(&bt), &input(&lt), &input(&rt), Some(&mut o))?
                    .content()
                    .to_vec()
            }
        };
        std::fs::write(dir.join(obj.to_string()), text)?;
        result.remove(&obj);
        conflicts.push(obj);
    }
    let mut msg = format!("Merged notes from {remote_ref} into {local_ref}");
    if !conflicts.is_empty() {
        msg.push_str("\n\nConflicts:\n");
        for c in &conflicts {
            let _ = writeln!(msg, "\t{c}");
        }
    }
    let changed = result != local_notes;
    say(
        &mut out,
        4,
        format!(
            "Merge result: {} unmerged notes and a {} notes tree",
            conflicts.len(),
            if changed { "dirty" } else { "clean" }
        ),
    );
    let tree = repo.find_tree(write_map(repo, &result)?)?;
    let sig = crate::git_repo::ident_signature(repo, true)?;
    let author = crate::git_repo::ident_signature(repo, false)?;
    let parents = [&repo.find_commit(local)?, &repo.find_commit(remote)?];
    let commit = repo.commit(None, &author, &sig, &msg, &tree, &parents)?;
    if conflicts.is_empty() {
        repo.reference(local_ref, commit, true, &reflog)?;
        return Ok((out, None));
    }
    std::fs::write(
        repo.path().join("NOTES_MERGE_PARTIAL"),
        format!("{commit}\n"),
    )?;
    std::fs::write(
        repo.path().join("NOTES_MERGE_REF"),
        format!("ref: {local_ref}\n"),
    )?;
    let message = format!(
        "Automatic notes merge failed. Fix conflicts in {} and commit the result with 'git \
         notes merge --commit', or abort the merge with 'git notes merge --abort'.",
        shown(repo, &dir)
    );
    Ok((out, Some((message, 1))))
}

/// `git notes merge --commit` (or `--abort` when `commit` is false): commit
/// the resolved notes in NOTES_MERGE_WORKTREE onto the partial merge, then
/// clean up.
pub(crate) fn merge_finish(
    repo: &Repository,
    commit: bool,
    verbosity: u8,
) -> Result<String, GitError> {
    let mut out = String::new();
    let dir = worktree_dir(repo);
    let read = |name: &str| std::fs::read_to_string(repo.path().join(name));
    if commit {
        let partial = read("NOTES_MERGE_PARTIAL")
            .ok()
            .and_then(|s| Oid::from_str(s.trim()).ok())
            .ok_or_else(|| GitError::Other("failed to read ref NOTES_MERGE_PARTIAL".into()))?;
        let partial = repo.find_commit(partial).map_err(|_| {
            GitError::Other("could not find commit from NOTES_MERGE_PARTIAL.".into())
        })?;
        let local_ref = read("NOTES_MERGE_REF")
            .ok()
            .and_then(|s| s.trim().strip_prefix("ref: ").map(str::to_owned))
            .ok_or_else(|| GitError::Other("failed to resolve NOTES_MERGE_REF".into()))?;
        let mut notes = note_map(Some(&partial.tree()?))?;
        if verbosity >= 3 {
            let _ = writeln!(
                out,
                "Committing notes in notes merge worktree at {}",
                shown(repo, &dir)
            );
        }
        let mut files: Vec<_> = std::fs::read_dir(&dir)?.flatten().collect();
        files.sort_by_key(std::fs::DirEntry::file_name);
        for f in files {
            let name = f.file_name().to_string_lossy().into_owned();
            let Some(obj) = Oid::from_str(&name).ok().filter(|_| name.len() == 40) else {
                continue;
            };
            let id = repo.blob(&std::fs::read(f.path())?)?;
            if verbosity >= 4 {
                let _ = writeln!(out, "Added resolved note for object {obj}: {id}");
            }
            notes.insert(obj, id);
        }
        let tree = repo.find_tree(write_map(repo, &notes)?)?;
        let parents: Vec<git2::Commit> = partial.parents().collect();
        let sig = crate::git_repo::ident_signature(repo, true)?;
        let author = crate::git_repo::ident_signature(repo, false)?;
        let message = String::from_utf8_lossy(partial.message_bytes()).into_owned();
        let id = repo.commit(
            None,
            &author,
            &sig,
            &message,
            &tree,
            &parents.iter().collect::<Vec<_>>(),
        )?;
        let subject = partial
            .summary()
            .ok()
            .flatten()
            .unwrap_or("")
            .trim()
            .to_owned();
        repo.reference(&local_ref, id, true, &format!("notes: {subject}"))?;
    }
    let _ = std::fs::remove_file(repo.path().join("NOTES_MERGE_PARTIAL"));
    let _ = std::fs::remove_file(repo.path().join("NOTES_MERGE_REF"));
    if verbosity >= 3 {
        let _ = writeln!(
            out,
            "Removing notes merge worktree at {}/*",
            shown(repo, &dir)
        );
    }
    let entries = std::fs::read_dir(&dir)
        .map_err(|_| GitError::Other("failed to remove 'git notes merge' worktree".into()))?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            std::fs::remove_dir_all(p)?;
        } else {
            std::fs::remove_file(p)?;
        }
    }
    Ok(out)
}

/// Copy the notes of rewritten commits to their rewrites, as git does after
/// `commit --amend` and `rebase` (`cmd`): the refs of notes.rewriteRef (or
/// $GIT_NOTES_REWRITE_REF), combined as notes.rewriteMode says, unless
/// notes.rewrite.<cmd> is false.
pub(crate) fn copy_for_rewrite(
    repo: &Repository,
    cmd: &str,
    pairs: &[(Oid, Oid)],
) -> Result<(), GitError> {
    let config = repo.config()?.snapshot()?;
    if !config
        .get_bool(&format!("notes.rewrite.{cmd}"))
        .unwrap_or(true)
        || pairs.is_empty()
    {
        return Ok(());
    }
    let patterns: Vec<String> = match std::env::var("GIT_NOTES_REWRITE_REF") {
        Ok(env) => env.split(':').map(str::to_owned).collect(),
        Err(_) => {
            let mut v = Vec::new();
            if let Ok(entries) = config.multivar("notes.rewriteRef", None) {
                let _ = entries
                    .for_each(|e| v.push(String::from_utf8_lossy(e.value_bytes()).into_owned()));
            }
            v
        }
    };
    let mode = std::env::var("GIT_NOTES_REWRITE_MODE")
        .ok()
        .or_else(|| config.get_string("notes.rewriteMode").ok())
        .unwrap_or_else(|| "concatenate".to_owned());
    let mut refs: Vec<String> = Vec::new();
    for p in patterns.iter().filter(|p| p.starts_with("refs/notes/")) {
        for r in repo.references_glob(p)?.flatten() {
            if let Ok(name) = r.name().map(str::to_owned)
                && !refs.contains(&name)
            {
                refs.push(name);
            }
        }
    }
    let message = match cmd {
        "amend" => "Notes added by 'git commit --amend'",
        _ => "Notes added by 'git rebase'",
    };
    for r in refs {
        commit_notes(repo, &r, message, |notes| {
            let before = notes.clone();
            for (from, to) in pairs {
                let Some(note) = before.get(from) else {
                    continue;
                };
                let new = blob(repo, Some(*note));
                let merged = match notes.get(to) {
                    None => Some(new),
                    Some(cur) => combine(&mode, &blob(repo, Some(*cur)), &new),
                };
                if let Some(m) = merged {
                    notes.insert(*to, repo.blob(&m)?);
                }
            }
            Ok(*notes != before)
        })?;
    }
    Ok(())
}

/// git's commit_notes: apply `edit` to the notes of `notes_ref` and, when it
/// reports a change, commit the result with `msg` and log `notes: <msg>`.
pub(crate) fn commit_notes(
    repo: &Repository,
    notes_ref: &str,
    msg: &str,
    edit: impl FnOnce(&mut BTreeMap<Oid, Oid>) -> Result<bool, GitError>,
) -> Result<(), GitError> {
    let parent = repo
        .refname_to_id(notes_ref)
        .ok()
        .map(|id| repo.find_commit(id))
        .transpose()?;
    let mut notes = match &parent {
        Some(p) => note_map(Some(&p.tree()?))?,
        None => BTreeMap::new(),
    };
    if !edit(&mut notes)? {
        return Ok(());
    }
    let tree = repo.find_tree(write_map(repo, &notes)?)?;
    let sig = crate::git_repo::ident_signature(repo, true)?;
    let author = crate::git_repo::ident_signature(repo, false)?;
    let id = repo.commit(
        None,
        &author,
        &sig,
        &format!("{msg}\n"),
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )?;
    repo.reference(notes_ref, id, true, &format!("notes: {msg}"))?;
    Ok(())
}
