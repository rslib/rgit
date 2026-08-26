//! A long-lived backend stays correct when plain `git` runs alongside it:
//! libgit2 caches one index per repo, so entry points reload it from disk.

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-idxsync-{}-{name}", std::process::id()))
}

fn init_repo(name: &str) -> std::path::PathBuf {
    let dir = scratch(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "test"],
    ] {
        assert!(Command::new("git").arg("-C").arg(&dir).args(&args).status().unwrap().success());
    }
    dir
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
}

fn tree_files(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C").arg(dir)
        .args(["ls-tree", "--name-only", "-r", "HEAD"])
        .output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// A file staged by plain `git add` is included when the backend commits, even
// though the backend's cached index was primed by an earlier commit.
#[test]
fn backend_commit_includes_a_git_staged_file() {
    let dir = init_repo("commit-staged");
    std::fs::write(dir.join("base.txt"), "b\n").unwrap();
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stage_all().unwrap();
    backend.commit("base").unwrap(); // primes libgit2's cached index

    std::fs::write(dir.join("foo.txt"), "1\n").unwrap();
    git(&dir, &["add", "foo.txt"]); // stage behind the backend's back
    backend.commit("add foo").unwrap();

    let files = tree_files(&dir);
    assert!(files.contains("foo.txt"), "commit must include the git-staged file, got: {files:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

// The same staging is reflected in status() after the external add.
#[test]
fn external_git_add_shows_as_staged() {
    let dir = init_repo("status-staged");
    std::fs::write(dir.join("base.txt"), "b\n").unwrap();
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stage_all().unwrap();
    backend.commit("base").unwrap();

    std::fs::write(dir.join("foo.txt"), "1\n").unwrap();
    assert!(backend.status().unwrap().staged_diff("foo.txt").is_none());
    git(&dir, &["add", "foo.txt"]);

    let st = backend.status().unwrap();
    assert!(st.staged_diff("foo.txt").is_some(), "external git add must show as staged");

    let _ = std::fs::remove_dir_all(&dir);
}
