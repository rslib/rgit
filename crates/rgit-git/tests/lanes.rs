//! Lanes: assign uncommitted files to lanes and commit each to its own branch,
//! in one worktree, without moving HEAD or rewriting the working tree.

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-lanes-{}-{name}", std::process::id()))
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
    // A base commit so HEAD exists.
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(&dir, &["add", "base.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);
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

fn tree_files(dir: &Path, rev: &str) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["ls-tree", "--name-only", "-r", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn head_oid(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn init_records_a_default_lane_and_off_removes_the_ref() {
    let dir = init_repo("init");
    let backend = Git2Backend::discover(&dir).unwrap();

    assert!(!backend.lanes_active());
    backend.lanes_init().unwrap();
    assert!(backend.lanes_active());

    let state = backend.lanes_state().unwrap();
    assert_eq!(state.lanes.len(), 1);
    assert_eq!(state.lanes[0].name, "default");
    assert_eq!(state.lanes[0].branch, "main");

    // A second init is refused.
    assert!(backend.lanes_init().is_err());

    backend.lanes_off().unwrap();
    assert!(!backend.lanes_active());
    // The ref is gone but the base commit / branches remain untouched.
    assert!(tree_files(&dir, "main").contains(&"base.txt".to_owned()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unowned_changes_fall_to_the_default_lane_and_assign_moves_them() {
    let dir = init_repo("assign");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();
    backend.lane_new("feat").unwrap();

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();

    // Both new files start in the default lane.
    let state = backend.lanes_state().unwrap();
    let default = state.lanes.iter().find(|l| l.name == "default").unwrap();
    assert!(default.paths.contains(&"a.txt".to_owned()));
    assert!(default.paths.contains(&"b.txt".to_owned()));

    // Assigning a.txt moves it to feat and out of default.
    backend.lane_assign("feat", "a.txt").unwrap();
    let state = backend.lanes_state().unwrap();
    let feat = state.lanes.iter().find(|l| l.name == "feat").unwrap();
    let default = state.lanes.iter().find(|l| l.name == "default").unwrap();
    assert_eq!(feat.paths, vec!["a.txt".to_owned()]);
    assert!(default.paths.contains(&"b.txt".to_owned()));
    assert!(!default.paths.contains(&"a.txt".to_owned()));

    // Assigning to a missing lane errors.
    assert!(backend.lane_assign("nope", "b.txt").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lane_commit_takes_only_its_files_and_leaves_head_and_worktree_alone() {
    let dir = init_repo("commit");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();
    backend.lane_new("feat").unwrap();

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    backend.lane_assign("feat", "a.txt").unwrap();

    let head_before = head_oid(&dir);
    let out = backend.lane_commit("feat", "add a").unwrap();
    assert!(out.starts_with("[feat "), "{out}");

    // feat exists and carries only base.txt + a.txt (not b.txt).
    let feat = tree_files(&dir, "feat");
    assert!(feat.contains(&"a.txt".to_owned()));
    assert!(feat.contains(&"base.txt".to_owned()));
    assert!(!feat.contains(&"b.txt".to_owned()));

    // HEAD (main) did not move, and the worktree still has both files on disk.
    assert_eq!(head_oid(&dir), head_before, "HEAD must not move");
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "a\n");
    assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");

    // feat keeps a.txt (still a diff from base); it does not fall back to
    // default, and b.txt stays pending in default.
    let state = backend.lanes_state().unwrap();
    let feat_lane = state.lanes.iter().find(|l| l.name == "feat").unwrap();
    assert_eq!(feat_lane.paths, vec!["a.txt".to_owned()]);
    let default = state.lanes.iter().find(|l| l.name == "default").unwrap();
    assert!(default.paths.contains(&"b.txt".to_owned()));
    assert!(!default.paths.contains(&"a.txt".to_owned()));

    // Committing again with no further edits has nothing new to commit.
    assert!(backend.lane_commit("feat", "again").is_err());

    // A further edit to a.txt still flows to feat and commits a second time.
    std::fs::write(dir.join("a.txt"), "a2\n").unwrap();
    backend.lane_commit("feat", "edit a").unwrap();
    assert_eq!(
        Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["rev-list", "--count", "feat"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap(),
        "3",
        "feat now has C0 + two lane commits"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reconcile_drops_ownership_of_a_reverted_path() {
    let dir = init_repo("reconcile");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();
    backend.lane_new("feat").unwrap();

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    backend.lane_assign("feat", "a.txt").unwrap();
    assert!(
        backend
            .lanes_state()
            .unwrap()
            .lanes
            .iter()
            .any(|l| l.paths.contains(&"a.txt".to_owned()))
    );

    // Revert the change (remove the file): it is no longer a diff from base.
    std::fs::remove_file(dir.join("a.txt")).unwrap();
    let state = backend.lanes_state().unwrap();
    assert!(
        state
            .lanes
            .iter()
            .all(|l| !l.paths.contains(&"a.txt".to_owned())),
        "a reverted path is dropped from every lane"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undo_reverses_a_lane_commit() {
    let dir = init_repo("undo");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();
    backend.lane_new("feat").unwrap();

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    backend.lane_assign("feat", "a.txt").unwrap();
    backend.lane_commit("feat", "add a").unwrap();
    assert!(backend.branch_exists("feat"), "feat was created");

    backend.undo().unwrap();
    assert!(
        !backend.branch_exists("feat"),
        "undo removes the branch the lane commit created"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn show(dir: &Path, spec: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["show", spec])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// One file's two hunks are split across two lanes and each commits only its own
// hunk to its own branch.
#[test]
fn a_files_hunks_split_across_lanes() {
    let dir = init_repo("hunks");
    // A 20-line file so two edits land in two separate hunks.
    let mut lines: Vec<String> = (1..=20).map(|n| format!("l{n}")).collect();
    std::fs::write(dir.join("f.txt"), lines.join("\n") + "\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "seed"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();
    backend.lane_new("top").unwrap();

    // Edit line 3 and line 17: two hunks in one file.
    lines[2] = "l3-CHANGED".into();
    lines[16] = "l17-CHANGED".into();
    std::fs::write(dir.join("f.txt"), lines.join("\n") + "\n").unwrap();

    let st = backend.status_full().unwrap();
    let hunks = st
        .unstaged_diff("f.txt")
        .expect("f.txt is changed")
        .hunks
        .clone();
    assert_eq!(hunks.len(), 2, "expected two separate hunks");
    let first = hunks[0].new_start;

    // Assign the first hunk (line 3) to `top`; the second stays in default.
    backend.lane_assign_hunk("top", "f.txt", first).unwrap();
    let state = backend.lanes_state().unwrap();
    let top = state.lanes.iter().find(|l| l.name == "top").unwrap();
    let default = state.lanes.iter().find(|l| l.name == "default").unwrap();
    assert_eq!(top.hunks.len(), 1, "top owns one hunk of f.txt");
    assert_eq!(default.hunks.len(), 1, "default owns the other hunk");
    assert!(
        default.paths.is_empty(),
        "f.txt is hunk-managed, not whole-file"
    );

    // Commit top: its branch gets the line-3 change but NOT the line-17 change.
    backend.lane_commit("top", "line 3").unwrap();
    let top_file = show(&dir, "top:f.txt");
    assert!(top_file.contains("l3-CHANGED"), "top has the line-3 change");
    assert!(
        !top_file.contains("l17-CHANGED"),
        "top must not have line-17"
    );

    // Commit default (-> main): the line-17 change but NOT line-3.
    backend.lane_commit("default", "line 17").unwrap();
    let main_file = show(&dir, "main:f.txt");
    assert!(
        main_file.contains("l17-CHANGED"),
        "main has the line-17 change"
    );
    assert!(
        !main_file.contains("l3-CHANGED"),
        "main must not have line-3"
    );

    // The worktree still has both edits.
    let wt = std::fs::read_to_string(dir.join("f.txt")).unwrap();
    assert!(wt.contains("l3-CHANGED") && wt.contains("l17-CHANGED"));

    let _ = std::fs::remove_dir_all(&dir);
}

// After committing, a lane's committed commits show in its state (git-safe M4).
#[test]
fn lane_state_shows_committed_commits() {
    let dir = init_repo("committed");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();
    backend.lane_new("feat").unwrap();

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    backend.lane_assign("feat", "a.txt").unwrap();
    backend.lane_commit("feat", "add a").unwrap();

    let state = backend.lanes_state().unwrap();
    let feat = state.lanes.iter().find(|l| l.name == "feat").unwrap();
    assert_eq!(feat.commits.len(), 1, "feat has one commit above base");
    assert_eq!(feat.commits[0].1, "add a");
    // default has no commits yet.
    let default = state.lanes.iter().find(|l| l.name == "default").unwrap();
    assert!(default.commits.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

// Renaming a lane renames its branch too.
#[test]
fn lane_rename_moves_the_branch() {
    let dir = init_repo("rename");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();
    backend.lane_new("feat").unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    backend.lane_assign("feat", "a.txt").unwrap();
    backend.lane_commit("feat", "add a").unwrap();

    backend.lane_rename("feat", "feature-x").unwrap();
    assert!(!backend.branch_exists("feat"));
    assert!(backend.branch_exists("feature-x"));
    let state = backend.lanes_state().unwrap();
    assert!(state.lanes.iter().any(|l| l.name == "feature-x"));
    assert!(!state.lanes.iter().any(|l| l.name == "feat"));

    // The default lane cannot be renamed.
    assert!(backend.lane_rename("default", "nope").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

// Deleting a lane returns its changes to default and keeps its branch.
#[test]
fn lane_delete_returns_changes_to_default() {
    let dir = init_repo("delete");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();
    backend.lane_new("feat").unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    backend.lane_assign("feat", "a.txt").unwrap();

    backend.lane_delete("feat").unwrap();
    let state = backend.lanes_state().unwrap();
    assert!(!state.lanes.iter().any(|l| l.name == "feat"));
    let default = state.lanes.iter().find(|l| l.name == "default").unwrap();
    assert!(
        default.paths.contains(&"a.txt".to_owned()),
        "a.txt returns to default"
    );

    // The default lane cannot be deleted.
    assert!(backend.lane_delete("default").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

fn rev(dir: &Path, spec: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", spec])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

// A stacked lane's commits build on the parent lane's branch, not the fork point.
#[test]
fn stacked_lane_builds_on_its_parent_lane() {
    let dir = init_repo("stacked");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();

    backend.lane_new("feat-a").unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    backend.lane_assign("feat-a", "a.txt").unwrap();
    backend.lane_commit("feat-a", "A1").unwrap();

    backend.lane_stack("feat-b", "feat-a").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    backend.lane_assign("feat-b", "b.txt").unwrap();
    backend.lane_commit("feat-b", "B1").unwrap();

    // feat-b carries a.txt (from feat-a) AND b.txt, and forks off feat-a's tip.
    let feat_b = tree_files(&dir, "feat-b");
    assert!(
        feat_b.contains(&"a.txt".to_owned()),
        "inherits feat-a's a.txt"
    );
    assert!(feat_b.contains(&"b.txt".to_owned()));
    assert_eq!(
        rev(&dir, "feat-b~1"),
        rev(&dir, "feat-a"),
        "feat-b forks off feat-a"
    );

    // The git stack config records the relationship (composes with restack).
    let parents = backend.stack_parents().unwrap();
    let fb = parents.iter().find(|(b, _)| b == "feat-b").unwrap();
    assert_eq!(fb.1.as_deref(), Some("feat-a"));

    let _ = std::fs::remove_dir_all(&dir);
}

// Stacking on a lane that has not committed yet is refused at commit time.
#[test]
fn stacked_lane_requires_the_parent_to_be_committed() {
    let dir = init_repo("stacked-order");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();
    backend.lane_new("base-lane").unwrap();
    backend.lane_stack("child", "base-lane").unwrap();

    std::fs::write(dir.join("c.txt"), "c\n").unwrap();
    backend.lane_assign("child", "c.txt").unwrap();
    // base-lane has no branch yet, so the child cannot fork off it.
    assert!(backend.lane_commit("child", "C1").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

// lane_restack moves a child lane onto the parent's new tip in the odb, without
// touching the (dirty) worktree or HEAD.
#[test]
fn lane_restack_moves_child_onto_parents_new_tip() {
    let dir = init_repo("lane-restack");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.lanes_init().unwrap();

    backend.lane_new("feat-a").unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    backend.lane_assign("feat-a", "a.txt").unwrap();
    backend.lane_commit("feat-a", "A1").unwrap();

    backend.lane_stack("feat-b", "feat-a").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    backend.lane_assign("feat-b", "b.txt").unwrap();
    backend.lane_commit("feat-b", "B1").unwrap();

    // Advance feat-a with a second commit; feat-b is now stale.
    std::fs::write(dir.join("a2.txt"), "a2\n").unwrap();
    backend.lane_assign("feat-a", "a2.txt").unwrap();
    backend.lane_commit("feat-a", "A2").unwrap();
    assert_ne!(
        rev(&dir, "feat-b~1"),
        rev(&dir, "feat-a"),
        "feat-b is stale before restack"
    );

    let head_before = head_oid(&dir);
    let outcome = backend.lane_restack().unwrap();
    assert_eq!(outcome.restacked, vec!["feat-b -> feat-a".to_owned()]);
    assert!(outcome.conflicted.is_empty());

    // feat-b now forks off feat-a's new tip and inherits a2.txt.
    assert_eq!(
        rev(&dir, "feat-b~1"),
        rev(&dir, "feat-a"),
        "feat-b moved onto feat-a"
    );
    let feat_b = tree_files(&dir, "feat-b");
    assert!(
        feat_b.contains(&"a2.txt".to_owned()),
        "inherits the parent's new file"
    );
    assert!(feat_b.contains(&"b.txt".to_owned()));

    // HEAD and the dirty worktree are untouched.
    assert_eq!(head_oid(&dir), head_before, "HEAD did not move");
    assert!(
        dir.join("a.txt").exists() && dir.join("b.txt").exists() && dir.join("a2.txt").exists()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
