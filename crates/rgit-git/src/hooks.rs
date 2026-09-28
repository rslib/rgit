//! Hooks git runs around every command rather than inside one:
//! `reference-transaction` for ref updates, `post-index-change` for index
//! writes, and `git hook run`.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use git2::Repository;

use crate::GitError;
use crate::git_repo::{hooks_dir, run_hook};

const ZERO: &str = "0000000000000000000000000000000000000000";

static STREAM: AtomicBool = AtomicBool::new(false);

/// Let hooks write straight to the terminal (their stdout on stderr), as git
/// runs them, instead of capturing their output for an error report.
pub fn stream_hooks() {
    STREAM.store(true, Ordering::Relaxed);
}

pub(crate) fn streaming() -> bool {
    STREAM.load(Ordering::Relaxed)
}

/// The executable `name` hook in `dir`, if there is one.
pub(crate) fn find_hook(dir: &Path, name: &str) -> Option<PathBuf> {
    let hook = dir.join(name);
    #[cfg(unix)]
    let runnable = {
        use std::os::unix::fs::PermissionsExt;
        hook.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    #[cfg(not(unix))]
    let runnable = hook.is_file();
    runnable.then_some(hook)
}

fn workdir(repo: &Repository) -> &Path {
    repo.workdir().unwrap_or(repo.path())
}

/// Every ref's value: an object id, or `ref:<target>` for a symbolic ref.
type Refs = BTreeMap<String, String>;

fn read_refs(repo: &Repository) -> Refs {
    let mut refs = Refs::new();
    let value = |r: &git2::Reference| match r.symbolic_target().ok().flatten() {
        Some(t) => Some(format!("ref:{t}")),
        None => r.target().map(|o| o.to_string()),
    };
    if let Ok(iter) = repo.references() {
        for r in iter.flatten() {
            // rgit's own undo log is no ref git would know of.
            if let (Ok(name), Some(v)) = (r.name(), value(&r))
                && !name.starts_with("refs/rgit/")
            {
                refs.insert(name.to_owned(), v);
            }
        }
    }
    for name in ["HEAD", "ORIG_HEAD"] {
        if let Some(v) = repo.find_reference(name).ok().as_ref().and_then(value) {
            refs.insert(name.to_owned(), v);
        }
    }
    refs
}

fn index_path(git_dir: &Path) -> PathBuf {
    std::env::var_os("GIT_INDEX_FILE")
        .filter(|v| !v.is_empty())
        .map_or_else(|| git_dir.join("index"), PathBuf::from)
}

/// The index's trailing checksum, which every write changes.
fn index_stamp(path: &Path) -> Option<[u8; 20]> {
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::End(-20)).ok()?;
    let mut sum = [0; 20];
    f.read_exact(&mut sum).ok()?;
    Some(sum)
}

/// What a command's ref and index changes are measured against, taken before
/// it runs, only when the hooks that want them exist.
pub struct HookWatch {
    git_dir: PathBuf,
    refs: Option<Refs>,
    index: Option<Option<[u8; 20]>>,
}

/// Start watching the repository at `git_dir` for the ref updates and index
/// writes a command makes, for [`HookWatch::finish`].
pub fn watch(git_dir: &Path) -> Option<HookWatch> {
    let repo = Repository::open(git_dir).ok()?;
    let dir = hooks_dir(&repo);
    let refs = find_hook(&dir, "reference-transaction").map(|_| read_refs(&repo));
    let index = find_hook(&dir, "post-index-change").map(|_| index_stamp(&index_path(git_dir)));
    (refs.is_some() || index.is_some()).then(|| HookWatch {
        git_dir: git_dir.to_owned(),
        refs,
        index,
    })
}

