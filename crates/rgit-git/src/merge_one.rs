//! `git merge-one-file` (the shell script, done natively) and
//! `git merge-index`, which runs a merge program on each unmerged path.

use std::path::Path;

use git2::build::CheckoutBuilder;
use git2::{Index, IndexEntry, IndexTime, Oid, Repository};

use crate::GitError;
use crate::index_ops::Report;

fn entry(path: &str, mode: &str, id: &str) -> Result<IndexEntry, GitError> {
    Ok(IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: u32::from_str_radix(mode, 8)
            .map_err(|_| GitError::Other(format!("invalid mode {mode}")))?,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: Oid::from_str(id)?,
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    })
}

fn checkout(repo: &Repository, index: &mut Index, path: &str) -> Result<(), GitError> {
    repo.checkout_index(Some(index), Some(CheckoutBuilder::new().force().path(path)))?;
    Ok(())
}

/// `git merge-one-file <orig> <ours> <theirs> <path> <orig mode> <our mode>
/// <their mode>`: resolve one unmerged path in the index and work tree.
pub fn merge_one_file(git_dir: &Path, a: &[String]) -> Result<Report, GitError> {
    let mut r = Report::default();
    if a.len() != 7 {
        let usage = "usage: git merge-one-file <orig blob> <our blob> <their blob> <path> \
                     <orig mode> <our mode> <their mode>";
        // git-sh-setup prints the usage line, then the long usage.
        r.out = format!(
            "{usage}\n\n{usage}\n\nBlob ids and modes should be empty for missing files.\n"
        );
        r.failed = true;
        return Ok(r);
    }
    let repo = Repository::open(git_dir)?;
    let work = repo
        .workdir()
        .ok_or_else(|| GitError::Other("this operation must be run in a work tree".into()))?
        .to_path_buf();
    let mut index = repo.index()?;
    let (o, ours, theirs, path) = (&a[0], &a[1], &a[2], a[3].as_str());
    let (om, m2, m3) = (&a[4], &a[5], &a[6]);
    let file = work.join(path);
    let fail = |r: &mut Report, msg: String| {
        r.err.push_str(&msg);
        r.failed = true;
    };
    fn dot(s: &str) -> &str {
        if s.is_empty() { "." } else { s }
    }
    let key = format!("{}{}{}", dot(o), dot(ours), dot(theirs));
    let deleted = !o.is_empty()
        && (key == format!("{o}..") || key == format!("{o}.{o}") || key == format!("{o}{o}."));
    if deleted {
        if (m2.is_empty() && om != m3) || (m3.is_empty() && om != m2) {
            fail(
                &mut r,
                format!(
                    "ERROR: File {path} deleted on one branch but had its\n\
                     ERROR: permissions changed on the other.\n"
                ),
            );
            return Ok(r);
        }
        if !ours.is_empty() {
            r.out.push_str(&format!("Removing {path}\n"));
            if file.is_file() {
                let _ = std::fs::remove_file(&file);
                let mut dir = file.parent();
                while let Some(d) = dir.filter(|d| *d != work) {
                    if std::fs::remove_dir(d).is_err() {
                        break;
                    }
                    dir = d.parent();
                }
            }
        }
        index.remove_path(Path::new(path))?;
        index.write()?;
        return Ok(r);
    }
    if o.is_empty() && !ours.is_empty() && theirs.is_empty() {
        index.add(&entry(path, m2, ours)?)?;
        index.write()?;
        return Ok(r);
    }
    if o.is_empty() && ours.is_empty() && !theirs.is_empty() {
        r.out.push_str(&format!("Adding {path}\n"));
        if file.is_file() {
            fail(
                &mut r,
                format!("ERROR: untracked {path} is overwritten by the merge.\n"),
            );
            return Ok(r);
        }
        index.add(&entry(path, m3, theirs)?)?;
        index.write()?;
        checkout(&repo, &mut index, path)?;
        return Ok(r);
    }
    if o.is_empty() && !ours.is_empty() && ours == theirs {
        if m2 != m3 {
            fail(
                &mut r,
                format!(
                    "ERROR: File {path} added identically in both branches,\n\
                     ERROR: but permissions conflict {m2}->{m3}.\n"
                ),
            );
            return Ok(r);
        }
        r.out.push_str(&format!("Adding {path}\n"));
        index.add(&entry(path, m2, ours)?)?;
        index.write()?;
        checkout(&repo, &mut index, path)?;
        return Ok(r);
    }
    if ours.is_empty() || theirs.is_empty() {
        fail(
            &mut r,
            format!("ERROR: {path}: Not handling case {o} -> {ours} -> {theirs}\n"),
        );
        return Ok(r);
    }
    for m in [m2, m3] {
        match m.as_str() {
            "120000" => {
                fail(
                    &mut r,
                    format!("ERROR: {path}: Not merging symbolic link changes.\n"),
                );
                return Ok(r);
            }
            "160000" => {
                fail(
                    &mut r,
                    format!("ERROR: {path}: Not merging conflicting submodule changes.\n"),
                );
                return Ok(r);
            }
            _ => {}
        }
    }
    let blob = |id: &str| -> Result<Vec<u8>, GitError> {
        Ok(repo.find_blob(Oid::from_str(id)?)?.content().to_vec())
    };
    let base = if o.is_empty() {
        r.out
            .push_str(&format!("Added {path} in both, but differently.\n"));
        Vec::new()
    } else {
        r.out.push_str(&format!("Auto-merging {path}\n"));
        blob(o)?
    };
    let (b2, b3) = (blob(ours)?, blob(theirs)?);
    // The script merges temporary files, so their names label the markers.
    let labels = [temp_name(), temp_name(), temp_name()];
    let opts = crate::MergeFileOpts {
        labels: [labels[0].clone(), labels[1].clone(), labels[2].clone()],
        alnum: true,
        ..Default::default()
    };
    // Its base is `git hash-object /dev/null`, which fails unless the
    // repository holds the empty blob; merge-file then keeps our side.
    let empty = Oid::hash_object(git2::ObjectType::Blob, b"")?;
    let (merged, conflicts) = if o.is_empty() && !repo.odb()?.exists(empty) {
        (b2.clone(), 1)
    } else {
        crate::merge_file(&b2, &base, &b3, &opts)?
    };
    let mut msg = String::new();
    if conflicts > 0 || o.is_empty() {
        msg.push_str("content conflict");
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&file, &merged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let exec = if m2 == "100755" { 0o755 } else { 0o644 };
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(exec));
    }
    if m2 != m3 {
        if !msg.is_empty() {
            msg.push_str(", ");
        }
        msg.push_str(&format!("permissions conflict: {om}->{m2},{m3}"));
    }
    if !msg.is_empty() {
        fail(&mut r, format!("ERROR: {msg} in {path}\n"));
        return Ok(r);
    }
    index.add_path(Path::new(path))?;
    index.write()?;
    Ok(r)
}

