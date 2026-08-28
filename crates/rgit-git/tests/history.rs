//! History-rewriting operations: rebase, reset, cherry-pick, revert, and the
//! redo half of the op-log (undo is covered in tests/oplog.rs).

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend, ResetMode};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-history-{}-{name}", std::process::id()))
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

fn parent(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", &format!("{rev}^")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn rebase_onto_replays_the_branch_on_top_of_the_new_base() {
    let dir = init_repo("rebase-onto");
    std::fs::write(dir.join("c0.txt"), "c0\n").unwrap();
    git(&dir, &["add", "c0.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);

    git(&dir, &["checkout", "-qb", "topic"]);
    std::fs::write(dir.join("t1.txt"), "t1\n").unwrap();
    git(&dir, &["add", "t1.txt"]);
    git(&dir, &["commit", "-qm", "T1"]);

    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("m1.txt"), "m1\n").unwrap();
    git(&dir, &["add", "m1.txt"]);
    git(&dir, &["commit", "-qm", "M1"]);
    let m1 = head(&dir);

    git(&dir, &["checkout", "-q", "topic"]);
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.rebase_onto("main", &|_| {}).unwrap();

    let topic_tip = head(&dir);
    assert_eq!(parent(&dir, &topic_tip), m1, "topic should now sit on M1");
    assert!(dir.join("m1.txt").exists(), "M1's file should be present");
    assert!(dir.join("t1.txt").exists(), "T1's own file should remain");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reset_soft_moves_head_but_keeps_index_and_worktree() {
    let dir = init_repo("reset-soft");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);
    let a = head(&dir);

    std::fs::write(dir.join("f.txt"), "v2\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "B"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.reset("HEAD~1", ResetMode::Soft).unwrap();

    assert_eq!(head(&dir), a, "soft reset should move HEAD to A");
    // Worktree still has v2, and it is staged (matches the index).
    assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "v2\n");
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["diff", "--cached", "--name-only"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "f.txt",
        "the B change should remain staged"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reset_mixed_moves_head_and_unstages_but_keeps_worktree() {
    let dir = init_repo("reset-mixed");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);
    let a = head(&dir);

    std::fs::write(dir.join("f.txt"), "v2\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "B"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.reset("HEAD~1", ResetMode::Mixed).unwrap();

    assert_eq!(head(&dir), a, "mixed reset should move HEAD to A");
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "v2\n",
        "worktree should keep the B change"
    );
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["diff", "--cached", "--name-only"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "the B change should be unstaged"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reset_hard_moves_head_and_discards_worktree_changes() {
    let dir = init_repo("reset-hard");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);
    let a = head(&dir);

    // Throwaway commit so the test stays self-contained.
    std::fs::write(dir.join("f.txt"), "v2\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "B"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.reset("HEAD~1", ResetMode::Hard).unwrap();

    assert_eq!(head(&dir), a, "hard reset should move HEAD to A");
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "v1\n",
        "worktree should match A; the B change is gone"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cherry_pick_applies_another_branch_commit_as_a_new_commit() {
    let dir = init_repo("cherry-pick");
    std::fs::write(dir.join("c0.txt"), "c0\n").unwrap();
    git(&dir, &["add", "c0.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);

    git(&dir, &["checkout", "-qb", "feature"]);
    std::fs::write(dir.join("feat.txt"), "feat\n").unwrap();
    git(&dir, &["add", "feat.txt"]);
    git(&dir, &["commit", "-qm", "Feature"]);
    let feature_commit = head(&dir);

    git(&dir, &["checkout", "-q", "main"]);
    let before = head(&dir);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.cherry_pick(&feature_commit, false).unwrap();

    let after = head(&dir);
    assert_ne!(after, before, "cherry-pick should create a new commit");
    assert_ne!(
        after, feature_commit,
        "the new commit should have its own oid"
    );
    assert!(dir.join("feat.txt").exists(), "the change should appear");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revert_undoes_a_commit_with_a_new_commit() {
    let dir = init_repo("revert");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);

    std::fs::write(dir.join("f.txt"), "v2\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "B"]);
    let b = head(&dir);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.revert("HEAD", false).unwrap();

    let after = head(&dir);
    assert_ne!(after, b, "revert should create a new commit");
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "v1\n",
        "the file should be back to its prior content"
    );

    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["log", "--oneline", "-1"])
        .output()
        .unwrap();
    let subject = String::from_utf8_lossy(&out.stdout);
    assert!(
        subject.contains("Revert"),
        "history should have the revert commit: {subject}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn redo_restores_the_state_undone_by_undo() {
    let dir = init_repo("redo");
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);
    let a = head(&dir);

    std::fs::write(dir.join("f.txt"), "v2\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.commit("B").unwrap();
    let b = head(&dir);
    assert_ne!(b, a);

    backend.undo().unwrap();
    assert_eq!(head(&dir), a, "undo should move HEAD back to A");

    let label = backend.redo().unwrap();
    assert_eq!(label, "commit");
    assert_eq!(head(&dir), b, "redo should restore commit B");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rebase_range_replays_only_the_upstream_delta_onto_the_new_base() {
    let dir = init_repo("rebase-range");
    std::fs::write(dir.join("c0.txt"), "c0\n").unwrap();
    git(&dir, &["add", "c0.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);
    let c0 = head(&dir);

    git(&dir, &["checkout", "-qb", "parent"]);
    std::fs::write(dir.join("p1.txt"), "p1\n").unwrap();
    git(&dir, &["add", "p1.txt"]);
    git(&dir, &["commit", "-qm", "P1"]);
    let parent_tip = head(&dir);

    git(&dir, &["checkout", "-b", "child", &c0]);
    std::fs::write(dir.join("t1.txt"), "t1\n").unwrap();
    git(&dir, &["add", "t1.txt"]);
    git(&dir, &["commit", "-qm", "T1"]);

    // git rebase --onto parent c0: replay child's own commits (c0..child)
    // on top of parent, without needing child to have ever seen parent.
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.rebase_range(&c0, "parent", &|_| {}).unwrap();

    let child_tip = head(&dir);
    assert_eq!(
        parent(&dir, &child_tip),
        parent_tip,
        "child's T1 should now sit on top of parent's P1"
    );
    assert!(dir.join("p1.txt").exists(), "P1's file should be present");
    assert!(dir.join("t1.txt").exists(), "T1's own file should remain");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rebase_abort_errors_cleanly_with_nothing_to_abort() {
    let dir = init_repo("rebase-abort");
    std::fs::write(dir.join("f.txt"), "base\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(
        backend.rebase_abort().is_err(),
        "aborting with no rebase in progress should error, not panic"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rebase_abort_cleans_up_a_cli_started_rebase() {
    // A git-CLI-started rebase uses git's default "merge" backend, whose state
    // libgit2's open_rebase cannot drive. rebase_abort now shells out to
    // `git rebase --abort` so it restores the pre-rebase HEAD regardless.
    let dir = init_repo("rebase-abort-cli");
    std::fs::write(dir.join("f.txt"), "base\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);

    // topic and main edit the same line, so a rebase must conflict and stop.
    git(&dir, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(dir.join("f.txt"), "topic\n").unwrap();
    git(&dir, &["commit", "-aqm", "T1"]);
    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("f.txt"), "main\n").unwrap();
    git(&dir, &["commit", "-aqm", "M1"]);
    git(&dir, &["checkout", "-q", "topic"]);
    let topic_tip = head(&dir);

    // Start a rebase via the CLI; it stops mid-flight on the conflict.
    let status = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rebase", "main"])
        .status()
        .unwrap();
    assert!(!status.success(), "the rebase should stop on the conflict");
    assert!(dir.join(".git/rebase-merge").exists(), "rebase in progress");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.rebase_abort().unwrap();

    assert!(
        !dir.join(".git/rebase-merge").exists(),
        "abort should clear the in-progress rebase"
    );
    assert_eq!(head(&dir), topic_tip, "HEAD restored to the pre-rebase tip");

    let _ = std::fs::remove_dir_all(&dir);
}

fn message(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "-1", "--format=%B", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn count(dir: &Path) -> usize {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

fn commit(dir: &Path, file: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", msg]);
}

/// Commit `file` with an explicit author/committer date (RFC 2822 or a git
/// approxidate), so tests can pin recency.
fn commit_at(dir: &Path, file: &str, content: &str, msg: &str, date: &str) {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-qm", msg])
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn file_activity_counts_churn_and_recency() {
    let dir = init_repo("activity");
    // hot: touched three times, most recently. cold: once, long ago.
    commit_at(&dir, "cold", "c0\n", "cold", "2020-01-01T00:00:00");
    commit_at(&dir, "hot", "h0\n", "hot 1", "2020-02-01T00:00:00");
    commit_at(&dir, "hot", "h1\n", "hot 2", "2020-03-01T00:00:00");
    commit_at(&dir, "hot", "h2\n", "hot 3", "2025-06-01T00:00:00");

    let backend = Git2Backend::discover(&dir).unwrap();
    let activity = backend.file_activity(100).unwrap();

    let hot = activity.get("hot").expect("hot tracked");
    let cold = activity.get("cold").expect("cold tracked");
    assert_eq!(hot.commits, 3);
    assert_eq!(cold.commits, 1);
    assert!(hot.last_epoch > cold.last_epoch);

    let weights = rgit_git::activity_weights(&activity);
    assert!(
        weights["hot"] > weights["cold"],
        "hot {} should outweigh cold {}",
        weights["hot"],
        weights["cold"]
    );
}

#[test]
fn reword_changes_message_and_replays_descendants() {
    let dir = init_repo("reword");
    commit(&dir, "a", "a\n", "A");
    commit(&dir, "b", "b\n", "B");
    commit(&dir, "c", "c\n", "C");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.reword("HEAD~1", "B reworded").unwrap();

    assert_eq!(count(&dir), 3, "commit count is unchanged");
    assert!(message(&dir, "HEAD~1").starts_with("B reworded"));
    assert_eq!(message(&dir, "HEAD"), "C", "C still on top, message intact");
    assert!(dir.join("c").exists() && dir.join("b").exists() && dir.join("a").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn uncommit_keeps_changes_staged() {
    let dir = init_repo("uncommit");
    commit(&dir, "f", "v1\n", "A");
    let a = head(&dir);
    commit(&dir, "f", "v2\n", "B");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.uncommit(1).unwrap();

    assert_eq!(head(&dir), a, "HEAD moved back to A");
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["diff", "--cached", "--name-only"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "f", "B's change is staged");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn squash_folds_a_commit_into_its_parent() {
    let dir = init_repo("squash");
    commit(&dir, "a", "a\n", "A");
    commit(&dir, "b", "b\n", "B");
    commit(&dir, "c", "c\n", "C");
    commit(&dir, "d", "d\n", "D");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.squash("HEAD~1").unwrap(); // fold C into B

    assert_eq!(count(&dir), 3, "one fewer commit");
    for f in ["a", "b", "c", "d"] {
        assert!(dir.join(f).exists(), "{f} preserved");
    }
    assert!(message(&dir, "HEAD~1").contains('C'), "B now mentions C");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn squash_range_folds_many_into_one() {
    let dir = init_repo("squash-range");
    commit(&dir, "base", "base\n", "base");
    let base = head(&dir);
    for n in 1..=3 {
        commit(&dir, "f", &format!("wip{n}\n"), &format!("wip {n}"));
    }
    assert_eq!(count(&dir), 4);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.squash_range("HEAD~3").unwrap();

    assert_eq!(count(&dir), 2, "the three wip commits became one");
    assert_eq!(parent(&dir, "HEAD"), base, "the fold sits on base");
    assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "wip3\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn split_by_path_makes_two_commits() {
    let dir = init_repo("split");
    std::fs::write(dir.join("a"), "base\n").unwrap();
    std::fs::write(dir.join("b"), "base\n").unwrap();
    git(&dir, &["add", "a", "b"]);
    git(&dir, &["commit", "-qm", "base"]);
    std::fs::write(dir.join("a"), "base\nx\n").unwrap();
    std::fs::write(dir.join("b"), "base\ny\n").unwrap();
    git(&dir, &["add", "a", "b"]);
    git(&dir, &["commit", "-qm", "change a and b"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.split("HEAD", &["a".to_owned()]).unwrap();

    assert_eq!(count(&dir), 3, "the commit became two");
    // Part 1 (HEAD~1) touches only a; part 2 (HEAD) only b.
    let names = |rev: &str| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["show", "--name-only", "--format=", rev])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    assert_eq!(names("HEAD~1"), "a");
    assert_eq!(names("HEAD"), "b");
    assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "base\nx\n");
    assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "base\ny\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn move_reorders_independent_commits() {
    let dir = init_repo("move");
    commit(&dir, "base", "base\n", "base");
    commit(&dir, "a", "a\n", "add a");
    commit(&dir, "b", "b\n", "add b");
    commit(&dir, "c", "c\n", "add c");
    let add_c = head(&dir);
    let add_a = String::from_utf8_lossy(
        &Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["rev-parse", "HEAD~2"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_owned();

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.reorder(&add_c, &add_a, true).unwrap(); // move c before a

    assert_eq!(count(&dir), 4);
    for f in ["base", "a", "b", "c"] {
        assert!(dir.join(f).exists(), "{f} preserved");
    }
    // add c is now below add a.
    assert!(message(&dir, "HEAD~2").starts_with("add c"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prune_deletes_only_merged_branches() {
    let dir = init_repo("prune");
    commit(&dir, "base", "base\n", "base");
    git(&dir, &["branch", "merged"]);
    git(&dir, &["checkout", "-q", "merged"]);
    commit(&dir, "m", "m\n", "on merged");
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["merge", "-q", "--no-ff", "merged", "-m", "merge"]);
    git(&dir, &["branch", "open"]);
    git(&dir, &["checkout", "-q", "open"]);
    commit(&dir, "o", "o\n", "on open");
    git(&dir, &["checkout", "-q", "main"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let deleted = backend.prune_merged("HEAD").unwrap();

    assert_eq!(deleted, vec!["merged".to_owned()]);
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["branch", "--format=%(refname:short)"])
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&out.stdout);
    assert!(branches.contains("open") && branches.contains("main"));
    assert!(!branches.contains("merged"));

    let _ = std::fs::remove_dir_all(&dir);
}

fn rev(dir: &Path, r: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", r])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn sync_fast_forwards_a_branch_to_its_upstream() {
    let base = scratch("sync-remote");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let remote = base.join("remote.git");
    let work = base.join("work");
    let work2 = base.join("work2");
    let url = format!("file://{}", remote.display());

    // A bare remote, and a working clone with a user identity.
    git(&base, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    git(&base, &["clone", "-q", &url, work.to_str().unwrap()]);
    git(&work, &["config", "user.email", "t@example.com"]);
    git(&work, &["config", "user.name", "test"]);

    commit(&work, "f", "base\n", "base");
    git(&work, &["push", "-q", "-u", "origin", "main"]);
    // A tracked feature branch, then move off it so it is not current.
    git(&work, &["checkout", "-qb", "feat"]);
    commit(&work, "g", "g\n", "on feat");
    git(&work, &["push", "-q", "-u", "origin", "feat"]);
    git(&work, &["checkout", "-q", "main"]);

    // Advance origin/feat from a second clone.
    git(&base, &["clone", "-q", &url, work2.to_str().unwrap()]);
    git(&work2, &["config", "user.email", "t@example.com"]);
    git(&work2, &["config", "user.name", "test"]);
    git(&work2, &["checkout", "-q", "feat"]);
    commit(&work2, "h", "h\n", "advance feat");
    git(&work2, &["push", "-q", "origin", "feat"]);

    // Sync fetches and fast-forwards the non-current feat to its upstream.
    let backend = Git2Backend::discover(&work).unwrap();
    backend.sync(&|_| {}).unwrap();

    assert_eq!(
        rev(&work, "feat"),
        rev(&work, "origin/feat"),
        "sync should fast-forward feat to its upstream"
    );

    let _ = std::fs::remove_dir_all(&base);
}