impl HookWatch {
    /// Run the hooks for what changed since [`watch`]: `reference-transaction`
    /// through `prepared` and `committed` (a refusing `prepared` puts the refs
    /// back and runs `aborted`), then `post-index-change` with its
    /// `<worktree updated> <skip-worktree changed>` flags.
    // ponytail: the hook sees one transaction per command, after the refs
    // moved, and an abort restores the refs but not their reflogs; a
    // refdb-backend wrapper would give git's per-update, before-write view.
    pub fn finish(self, flags: impl FnOnce() -> (bool, bool)) -> Result<(), GitError> {
        let repo = Repository::open(&self.git_dir)?;
        let dir = hooks_dir(&repo);
        let cwd = workdir(&repo);
        if let Some(old) = self.refs {
            let new = read_refs(&repo);
            let mut lines = String::new();
            let mut changed = Vec::new();
            for name in old
                .keys()
                .chain(new.keys().filter(|k| !old.contains_key(*k)))
            {
                let (a, b) = (old.get(name), new.get(name));
                if a == b {
                    continue;
                }
                let before = a
                    .filter(|v| !v.starts_with("ref:"))
                    .map_or(ZERO, String::as_str);
                lines.push_str(&format!(
                    "{before} {} {name}\n",
                    b.map_or(ZERO, String::as_str)
                ));
                changed.push((name.clone(), a.cloned()));
            }
            if !lines.is_empty() {
                let run = |state: &str| {
                    run_hook(
                        &dir,
                        cwd,
                        "reference-transaction",
                        &[state],
                        Some(lines.as_bytes()),
                    )
                    .map(|o| o.as_ref().is_none_or(echo))
                };
                if !run("prepared")? {
                    for (name, value) in changed {
                        restore(&repo, &name, value.as_deref())?;
                    }
                    run("aborted")?;
                    return Err(GitError::Other("ref updates aborted by hook".to_owned()));
                }
                run("committed")?;
            }
        }
        if let Some(before) = self.index
            && index_stamp(&index_path(&self.git_dir)) != before
        {
            let (worktree, skip) = flags();
            let flag = |b: bool| if b { "1" } else { "0" };
            let out = run_hook(
                &dir,
                cwd,
                "post-index-change",
                &[flag(worktree), flag(skip)],
                None,
            )?;
            out.as_ref().map(echo);
        }
        Ok(())
    }
}

/// Put ref `name` back to `value` (absent, an id or `ref:<target>`).
fn restore(repo: &Repository, name: &str, value: Option<&str>) -> Result<(), GitError> {
    const MSG: &str = "reference-transaction: aborted";
    match value {
        None => {
            if let Ok(mut r) = repo.find_reference(name) {
                r.delete()?;
            }
        }
        Some(v) => match v.strip_prefix("ref:") {
            Some(target) => {
                repo.reference_symbolic(name, target, true, MSG)?;
            }
            None => {
                repo.reference(name, git2::Oid::from_str(v)?, true, MSG)?;
            }
        },
    }
    Ok(())
}

/// `git hook run`: run hook `name` with `args`, `stdin` from a file, and
/// return its exit status; a missing hook is an error unless
/// `ignore_missing`.
pub fn hook_run(
    git_dir: &Path,
    name: &str,
    args: &[String],
    stdin: Option<&Path>,
    ignore_missing: bool,
) -> Result<i32, GitError> {
    let repo = Repository::open(git_dir)?;
    let dir = hooks_dir(&repo);
    if find_hook(&dir, name).is_none() {
        return if ignore_missing {
            Ok(0)
        } else {
            Err(GitError::Other(format!("cannot find a hook named {name}")))
        };
    }
    let input = stdin.map(std::fs::read).transpose()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_hook(&dir, workdir(&repo), name, &args, input.as_deref())?;
    Ok(out.map_or(0, |o| {
        echo(&o);
        o.status.code().unwrap_or(1)
    }))
}

/// Show a hook's captured output on stderr, where git shows it; whether it
/// passed.
fn echo(out: &std::process::Output) -> bool {
    use std::io::Write;
    let _ = std::io::stderr().write_all(&out.stdout);
    let _ = std::io::stderr().write_all(&out.stderr);
    out.status.success()
}

/// Run hook `name` with no arguments, its output on stderr; whether it
/// passed (or is absent).
pub(crate) fn hook_ok(repo: &Repository, name: &str) -> bool {
    run_hook(&hooks_dir(repo), workdir(repo), name, &[], None)
        .is_ok_and(|o| o.as_ref().is_none_or(echo))
}
