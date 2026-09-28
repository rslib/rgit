//! Git-compatibility behaviors: how `GitBackend` matches real git's semantics
//! for branch deletion, clean, stash scoping, merges, tags, and remotes.

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend, LogOptions};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-compat-{}-{name}", std::process::id()))
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

fn rev(dir: &Path, r: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", r])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
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
fn delete_branch_requires_force_when_not_merged() {
    let dir = init_repo("delete-branch-unmerged");
    commit(&dir, "base", "base\n", "base");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "f", "f\n", "on feat");
    git(&dir, &["checkout", "-q", "main"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(
        backend.delete_branch("feat", false).is_err(),
        "feat has commits not on main; deleting without force should error"
    );
    let branches = backend.local_branches().unwrap();
    assert!(branches.contains(&"feat".to_owned()), "feat still exists");

    backend.delete_branch("feat", true).unwrap();
    let branches = backend.local_branches().unwrap();
    assert!(!branches.contains(&"feat".to_owned()), "force deleted feat");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn delete_branch_merged_into_head_needs_no_force() {
    let dir = init_repo("delete-branch-merged");
    commit(&dir, "base", "base\n", "base");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "f", "f\n", "on feat");
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["merge", "-q", "feat"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.delete_branch("feat", false).unwrap();
    assert!(
        !backend
            .local_branches()
            .unwrap()
            .contains(&"feat".to_owned())
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clean_dry_run_lists_without_deleting() {
    let dir = init_repo("clean");
    commit(&dir, "tracked", "t\n", "seed");
    std::fs::write(dir.join("junk.txt"), "trash\n").unwrap();

    let backend = Git2Backend::discover(&dir).unwrap();
    let out = backend.clean(true, &[]).unwrap();
    assert!(
        out.contains("junk.txt"),
        "dry run should mention the file: {out}"
    );
    assert!(dir.join("junk.txt").exists(), "dry run must not delete");

    backend.clean(false, &[]).unwrap();
    assert!(!dir.join("junk.txt").exists(), "real clean removes it");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stash_push_untracked_scoping() {
    let dir = init_repo("stash-untracked");
    commit(&dir, "tracked", "v1\n", "seed");

    let backend = Git2Backend::discover(&dir).unwrap();

    // Without include_untracked, only the tracked change is stashed.
    std::fs::write(dir.join("tracked"), "v2\n").unwrap();
    std::fs::write(dir.join("untracked.txt"), "u\n").unwrap();
    backend.stash_push(false).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("tracked")).unwrap(),
        "v1\n"
    );
    assert!(
        dir.join("untracked.txt").exists(),
        "untracked file should survive a non-untracked stash"
    );
    backend.stash_pop(0).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("tracked")).unwrap(),
        "v2\n"
    );

    // With include_untracked, the untracked file is stashed away too.
    backend.stash_push(true).unwrap();
    assert!(
        !dir.join("untracked.txt").exists(),
        "include_untracked should sweep up the untracked file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn status_separates_staged_and_unstaged_paths() {
    let dir = init_repo("staged-vs-unstaged");
    std::fs::write(dir.join("a.txt"), "a1\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b1\n").unwrap();
    git(&dir, &["add", "a.txt", "b.txt"]);
    git(&dir, &["commit", "-qm", "seed"]);

    std::fs::write(dir.join("a.txt"), "a2\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    std::fs::write(dir.join("b.txt"), "b2\n").unwrap();

    let backend = Git2Backend::discover(&dir).unwrap();
    let status = backend.status().unwrap();

    assert!(status.staged.iter().any(|f| f.path == "a.txt"));
    assert!(!status.staged.iter().any(|f| f.path == "b.txt"));
    assert!(status.unstaged.iter().any(|f| f.path == "b.txt"));
    assert!(!status.unstaged.iter().any(|f| f.path == "a.txt"));
}

#[test]
fn merge_ff_only_errors_when_it_cannot_fast_forward() {
    let dir = init_repo("merge-ff-only");
    commit(&dir, "base", "base\n", "base");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "f", "f\n", "on feat");
    git(&dir, &["checkout", "-q", "main"]);
    commit(&dir, "m", "m\n", "on main");

    let backend = Git2Backend::discover(&dir).unwrap();
    let err = backend.merge("feat", false, true, &|_| {});
    assert!(
        err.is_err(),
        "diverged branches cannot fast-forward, and ff_only should refuse a merge commit"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_abort_clears_a_conflicted_merge() {
    let dir = init_repo("merge-abort");
    commit(&dir, "f.txt", "base\n", "base");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "f.txt", "feat\n", "on feat");
    git(&dir, &["checkout", "-q", "main"]);
    commit(&dir, "f.txt", "main\n", "on main");

    let backend = Git2Backend::discover(&dir).unwrap();
    let result = backend.merge("feat", false, false, &|_| {});
    assert!(result.is_err(), "conflicting edits should fail the merge");
    assert!(
        dir.join(".git/MERGE_HEAD").exists(),
        "merge left in progress"
    );

    backend.merge_abort().unwrap();
    assert!(
        !dir.join(".git/MERGE_HEAD").exists(),
        "abort should clear MERGE_HEAD"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "main\n",
        "worktree restored to main's version"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remote_branches_lists_the_tracking_refs_after_a_clone() {
    let base = scratch("remote-branches");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let remote = base.join("remote.git");
    let work = base.join("work");
    let clone = base.join("clone");
    let url = format!("file://{}", remote.display());

    git(
        &base,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    git(&base, &["clone", "-q", &url, work.to_str().unwrap()]);
    git(&work, &["config", "user.email", "t@example.com"]);
    git(&work, &["config", "user.name", "test"]);
    commit(&work, "f", "base\n", "base");
    git(&work, &["push", "-q", "-u", "origin", "main"]);

    git(&base, &["clone", "-q", &url, clone.to_str().unwrap()]);
    let backend = Git2Backend::discover(&clone).unwrap();
    let branches = backend.remote_branches().unwrap();
    assert!(
        branches.iter().any(|b| b == "origin/main"),
        "expected origin/main in {branches:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn set_remote_url_and_rename_remote() {
    let dir = init_repo("remote-rename");
    commit(&dir, "a", "a\n", "seed");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend
        .add_remote("origin", "https://example.com/one.git")
        .unwrap();

    backend
        .set_remote_url("origin", "https://example.com/two.git")
        .unwrap();
    let remotes = backend.remotes().unwrap();
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].url, "https://example.com/two.git");

    backend.rename_remote("origin", "upstream").unwrap();
    let remotes = backend.remotes().unwrap();
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name, "upstream");
    assert_eq!(remotes[0].url, "https://example.com/two.git");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cherry_pick_no_commit_leaves_head_and_stages_the_change() {
    let dir = init_repo("cherry-pick-no-commit");
    commit(&dir, "c0", "c0\n", "C0");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "feat.txt", "feat\n", "Feature");
    let feature_commit = head(&dir);
    git(&dir, &["checkout", "-q", "main"]);
    let before = head(&dir);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.cherry_pick(&feature_commit, true).unwrap();

    assert_eq!(head(&dir), before, "no_commit should not move HEAD");
    let status = backend.status().unwrap();
    assert!(!status.staged.is_empty(), "the change should be staged");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cherry_pick_normal_creates_a_new_commit() {
    let dir = init_repo("cherry-pick-commit");
    commit(&dir, "c0", "c0\n", "C0");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "feat.txt", "feat\n", "Feature");
    let feature_commit = head(&dir);
    git(&dir, &["checkout", "-q", "main"]);
    let before = head(&dir);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.cherry_pick(&feature_commit, false).unwrap();

    assert_ne!(head(&dir), before, "cherry-pick should create a new commit");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revert_no_commit_leaves_head_and_stages_the_inverse() {
    let dir = init_repo("revert-no-commit");
    commit(&dir, "f.txt", "v1\n", "A");
    commit(&dir, "f.txt", "v2\n", "B");
    let before = head(&dir);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.revert("HEAD", true).unwrap();

    assert_eq!(head(&dir), before, "no_commit should not move HEAD");
    let status = backend.status().unwrap();
    assert!(
        !status.staged.is_empty(),
        "the inverse change should be staged"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "v1\n",
        "worktree reflects the reverted content"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rebase_range_replays_feat_commits_onto_advanced_main() {
    let dir = init_repo("rebase-range-compat");
    commit(&dir, "base.txt", "base\n", "base");
    let base = head(&dir);

    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "f1.txt", "f1\n", "F1");
    commit(&dir, "f2.txt", "f2\n", "F2");

    git(&dir, &["checkout", "-q", "main"]);
    commit(&dir, "newbase.txt", "nb\n", "newbase");
    let main_tip = head(&dir);

    git(&dir, &["checkout", "-q", "feat"]);
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.rebase_range(&base, "main", &|_| {}).unwrap();

    let feat_tip = head(&dir);
    assert_eq!(
        rev(&dir, &format!("{feat_tip}~2")),
        main_tip,
        "feat's two commits should now sit on top of main's tip"
    );
    for f in ["newbase.txt", "f1.txt", "f2.txt"] {
        assert!(dir.join(f).exists(), "{f} should be present after rebase");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn describe_reports_the_nearest_tag() {
    let dir = init_repo("describe");
    commit(&dir, "f.txt", "v1\n", "A");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.create_tag("v1", "release 1").unwrap();

    let clean = backend
        .describe("HEAD", false, false, false, None, None)
        .unwrap();
    assert_eq!(clean, "v1");

    // `--dirty` describes the workdir: with an uncommitted change it appends
    // "-dirty"; without the flag it does not.
    std::fs::write(dir.join("f.txt"), "v1\nmodified\n").unwrap();
    let no_flag = backend
        .describe("HEAD", false, false, false, None, None)
        .unwrap();
    assert_eq!(no_flag, "v1", "no --dirty means no suffix");
    let dirty = backend
        .describe("HEAD", false, true, false, None, None)
        .unwrap();
    assert_eq!(dirty, "v1-dirty", "--dirty reports the dirty worktree");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remove_path_cached_keeps_the_worktree_file() {
    let dir = init_repo("remove-path-cached");
    commit(&dir, "f.txt", "v1\n", "seed");

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.remove_path("f.txt", true, false).unwrap();

    assert!(
        dir.join("f.txt").exists(),
        "cached removal keeps the file on disk"
    );
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["diff", "--cached", "--name-status"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("f.txt"),
        "index should show the removal staged"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn move_path_requires_force_over_an_existing_destination() {
    let dir = init_repo("move-path");
    commit(&dir, "src.txt", "source\n", "seed");
    std::fs::write(dir.join("dst.txt"), "dest\n").unwrap();
    git(&dir, &["add", "dst.txt"]);
    git(&dir, &["commit", "-qm", "add dest"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(
        backend.move_path("src.txt", "dst.txt", false).is_err(),
        "moving onto an existing path without force should error"
    );

    backend.move_path("src.txt", "dst.txt", true).unwrap();
    assert!(!dir.join("src.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("dst.txt")).unwrap(),
        "source\n",
        "force move overwrites the destination with the source content"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn log_filters_by_path() {
    let dir = init_repo("log-path");
    commit(&dir, "A", "a1\n", "add A");
    commit(&dir, "B", "b1\n", "add B");
    commit(&dir, "A", "a2\n", "update A");

    let backend = Git2Backend::discover(&dir).unwrap();
    let a_log = backend
        .log(&LogOptions {
            paths: vec!["A".into()],
            ..Default::default()
        })
        .unwrap();
    let summaries: Vec<&str> = a_log.iter().map(|e| e.summary.as_str()).collect();
    assert_eq!(summaries, vec!["update A", "add A"]);
}

#[test]
fn log_filters_by_since_and_until() {
    let dir = init_repo("log-since-until");
    commit_at(&dir, "f", "1\n", "old", "2020-01-01T00:00:00+0000");
    commit_at(&dir, "f", "2\n", "mid", "2021-01-01T00:00:00+0000");
    commit_at(&dir, "f", "3\n", "new", "2022-01-01T00:00:00+0000");

    let backend = Git2Backend::discover(&dir).unwrap();

    let since = backend
        .log(&LogOptions {
            since: Some(epoch("2021-01-01T00:00:00+0000")),
            ..Default::default()
        })
        .unwrap();
    let summaries: Vec<&str> = since.iter().map(|e| e.summary.as_str()).collect();
    assert!(
        !summaries.contains(&"old"),
        "since should exclude the older commit"
    );
    assert!(summaries.contains(&"mid") && summaries.contains(&"new"));

    let until = backend
        .log(&LogOptions {
            until: Some(epoch("2021-01-01T00:00:00+0000")),
            ..Default::default()
        })
        .unwrap();
    let summaries: Vec<&str> = until.iter().map(|e| e.summary.as_str()).collect();
    assert!(
        !summaries.contains(&"new"),
        "until should exclude the newer commit"
    );
    assert!(summaries.contains(&"old") && summaries.contains(&"mid"));
}

/// Unix seconds for an RFC 2822-ish timestamp, via `date`, so the assertion
/// uses the same clock as `commit_at`'s GIT_AUTHOR_DATE.
fn epoch(stamp: &str) -> i64 {
    let out = Command::new("date")
        .args(["-j", "-f", "%Y-%m-%dT%H:%M:%S%z", stamp, "+%s"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

#[test]
fn all_tags_orders_newest_first() {
    let dir = init_repo("all-tags");
    commit_at(&dir, "f", "1\n", "old", "2020-01-01T00:00:00+00:00");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.create_tag("old-tag", "old release").unwrap();

    commit_at(&dir, "f", "2\n", "new", "2023-01-01T00:00:00+00:00");
    backend.create_tag("new-tag", "new release").unwrap();

    let tags = backend.all_tags().unwrap();
    let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["new-tag", "old-tag"], "newest-first order");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn push_tags_and_push_delete_over_a_local_remote() {
    let base = scratch("push-tags-delete");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let remote = base.join("remote.git");
    let work = base.join("work");
    let url = format!("file://{}", remote.display());

    git(
        &base,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    git(&base, &["clone", "-q", &url, work.to_str().unwrap()]);
    git(&work, &["config", "user.email", "t@example.com"]);
    git(&work, &["config", "user.name", "test"]);
    commit(&work, "f", "base\n", "base");
    git(&work, &["push", "-q", "-u", "origin", "main"]);

    let backend = Git2Backend::discover(&work).unwrap();
    backend.create_tag("v1", "release").unwrap();
    backend.push_tags(None, &|_| {}).unwrap();

    let out = Command::new("git")
        .arg("-C")
        .arg(&work)
        .args(["ls-remote", "--tags", "origin"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("refs/tags/v1"),
        "the tag should be visible on the remote"
    );

    git(&work, &["checkout", "-qb", "throwaway"]);
    commit(&work, "g", "g\n", "on throwaway");
    git(&work, &["push", "-q", "-u", "origin", "throwaway"]);

    backend.push_delete(None, "throwaway", &|_| {}).unwrap();
    let out = Command::new("git")
        .arg("-C")
        .arg(&work)
        .args(["ls-remote", "--heads", "origin"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("throwaway"),
        "the branch should be gone from the remote"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn remove_worktree_drops_it_from_the_list() {
    let dir = init_repo("remove-worktree");
    commit(&dir, "a.txt", "x\n", "seed");

    let backend = Git2Backend::discover(&dir).unwrap();
    let wt_path = scratch("remove-worktree-linked");
    let _ = std::fs::remove_dir_all(&wt_path);
    // worktrees() lists the main worktree too, so assert on the linked ones.
    let linked = |b: &Git2Backend| {
        b.worktrees()
            .unwrap()
            .into_iter()
            .filter(|w| !w.is_main)
            .collect::<Vec<_>>()
    };
    backend
        .add_worktree("side", wt_path.to_str().unwrap())
        .unwrap();
    let after_add = linked(&backend);
    assert_eq!(after_add.len(), 1);
    assert_eq!(after_add[0].name, "side");

    backend.remove_worktree("side", true).unwrap();
    assert!(linked(&backend).is_empty());
    // The main worktree is always present.
    assert!(backend.worktrees().unwrap().iter().any(|w| w.is_main));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn worktrees_report_branch_head_and_dirty_state() {
    let dir = init_repo("worktree-inspect");
    commit(&dir, "a.txt", "x\n", "seed");

    let backend = Git2Backend::discover(&dir).unwrap();
    let wt_path = scratch("worktree-inspect-linked");
    let _ = std::fs::remove_dir_all(&wt_path);
    backend
        .add_worktree("feature", wt_path.to_str().unwrap())
        .unwrap();

    let list = backend.worktrees().unwrap();
    let main = list.iter().find(|w| w.is_main).expect("main worktree");
    assert!(main.head.is_some(), "main has a HEAD commit");
    assert!(!main.dirty, "seeded main is clean");

    let side = list
        .iter()
        .find(|w| w.name == "feature")
        .expect("linked worktree");
    assert_eq!(side.branch.as_deref(), Some("feature"));
    assert!(side.head.is_some());
    assert!(!side.dirty, "fresh worktree is clean");

    // Dirty the linked worktree; it should now report dirty.
    std::fs::write(wt_path.join("new.txt"), "hello\n").unwrap();
    let side = backend
        .worktrees()
        .unwrap()
        .into_iter()
        .find(|w| w.name == "feature")
        .unwrap();
    assert!(side.dirty, "an untracked file makes the worktree dirty");

    backend.remove_worktree("feature", true).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
