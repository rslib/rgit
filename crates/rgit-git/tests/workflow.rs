//! Branching workflow presets: init picks a policy, start/finish/release move
//! branches per that policy.

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend, workflow};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-workflow-{}-{name}", std::process::id()))
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

fn commit_file(dir: &Path, file: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", msg]);
}

fn current_branch(backend: &Git2Backend) -> String {
    backend.status().unwrap().head.branch.unwrap()
}

fn parent_count(dir: &Path, rev: &str) -> usize {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", &format!("{rev}^@")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .count()
}

fn tag_exists(dir: &Path, name: &str) -> bool {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["tag", "-l", name])
        .output()
        .unwrap();
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

#[test]
fn init_gitflow_creates_develop_and_sets_config() {
    let dir = init_repo("init");
    commit_file(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    workflow::init(&backend, "gitflow").unwrap();

    assert_eq!(
        backend.config_get("rgit.workflow").unwrap(),
        Some("gitflow".to_owned())
    );
    assert!(backend.branch_exists("develop"));

    let status = workflow::status(&backend).unwrap();
    assert!(status.contains("workflow: gitflow"));
    assert!(status.contains("integration: develop"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_unknown_preset_returns_err() {
    let dir = init_repo("badpreset");
    commit_file(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(workflow::init(&backend, "nonsense").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn start_creates_feature_branch_from_base() {
    let dir = init_repo("start");
    commit_file(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    workflow::init(&backend, "gitflow").unwrap();

    workflow::start(&backend, "widget").unwrap();

    assert!(backend.branch_exists("feature/widget"));
    assert_eq!(current_branch(&backend), "feature/widget");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn finish_merges_feature_into_develop_with_merge_commit() {
    let dir = init_repo("finish");
    commit_file(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    workflow::init(&backend, "gitflow").unwrap();
    workflow::start(&backend, "widget").unwrap();
    commit_file(&dir, "widget.txt", "wip\n", "widget work");

    let note = workflow::finish(&backend).unwrap();
    assert!(note.contains("merged feature/widget into develop"));

    assert_eq!(current_branch(&backend), "develop");
    // finish always merges with no_ff, so develop's tip is a merge commit.
    assert_eq!(parent_count(&dir, "develop"), 2);
    // gitflow deletes the feature branch on finish.
    assert!(!backend.branch_exists("feature/widget"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn release_start_then_finish_merges_tags_and_deletes() {
    let dir = init_repo("release");
    commit_file(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    workflow::init(&backend, "gitflow").unwrap();

    workflow::release(&backend, "v1.0", false).unwrap();
    assert!(backend.branch_exists("release/v1.0"));
    assert_eq!(current_branch(&backend), "release/v1.0");
    commit_file(&dir, "changelog.txt", "notes\n", "prep release");

    workflow::release(&backend, "v1.0", true).unwrap();

    // gitflow merges the release branch into main then develop.
    assert_eq!(parent_count(&dir, "main"), 2);
    assert_eq!(parent_count(&dir, "develop"), 2);
    assert!(tag_exists(&dir, "v1.0"));
    assert!(!backend.branch_exists("release/v1.0"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn status_without_workflow_set_is_err() {
    let dir = init_repo("nostatus");
    commit_file(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    // No `workflow::init` call: no preset stored in git config.
    assert!(workflow::status(&backend).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}