/// Whether the index has `path` at stage 0.
pub fn index_has_path(git_dir: &Path, path: &str) -> Result<bool, GitError> {
    let repo = Repository::open(git_dir)?;
    Ok(repo.index()?.get_path(Path::new(path), 0).is_some())
}

/// The unmerged paths of the index with the seven merge-one-file
/// arguments for each: ids of stages 1-3, the path, then their modes.
pub fn unmerged_stages(git_dir: &Path) -> Result<Vec<[String; 7]>, GitError> {
    let repo = Repository::open(git_dir)?;
    let index = repo.index()?;
    let mut out: Vec<[String; 7]> = Vec::new();
    for e in index.iter() {
        let stage = ((e.flags >> 12) & 3) as usize;
        if stage == 0 {
            continue;
        }
        let path = String::from_utf8_lossy(&e.path).into_owned();
        if out.last().is_none_or(|l| l[3] != path) {
            out.push(Default::default());
            out.last_mut().expect("just pushed")[3] = path;
        }
        let last = out.last_mut().expect("an entry");
        last[stage - 1] = e.id.to_string();
        last[stage + 3] = format!("{:o}", e.mode);
    }
    Ok(out)
}

/// A `.merge_file_XXXXXX` name, as git's mkstemp picks one.
fn temp_name() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
        ^ (u64::from(std::process::id()) << 32);
    let tail: String = (0..6)
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            CHARS[(seed >> 33) as usize % CHARS.len()] as char
        })
        .collect();
    format!(".merge_file_{tail}")
}
