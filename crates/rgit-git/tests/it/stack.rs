//! Branch stacking: `stack_new` records a parent, `restack` rebases children
//! onto their parent's updated tip, and `smartlog` lists draft work above trunk.

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-stack-{}-{name}", std::process::id()))
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

fn commit(dir: &Path, file: &str, contents: &str, message: &str) {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", message]);
}

#[test]
fn stack_new_records_the_current_branch_as_parent() {
    let dir = init_repo("new");
    commit(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    let msg = backend.stack_new("feature").unwrap();
    assert_eq!(msg, "created feature stacked on main");

    let parents: std::collections::HashMap<_, _> =
        backend.stack_parents().unwrap().into_iter().collect();
    assert_eq!(
        parents.get("feature").cloned().flatten(),
        Some("main".to_owned())
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stack_parents_is_none_for_the_trunk() {
    let dir = init_repo("trunk");
    commit(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stack_new("feature").unwrap();

    let parents: std::collections::HashMap<_, _> =
        backend.stack_parents().unwrap().into_iter().collect();
    assert_eq!(parents.get("main").cloned().flatten(), None);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restack_rebases_the_child_onto_the_parents_new_tip() {
    let dir = init_repo("restack");
    commit(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stack_new("feature").unwrap();
    commit(&dir, "g.txt", "child\n", "child commit");
    let child_tip_before = backend.branch_tip("feature").unwrap().unwrap();

    // Move main forward; feature's recorded base is now stale.
    backend.checkout_branch("main").unwrap();
    commit(&dir, "h.txt", "parent update\n", "B");
    let main_tip = backend.branch_tip("main").unwrap().unwrap();

    let restacked = backend.restack().unwrap();
    assert_eq!(restacked.restacked, vec!["feature -> main".to_owned()]);
    assert!(restacked.conflicted.is_empty());

    let child_tip_after = backend.branch_tip("feature").unwrap().unwrap();
    assert_ne!(
        child_tip_after, child_tip_before,
        "feature should get a new tip"
    );

    // feature's single commit ("child commit") should now sit on top of main.
    backend.checkout_branch("feature").unwrap();
    let between = backend.commits_between(&main_tip).unwrap();
    assert_eq!(between.len(), 1);
    assert_eq!(between[0].1, "child commit");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restack_is_a_noop_when_the_parent_has_not_moved() {
    let dir = init_repo("noop");
    commit(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stack_new("feature").unwrap();
    commit(&dir, "g.txt", "child\n", "child commit");

    let restacked = backend.restack().unwrap();
    assert!(
        restacked.is_empty(),
        "nothing to restack when the parent tip is unchanged"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn smartlog_lists_draft_commits_above_the_trunk() {
    let dir = init_repo("smartlog");
    commit(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.create_branch("feature").unwrap();
    commit(&dir, "g.txt", "draft1\n", "draft one");
    commit(&dir, "h.txt", "draft2\n", "draft two");

    let entries = backend.smartlog().unwrap();
    let summaries: Vec<&str> = entries.iter().map(|e| e.summary.as_str()).collect();
    assert!(summaries.contains(&"draft one"));
    assert!(summaries.contains(&"draft two"));
    assert!(summaries.contains(&"A"), "trunk tip should also be listed");

    let trunk_entry = entries.iter().find(|e| e.is_trunk).unwrap();
    assert_eq!(trunk_entry.summary, "A");

    let _ = std::fs::remove_dir_all(&dir);
}

// A conflict on one branch does not abort the whole restack: the clean branch
// still moves, the conflicting one is reported and left at its old base.
#[test]
fn restack_is_non_blocking_across_a_conflict() {
    let dir = init_repo("nonblocking");
    commit(&dir, "shared.txt", "base\n", "C0");

    let backend = Git2Backend::discover(&dir).unwrap();

    // feat-a stacks on main and adds an unrelated file (will rebase cleanly).
    backend.stack_new("feat-a").unwrap();
    commit(&dir, "a.txt", "a\n", "A1");

    // feat-b stacks on main and edits the shared file (will conflict).
    backend.checkout_branch("main").unwrap();
    backend.stack_new("feat-b").unwrap();
    commit(&dir, "shared.txt", "b-version\n", "B1");
    let feat_b_before = backend.branch_tip("feat-b").unwrap().unwrap();

    // Advance main so both children need restacking, editing the shared file so
    // feat-b's rebase conflicts.
    backend.checkout_branch("main").unwrap();
    commit(&dir, "shared.txt", "main-version\n", "M1");

    let outcome = backend.restack().unwrap();
    assert_eq!(
        outcome.restacked,
        vec!["feat-a -> main".to_owned()],
        "the clean branch still restacks"
    );
    assert_eq!(
        outcome.conflicted,
        vec!["feat-b".to_owned()],
        "the conflicting branch is reported, not fatal"
    );

    // feat-b is untouched (self-aborted rebase), still at its old tip.
    assert_eq!(
        backend.branch_tip("feat-b").unwrap().unwrap(),
        feat_b_before,
        "the conflicting branch is left at its old base for manual resolve"
    );
    // We are back on the branch we started restack from.
    assert_eq!(
        backend.status().unwrap().head.branch.as_deref(),
        Some("main")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn stack_file_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".git/rgit/stacks.toml")).unwrap()
}

#[test]
fn stack_file_stack_new_records_parent_and_base() {
    let dir = init_repo("file-new");
    commit(&dir, "f.txt", "v1\n", "A");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stack_new("feat").unwrap();

    let records = rgit_git::stack_file::read(&rgit_git::stack_file::dirs_of(&backend)).unwrap();
    let tip = backend.branch_tip("main").unwrap();
    let feat = &records["feat"];
    assert_eq!(feat.parent, "main");
    assert_eq!(feat.base, tip);
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["config", "--get-regexp", "rgit-stack"])
        .output()
        .unwrap();
    assert!(out.stdout.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stack_file_linked_worktree_sees_the_same_stack() {
    let dir = init_repo("file-worktree");
    commit(&dir, "f.txt", "v1\n", "A");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stack_new("feat").unwrap();
    backend.checkout_branch("main").unwrap();

    let linked = scratch("file-worktree-linked");
    let _ = std::fs::remove_dir_all(&linked);
    git(
        &dir,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            linked.to_str().unwrap(),
        ],
    );
    let there = Git2Backend::discover(&linked).unwrap();
    let parents: std::collections::HashMap<_, _> =
        there.stack_parents().unwrap().into_iter().collect();
    assert_eq!(
        parents.get("feat").cloned().flatten(),
        Some("main".to_owned())
    );
    let _ = std::fs::remove_dir_all(&linked);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stack_file_parallel_writers_keep_every_record() {
    let dir = init_repo("file-parallel");
    commit(&dir, "f.txt", "v1\n", "A");
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let backend = Git2Backend::discover(&dir).unwrap();
                let dirs = rgit_git::stack_file::dirs_of(&backend);
                rgit_git::stack_file::record(&dirs, &format!("b{i}"), "main", None).unwrap();
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let backend = Git2Backend::discover(&dir).unwrap();
    let records = rgit_git::stack_file::read(&rgit_git::stack_file::dirs_of(&backend)).unwrap();
    assert_eq!(records.len(), 8);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stack_file_keeps_comments() {
    let dir = init_repo("file-comments");
    commit(&dir, "f.txt", "v1\n", "A");
    std::fs::create_dir_all(dir.join(".git/rgit")).unwrap();
    std::fs::write(dir.join(".git/rgit/stacks.toml"), "# mine\n").unwrap();
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stack_new("feat/x").unwrap();
    let text = stack_file_text(&dir);
    assert!(text.contains("# mine"), "{text}");
    assert!(text.contains("[branch.\"feat/x\"]"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
