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

fn rev(dir: &Path, name: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--verify", "-q", name])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[test]
fn failed_operation_leaves_the_oplog_and_redo_untouched() {
    let dir = init_repo("failed");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.create_branch("one").unwrap();
    backend.create_branch("two").unwrap();
    backend.undo().unwrap();
    let undo_before = rev(&dir, "refs/rgit/undo");
    let redo_before = rev(&dir, "refs/rgit/redo");
    assert!(redo_before.is_some(), "undo should leave a redo entry");
    let log_before = backend.oplog().unwrap();

    assert!(backend.commit("nothing staged").is_err());

    assert_eq!(rev(&dir, "refs/rgit/undo"), undo_before);
    assert_eq!(rev(&dir, "refs/rgit/redo"), redo_before);
    let log_after = backend.oplog().unwrap();
    assert_eq!(log_after.len(), log_before.len());
    assert_eq!(log_after[0].label, "create branch");
    assert_eq!(backend.redo().unwrap(), "create branch");
    assert!(
        backend.branch_exists("two"),
        "redo still restores the branch"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn successful_operation_still_records() {
    let dir = init_repo("success");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(backend.commit("nothing staged").is_err());
    assert!(backend.oplog().unwrap().is_empty());

    std::fs::write(dir.join("f.txt"), "v2\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    backend.commit("B").unwrap();
    let log = backend.oplog().unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].label, "commit");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_operation_that_changed_state_keeps_its_snapshot() {
    let dir = init_repo("conflict");
    std::fs::write(dir.join("f.txt"), "base\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "base"]);
    git(&dir, &["checkout", "-qb", "side"]);
    std::fs::write(dir.join("f.txt"), "side\n").unwrap();
    git(&dir, &["commit", "-qam", "side"]);
    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("f.txt"), "main\n").unwrap();
    git(&dir, &["commit", "-qam", "main"]);
    let main = head(&dir);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(backend.merge("side", false, false, &|_| {}).is_err());
    let log = backend.oplog().unwrap();
    assert_eq!(log.len(), 1, "the conflicted merge stays undoable");
    assert_eq!(log[0].label, "merge");

    assert_eq!(backend.undo().unwrap(), "merge");
    assert_eq!(head(&dir), main);
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "main\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
