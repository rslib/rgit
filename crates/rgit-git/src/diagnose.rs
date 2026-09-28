//! `git bugreport` and `git diagnose`: a report template with system
//! details, and a zip of repository statistics (and, in `all` mode, the
//! repository's own files).

use std::path::{Path, PathBuf};

use git2::Repository;

use crate::GitError;

/// `now` formatted with strftime's `fmt`, in local time.
pub fn strftime_now(fmt: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as libc::time_t);
    let Ok(cfmt) = std::ffi::CString::new(fmt) else {
        return String::new();
    };
    let mut buf = vec![0u8; 256];
    // SAFETY: localtime_r and strftime write into the buffers we own.
    let n = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        libc::strftime(buf.as_mut_ptr().cast(), buf.len(), cfmt.as_ptr(), &tm)
    };
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn uname() -> String {
    // SAFETY: uname fills the struct we own.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            return String::new();
        }
        let f = |p: &[libc::c_char]| {
            std::ffi::CStr::from_ptr(p.as_ptr())
                .to_string_lossy()
                .into_owned()
        };
        format!(
            "{} {} {} {}",
            f(&u.sysname),
            f(&u.release),
            f(&u.version),
            f(&u.machine)
        )
    }
}

fn version_info() -> String {
    format!(
        "rgit version {}\ncpu: {}\nsizeof-long: {}\nsizeof-size_t: {}\nshell-path: /bin/sh\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::mem::size_of::<libc::c_long>(),
        std::mem::size_of::<usize>(),
    )
}

/// The hooks that would run: executable files in the hooks folder.
fn hooks(repo: &Repository) -> Vec<String> {
    let dir = crate::git_repo::hooks_dir(repo);
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            use std::os::unix::fs::PermissionsExt;
            e.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".sample"))
        .collect();
    out.sort();
    out
}

/// Whether the repository is a partial clone (a promisor remote).
pub fn is_partial_clone(git_dir: &Path) -> bool {
    let Ok(cfg) = Repository::open(git_dir).and_then(|r| r.config()) else {
        return false;
    };
    let promisor = cfg
        .entries(Some(r"^remote\..*\.promisor$"))
        .is_ok_and(|mut e| {
            let mut any = false;
            while let Some(Ok(entry)) = e.next() {
                any |= entry.value().is_ok_and(|v| v == "true");
            }
            any
        });
    promisor || cfg.get_string("extensions.partialclone").is_ok()
}

/// `git bugreport`: write the report into `dir` named with the strftime
/// `suffix` (none with `None`); the file's path.
pub fn bugreport(
    git_dir: Option<&Path>,
    dir: &Path,
    suffix: Option<&str>,
) -> Result<PathBuf, GitError> {
    let mut name = String::from("git-bugreport");
    if let Some(s) = suffix {
        name.push('-');
        name.push_str(&strftime_now(s));
    }
    name.push_str(".txt");
    std::fs::create_dir_all(dir).map_err(|_| {
        GitError::Other(format!(
            "could not create leading directories for '{}'",
            dir.join(&name).display()
        ))
    })?;
    let path = dir.join(&name);
    let mut text = String::from(
        "Thank you for filling out a Git bug report!\n\
         Please answer the following questions to help us understand your issue.\n\n\
         What did you do before the bug happened? (Steps to reproduce your issue)\n\n\
         What did you expect to happen? (Expected behavior)\n\n\
         What happened instead? (Actual behavior)\n\n\
         What's different between what you expected and what actually happened?\n\n\
         Anything else you want to add:\n\n\
         Please review the rest of the bug report below.\n\
         You can delete any lines you don't wish to share.\n",
    );
    text.push_str("\n\n[System Info]\n");
    text.push_str("git version:\n");
    text.push_str(&version_info());
    text.push_str(&format!("uname: {}\n", uname()));
    text.push_str("compiler info: rustc\n");
    text.push_str(&format!("libc info: {}\n", std::env::consts::OS));
    text.push_str(&format!(
        "$SHELL (typically, interactive shell): {}\n",
        std::env::var("SHELL").unwrap_or_else(|_| "<unset>".into())
    ));
    text.push_str("\n\n[Enabled Hooks]\n");
    match git_dir.map(Repository::open).transpose()? {
        Some(repo) => {
            for h in hooks(&repo) {
                text.push_str(&format!("{h}\n"));
            }
        }
        None => text.push_str("not run from a git repository - no hooks to show\n"),
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| GitError::Other(format!("could not open '{}': {e}", path.display())))?;
    std::io::Write::write_all(&mut f, text.as_bytes())?;
    Ok(path)
}

/// `git diagnose`: write `zip_path` with diagnostics.log, packs-local.txt
/// and objects-local.txt (and with `all`, the repository's config, hooks,
/// info, logs and objects/info files); returns the log.
pub fn diagnose(git_dir: &Path, zip_path: &Path, all: bool) -> Result<String, GitError> {
    let repo = Repository::open(git_dir)?;
    let root = repo.workdir().unwrap_or(repo.path());
    let mut log = String::from("Collecting diagnostic info\n\n");
    log.push_str(&version_info());
    log.push_str(&format!(
        "Repository root: {}\n",
        root.display().to_string().trim_end_matches('/')
    ));
    let objects = repo.commondir().join("objects");
    let mut packs = format!("Contents of {}:\n", objects.join("pack").display());
    let mut names: Vec<(String, u64)> = std::fs::read_dir(objects.join("pack"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            Some((
                e.file_name().to_string_lossy().into_owned(),
                e.metadata().ok()?.len(),
            ))
        })
        .collect();
    names.sort();
    for (n, size) in names {
        packs.push_str(&format!("{n:<70} {size:>16}\n"));
    }
    let mut loose = String::new();
    let mut total = 0;
    for i in 0..256 {
        let d = objects.join(format!("{i:02x}"));
        let n = std::fs::read_dir(&d).map_or(0, |r| r.count());
        if n > 0 {
            loose.push_str(&format!("{i:02x}: {n}\n"));
            total += n;
        }
    }
    loose.push_str(&format!("Total: {total} loose objects"));
    let mut extra = vec![
        (
            "diagnostics.log".to_owned(),
            0o100644,
            log.clone().into_bytes(),
        ),
        ("packs-local.txt".to_owned(), 0o100644, packs.into_bytes()),
        ("objects-local.txt".to_owned(), 0o100644, loose.into_bytes()),
    ];
    if all {
        let gd = repo.path();
        for (dir, recursive) in [
            ("", false),
            ("hooks", false),
            ("info", false),
            ("logs", true),
            ("objects/info", false),
        ] {
            let base = gd.join(dir);
            let walk =
                walkdir::WalkDir::new(&base).max_depth(if recursive { usize::MAX } else { 1 });
            for e in walk
                .into_iter()
                .flatten()
                .filter(|e| e.file_type().is_file())
            {
                let rel = e.path().strip_prefix(gd).unwrap_or(e.path());
                extra.push((
                    format!(".git/{}", rel.display()),
                    0o100644,
                    std::fs::read(e.path()).unwrap_or_default(),
                ));
            }
        }
    }
    let empty = repo.treebuilder(None)?.write()?;
    let o = crate::ArchiveOpts {
        rev: empty.to_string(),
        format: "zip".into(),
        extra,
        ..crate::ArchiveOpts::default()
    };
    let bytes = crate::archive::archive(&repo, &o)?;
    if let Some(dir) = zip_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(zip_path, bytes)?;
    Ok(log)
}
