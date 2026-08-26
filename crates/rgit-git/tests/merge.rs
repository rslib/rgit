//! Merge: fast-forward, no-ff, divergent merge, and conflict handling.

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend, RepoState};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-merge-{}-{name}", std::process::id()))
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

/// Number of parents of `rev`, via `git cat-file`.
fn parent_count(dir: &Path, rev: &str) -> usize {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["cat-file", "-p", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .take_while(|l| !l.is_empty())
        .filter(|l| l.starts_with("parent "))
        .count()
}

#[test]
fn fast_forward_merge_moves_the_branch_tip() {
    let dir = init_repo("ff");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);

    git(&dir, &["checkout", "-qb", "feature"]);
    std::fs::write(dir.join("g.txt"), "v1\n").unwrap();
    git(&dir, &["add", "g.txt"]);
    git(&dir, &["commit", "-qm", "C1"]);
    let feature_tip = head(&dir);
    git(&dir, &["checkout", "-q", "main"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.merge("feature", false, &|_| {}).unwrap();

    assert_eq!(head(&dir), feature_tip, "main should fast-forward to feature");
    assert_eq!(parent_count(&dir, "HEAD"), 1, "ff merge has no merge commit");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_ff_merge_creates_a_merge_commit() {
    let dir = init_repo("noff");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);

    git(&dir, &["checkout", "-qb", "feature"]);
    std::fs::write(dir.join("g.txt"), "v1\n").unwrap();
    git(&dir, &["add", "g.txt"]);
    git(&dir, &["commit", "-qm", "C1"]);
    let feature_tip = head(&dir);
    git(&dir, &["checkout", "-q", "main"]);
    let main_tip = head(&dir);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.merge("feature", true, &|_| {}).unwrap();

    let new_head = head(&dir);
    assert_ne!(new_head, feature_tip, "no-ff must not just fast-forward");
    assert_eq!(parent_count(&dir, &new_head), 2, "no-ff merge commit has 2 parents");
    assert_ne!(new_head, main_tip, "HEAD moved past the pre-merge tip");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn divergent_merge_combines_both_sides() {
    let dir = init_repo("divergent");
    std::fs::write(dir.join("base.txt"), "v1\n").unwrap();
    git(&dir, &["add", "base.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);

    git(&dir, &["checkout", "-qb", "feature"]);
    std::fs::write(dir.join("feature.txt"), "v1\n").unwrap();
    git(&dir, &["add", "feature.txt"]);
    git(&dir, &["commit", "-qm", "feature side"]);

    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("main.txt"), "v1\n").unwrap();
    git(&dir, &["add", "main.txt"]);
    git(&dir, &["commit", "-qm", "main side"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.merge("feature", false, &|_| {}).unwrap();

    let new_head = head(&dir);
    assert_eq!(parent_count(&dir, &new_head), 2);
    assert!(dir.join("base.txt").exists());
    assert!(dir.join("main.txt").exists());
    assert!(dir.join("feature.txt").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conflicting_merge_leaves_the_repo_mid_merge_and_resolve_picks_ours() {
    let dir = init_repo("conflict");
    std::fs::write(dir.join("f.txt"), "base\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);

    git(&dir, &["checkout", "-qb", "feature"]);
    std::fs::write(dir.join("f.txt"), "feature side\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "feature edit"]);

    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("f.txt"), "main side\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "main edit"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let result = backend.merge("feature", false, &|_| {});
    assert!(result.is_err(), "conflicting content must fail the merge");

    let status = backend.status().unwrap();
    assert_eq!(status.state, RepoState::Merge, "repo should be mid-merge");

    backend.resolve_conflict("f.txt", true).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "main side\n",
        "ours should win"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dirty_worktree_refuses_fast_forward_without_clobbering() {
    let dir = init_repo("dirty");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);

    git(&dir, &["checkout", "-qb", "feature"]);
    // Feature moves f.txt forward, so the ff checkout would touch f.txt.
    std::fs::write(dir.join("f.txt"), "v2\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "C1"]);
    let main_tip_before = {
        git(&dir, &["checkout", "-q", "main"]);
        head(&dir)
    };

    // Dirty, uncommitted change to the same tracked file on main.
    std::fs::write(dir.join("f.txt"), "uncommitted\n").unwrap();

    let backend = Git2Backend::discover(&dir).unwrap();
    let result = backend.merge("feature", false, &|_| {});
    assert!(result.is_err(), "dirty ff must be refused");

    assert_eq!(head(&dir), main_tip_before, "HEAD must not move");
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "uncommitted\n",
        "uncommitted change must survive"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
