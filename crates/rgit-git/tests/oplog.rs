//! The operation log: undo/redo restore HEAD and the working tree (including
//! uncommitted changes), and the log stays out of `refs/heads`.

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-oplog-{}-{name}", std::process::id()))
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
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    dir
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn head(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn undo_recovers_a_commit_and_its_working_tree() {
    let dir = init_repo("undo");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);
    let a = head(&dir);

    // Change and commit through the backend (which snapshots first).
    std::fs::write(dir.join("f.txt"), "v2\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.commit("B").unwrap();
    assert_ne!(head(&dir), a, "commit B should move HEAD");

    // Undo: HEAD back to A, and the v2 change is recovered (not lost).
    let label = backend.undo().unwrap();
    assert_eq!(label, "commit");
    assert_eq!(head(&dir), a, "undo should restore HEAD to A");
    assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "v2\n");

    // Redo: HEAD returns to B.
    assert_eq!(backend.redo().unwrap(), "commit");
    assert_ne!(head(&dir), a, "redo should restore commit B");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oplog_lists_snapshots_and_undo_errors_when_empty() {
    let dir = init_repo("list");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(backend.oplog().unwrap().is_empty());
    assert!(backend.undo().is_err(), "nothing to undo yet");

    backend.create_branch("feature").unwrap();
    let log = backend.oplog().unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].label, "create branch");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undo_restores_a_deleted_branch() {
    let dir = init_repo("delbranch");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);
    git(&dir, &["branch", "feature"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.delete_branch("feature", true).unwrap();
    assert!(!backend.branch_exists("feature"), "feature deleted");

    backend.undo().unwrap();
    assert!(
        backend.branch_exists("feature"),
        "undo should restore the deleted branch"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
