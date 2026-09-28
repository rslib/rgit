//! cherry-pick, revert, merge and rebase options run natively and give the same
//! result as git. Each case builds the same history twice (fixed dates, so the
//! same commit ids), runs git in one copy and rgit in the other, and compares.

use std::path::{Path, PathBuf};
use std::process::Command;

const DATE: &str = "2020-01-01T00:00:00Z";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rgit(dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap();
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        out.status.success(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, success) = rgit(dir, args);
    assert!(success, "rgit {args:?}: {out}");
    out
}

fn fails(dir: &Path, args: &[&str]) -> String {
    let (out, success) = rgit(dir, args);
    assert!(!success, "rgit {args:?} should fail: {out}");
    out
}

fn commit(dir: &Path, path: &str, text: &str, subject: &str) {
    std::fs::write(dir.join(path), text).unwrap();
    git(dir, &["add", path]);
    git(dir, &["commit", "-qm", subject]);
}

fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-seq-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    dir
}

/// The same history in two repos: one for git, one for rgit.
fn twins(tag: &str, build: fn(&Path)) -> (PathBuf, PathBuf) {
    let a = repo(&format!("{tag}-git"));
    let b = repo(&format!("{tag}-rgit"));
    build(&a);
    build(&b);
    assert_eq!(head(&a), head(&b), "twins must start identical");
    (a, b)
}

fn head(dir: &Path) -> String {
    git(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Messages and trees of the last `n` commits, and the index tree.
fn result(dir: &Path, n: usize) -> String {
    let log = git(dir, &["log", &format!("-{n}"), "--format=%T %P%n%B"]);
    // Parents differ once commits are made at different times; count them only.
    let log: String = log
        .lines()
        .map(|l| match l.split_once(' ') {
            Some((tree, parents)) if tree.len() == 40 && parents.len() % 41 == 40 => {
                format!("{tree} parents={}", parents.split(' ').count())
            }
            _ => l.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{log}\nindex {}", git(dir, &["write-tree"]).trim())
}

fn state_file(dir: &Path, name: &str) -> bool {
    dir.join(".git").join(name).exists()
}

/// main: base; side: s1, s2, s3 (each a new file).
fn side_branch(dir: &Path) {
    commit(dir, "base", "base\n", "base");
    git(dir, &["checkout", "-qb", "side"]);
    commit(dir, "s1", "1\n", "s1");
    commit(dir, "s2", "2\n", "s2");
    commit(dir, "s3", "3\n", "s3");
    git(dir, &["checkout", "-q", "main"]);
}

#[test]
fn cherry_pick_range_with_x_matches_git() {
    let (a, b) = twins("range", side_branch);
    git(&a, &["cherry-pick", "-x", "side~2..side"]);
    ok(&b, &["cherry-pick", "-x", "side~2..side"]);
    assert_eq!(result(&a, 3), result(&b, 3));
    assert!(result(&b, 1).contains("(cherry picked from commit"));
}

#[test]
fn cherry_pick_several_commits_in_given_order() {
    let (a, b) = twins("list", side_branch);
    git(&a, &["cherry-pick", "side", "side~2"]);
    ok(&b, &["cherry-pick", "side", "side~2"]);
    assert_eq!(result(&a, 3), result(&b, 3));
}

#[test]
fn cherry_pick_no_commit_stages_several() {
    let (a, b) = twins("nocommit", side_branch);
    git(&a, &["cherry-pick", "-n", "side~1", "side"]);
    ok(&b, &["cherry-pick", "-n", "side~1", "side"]);
    assert_eq!(head(&a), head(&b));
    assert_eq!(result(&a, 1), result(&b, 1));
}

#[test]
fn revert_range_newest_first_matches_git() {
    let (a, b) = twins("revert", side_branch);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "side"]);
    }
    git(&a, &["revert", "--no-edit", "HEAD~2..HEAD"]);
    ok(&b, &["revert", "--no-edit", "HEAD~2..HEAD"]);
    assert_eq!(result(&a, 2), result(&b, 2));
}

/// main: f=base, then m1 (f=main); side: s1 (f=side), s2 (new file g).
fn conflicting(dir: &Path) {
    commit(dir, "f", "base\n", "base");
    git(dir, &["checkout", "-qb", "side"]);
    git(dir, &["config", "user.name", "sider"]);
    commit(dir, "f", "side\n", "s1");
    commit(dir, "g", "g\n", "s2");
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["checkout", "-q", "main"]);
    commit(dir, "f", "main\n", "m1");
}

#[test]
fn cherry_pick_conflict_continue() {
    let (a, b) = twins("continue", conflicting);
    let out = fails(&b, &["cherry-pick", "side~2..side"]);
    assert!(out.contains("--continue"), "{out}");
    assert!(state_file(&b, "CHERRY_PICK_HEAD"));
    assert!(git(&b, &["status"]).contains("cherry-pick"));
    let status = ok(&b, &["--toon", "status"]);
    assert!(status.contains("cherry-pick --continue"), "{status}");

    // Continue in rgit.
    std::fs::write(b.join("f"), "resolved\n").unwrap();
    git(&b, &["add", "f"]);
    ok(&b, &["cherry-pick", "--continue"]);
    assert!(!state_file(&b, "CHERRY_PICK_HEAD"));
    assert!(!state_file(&b, "sequencer"));

    // The same stop, continued by git from rgit's sequencer state.
    fails(&a, &["cherry-pick", "side~2..side"]);
    std::fs::write(a.join("f"), "resolved\n").unwrap();
    git(&a, &["add", "f"]);
    git(&a, &["cherry-pick", "--continue"]);

    assert_eq!(result(&a, 3), result(&b, 3));
    assert_eq!(
        git(&b, &["log", "-1", "--format=%an", "HEAD~1"]).trim(),
        "sider"
    );
}

#[test]
fn cherry_pick_conflict_skip_and_abort() {
    let (a, b) = twins("skip", conflicting);
    let start = head(&a);
    fails(&a, &["cherry-pick", "side~2..side"]);
    ok(&a, &["cherry-pick", "--abort"]);
    assert_eq!(head(&a), start);
    assert!(!state_file(&a, "sequencer"));
    assert_eq!(git(&a, &["status", "--short"]), "");

    fails(&b, &["cherry-pick", "side~2..side"]);
    ok(&b, &["cherry-pick", "--skip"]);
    assert_eq!(
        git(&b, &["log", "--format=%s", "-2"]),
        "s2\nm1\n",
        "s1 skipped, s2 applied"
    );
    assert!(!state_file(&b, "sequencer"));
}

#[test]
fn revert_conflict_continue_and_strategy_option() {
    let (a, b) = twins("revert-x", conflicting);
    for d in [&a, &b] {
        git(d, &["merge", "-q", "--no-edit", "-X", "ours", "side"]);
        commit(d, "f", "later\n", "later");
    }
    // Reverting s1 conflicts with "later".
    fails(&b, &["revert", "side~1"]);
    assert!(state_file(&b, "REVERT_HEAD"));
    std::fs::write(b.join("f"), "base\n").unwrap();
    git(&b, &["add", "f"]);
    ok(&b, &["revert", "--continue"]);
    assert!(!state_file(&b, "REVERT_HEAD"));
    assert!(git(&b, &["log", "-1", "--format=%B"]).starts_with("Revert \"s1\""));

    // -X theirs settles it without stopping, like git.
    git(&a, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&b, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&a, &["revert", "--no-edit", "-X", "theirs", "side~1"]);
    ok(&b, &["revert", "-X", "theirs", "side~1"]);
    assert_eq!(result(&a, 1), result(&b, 1));
}

/// main: base, m (a), merge of feat (b, c) with --no-ff.
fn merged(dir: &Path) {
    commit(dir, "base", "base\n", "base");
    git(dir, &["checkout", "-qb", "feat"]);
    commit(dir, "b", "b\n", "b");
    commit(dir, "c", "c\n", "c");
    git(dir, &["checkout", "-q", "main"]);
    commit(dir, "a", "a\n", "a");
    git(dir, &["merge", "-q", "--no-ff", "--no-edit", "feat"]);
}

#[test]
fn mainline_revert_and_cherry_pick_of_a_merge() {
    let (a, b) = twins("mainline", merged);
    git(&a, &["revert", "--no-edit", "-m", "1", "HEAD"]);
    ok(&b, &["revert", "-m", "1", "HEAD"]);
    assert_eq!(result(&a, 1), result(&b, 1));
    let out = fails(&b, &["revert", "HEAD~1"]);
    assert!(out.contains("mainline"), "{out}");

    for d in [&a, &b] {
        git(d, &["checkout", "-qb", "other", "main~3"]);
    }
    git(&a, &["cherry-pick", "-m", "1", "main~1"]);
    ok(&b, &["cherry-pick", "--mainline", "1", "main~1"]);
    assert_eq!(result(&a, 1), result(&b, 1));
}

/// main: base, a; feat: b, c.
fn diverged(dir: &Path) {
    commit(dir, "base", "base\n", "base");
    git(dir, &["checkout", "-qb", "feat"]);
    commit(dir, "b", "b\n", "b");
    commit(dir, "c", "c\n", "c");
    git(dir, &["checkout", "-q", "main"]);
    commit(dir, "a", "a\n", "a");
}

#[test]
fn merge_squash_stages_without_merge_head() {
    let (a, b) = twins("squash", diverged);
    git(&a, &["merge", "--squash", "feat"]);
    ok(&b, &["merge", "--squash", "feat"]);
    assert_eq!(head(&a), head(&b));
    assert_eq!(result(&a, 1), result(&b, 1));
    assert!(!state_file(&b, "MERGE_HEAD"));
    ok(&b, &["commit", "-m", "squashed"]);
    assert_eq!(git(&b, &["log", "-1", "--format=%P"]).split(' ').count(), 1);
}

#[test]
fn merge_no_commit_then_continue_and_message() {
    let (a, b) = twins("nocommit-merge", diverged);
    ok(&b, &["merge", "--no-commit", "feat"]);
    assert_eq!(head(&a), head(&b), "HEAD must not move");
    assert!(state_file(&b, "MERGE_HEAD"));
    ok(&b, &["merge", "--continue"]);
    assert_eq!(git(&b, &["log", "-1", "--format=%P"]).split(' ').count(), 2);
    assert!(!state_file(&b, "MERGE_HEAD"));

    ok(&a, &["merge", "-m", "join feat", "--no-edit", "feat"]);
    assert_eq!(git(&a, &["log", "-1", "--format=%s"]).trim(), "join feat");
    git(&a, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&b, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&a, &["merge", "--no-edit", "feat"]);
    ok(&b, &["merge", "feat"]);
    assert_eq!(result(&a, 1).lines().next(), result(&b, 1).lines().next());
}

#[test]
fn conflicted_merge_committed_later_has_both_parents() {
    let dir = repo("merge-parents");
    conflicting(&dir);
    fails(&dir, &["merge", "side"]);
    std::fs::write(dir.join("f"), "both\n").unwrap();
    git(&dir, &["add", "f"]);
    ok(&dir, &["commit", "-m", "merge side"]);
    let parents = git(&dir, &["log", "-1", "--format=%P"]);
    assert_eq!(parents.split(' ').count(), 2, "{parents}");
    assert!(!state_file(&dir, "MERGE_HEAD"));

    git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
    fails(&dir, &["merge", "side"]);
    std::fs::write(dir.join("f"), "both\n").unwrap();
    git(&dir, &["add", "f"]);
    ok(&dir, &["merge", "--continue"]);
    assert_eq!(
        git(&dir, &["log", "-1", "--format=%P"]).split(' ').count(),
        2
    );
}

#[test]
fn merge_strategy_option_and_octopus() {
    let (a, b) = twins("merge-x", conflicting);
    git(&a, &["merge", "--no-edit", "-X", "theirs", "side"]);
    ok(&b, &["merge", "-X", "theirs", "side"]);
    assert_eq!(result(&a, 1).lines().next(), result(&b, 1).lines().next());

    let dir = repo("octopus");
    diverged(&dir);
    git(&dir, &["checkout", "-qb", "third", "main~1"]);
    commit(&dir, "d", "d\n", "d");
    git(&dir, &["checkout", "-q", "main"]);
    ok(&dir, &["merge", "feat", "third"]);
    assert_eq!(
        git(&dir, &["log", "-1", "--format=%P"]).split(' ').count(),
        3
    );
}

/// main: base; work: a, b, "fixup! a".
fn fixups(dir: &Path) {
    commit(dir, "base", "base\n", "base");
    git(dir, &["checkout", "-qb", "work"]);
    commit(dir, "a", "a\n", "a");
    commit(dir, "b", "b\n", "b");
    commit(dir, "a", "a2\n", "fixup! a");
}

#[test]
fn rebase_autosquash_matches_git() {
    let (a, b) = twins("autosquash", fixups);
    git(&a, &["rebase", "-i", "--autosquash", "main"]);
    ok(&b, &["rebase", "--autosquash", "main"]);
    assert_eq!(git(&b, &["log", "--format=%s", "main.."]), "b\na\n");
    assert_eq!(result(&a, 2), result(&b, 2));
}

#[test]
fn rebase_root_with_exec_runs_for_every_commit() {
    let dir = repo("exec");
    fixups(&dir);
    ok(
        &dir,
        &["rebase", "--root", "--exec", "echo ran >> .git/exec-log"],
    );
    let log = std::fs::read_to_string(dir.join(".git/exec-log")).unwrap();
    assert_eq!(log.lines().count(), 4);
}

#[test]
fn rebase_update_refs_moves_stacked_branches() {
    let dir = repo("update-refs");
    fixups(&dir);
    git(&dir, &["branch", "lower", "work~1"]);
    git(&dir, &["checkout", "-q", "main"]);
    commit(&dir, "m", "m\n", "m");
    git(&dir, &["checkout", "-q", "work"]);
    ok(&dir, &["rebase", "--update-refs", "main"]);
    git(&dir, &["merge-base", "--is-ancestor", "main", "lower"]);
    git(&dir, &["merge-base", "--is-ancestor", "lower", "work"]);
}

#[test]
fn rebase_exec_conflict_stops_for_continue() {
    let dir = repo("rebase-stop");
    conflicting(&dir);
    git(&dir, &["checkout", "-q", "side"]);
    let out = fails(&dir, &["rebase", "--exec", "true", "main"]);
    assert!(out.contains("--continue"), "{out}");
    assert!(ok(&dir, &["--toon", "status"]).contains("rebasing"));
    std::fs::write(dir.join("f"), "resolved\n").unwrap();
    git(&dir, &["add", "f"]);
    ok(&dir, &["rebase", "--continue"]);
    assert_eq!(git(&dir, &["log", "--format=%s", "-3"]), "s2\ns1\nm1\n");
}

/// main: f (8 lines); side: s1 changes the last line, s2 adds g.
fn lines(dir: &Path) {
    commit(dir, "f", "1\n2\n3\n4\n5\n6\n7\n8\n", "base");
    git(dir, &["checkout", "-qb", "side"]);
    commit(dir, "f", "1\n2\n3\n4\n5\n6\n7\nX\n", "s1");
    commit(dir, "g", "g\n", "s2");
    git(dir, &["checkout", "-q", "main"]);
}

#[test]
fn cherry_pick_no_commit_merges_into_staged_changes() {
    let (a, b) = twins("n-staged", lines);
    for d in [&a, &b] {
        std::fs::write(d.join("f"), "Y\n2\n3\n4\n5\n6\n7\n8\n").unwrap();
        git(d, &["add", "f"]);
    }
    git(&a, &["cherry-pick", "-n", "side~1"]);
    ok(&b, &["cherry-pick", "-n", "side~1"]);
    assert_eq!(result(&a, 1), result(&b, 1));
    assert_eq!(
        std::fs::read_to_string(b.join("f")).unwrap(),
        "Y\n2\n3\n4\n5\n6\n7\nX\n"
    );
    assert_eq!(git(&b, &["status", "--short"]), "M  f\n");
}

#[test]
fn cherry_pick_signoff_and_revert_reference_match_git() {
    let (a, b) = twins("signoff", side_branch);
    git(&a, &["cherry-pick", "-x", "-s", "side"]);
    ok(&b, &["cherry-pick", "-x", "-s", "side"]);
    assert_eq!(result(&a, 1), result(&b, 1));
    assert!(result(&b, 1).contains("Signed-off-by: t <t@t>"));
    git(&a, &["revert", "--no-edit", "--reference", "side"]);
    ok(&b, &["revert", "--reference", "side"]);
    assert_eq!(result(&a, 1), result(&b, 1));
}

#[test]
fn cherry_pick_empty_commits_match_git() {
    let (a, b) = twins("empty", side_branch);
    for d in [&a, &b] {
        git(d, &["cherry-pick", "side"]);
        git(d, &["commit", "-q", "--allow-empty", "-m", "nothing"]);
        git(d, &["branch", "hollow"]);
        git(d, &["reset", "-q", "--hard", "HEAD~1"]);
    }
    // s3 is already applied: stop by default, drop or keep on request.
    fails(&b, &["cherry-pick", "side"]);
    ok(&b, &["cherry-pick", "--abort"]);
    let start = head(&b);
    ok(&b, &["cherry-pick", "--empty=drop", "side"]);
    assert_eq!(head(&b), start);
    git(&a, &["cherry-pick", "--empty=keep", "side"]);
    ok(&b, &["cherry-pick", "--empty=keep", "side"]);
    assert_eq!(result(&a, 1), result(&b, 1));
    // An empty commit needs --allow-empty.
    fails(&b, &["cherry-pick", "hollow"]);
    ok(&b, &["cherry-pick", "--quit"]);
    assert!(!state_file(&b, "CHERRY_PICK_HEAD"));
    git(&a, &["cherry-pick", "--allow-empty", "hollow"]);
    ok(&b, &["cherry-pick", "--allow-empty", "hollow"]);
    assert_eq!(result(&a, 2), result(&b, 2));
}

#[test]
fn cherry_pick_ff_moves_head_to_the_commit() {
    let dir = repo("ff");
    side_branch(&dir);
    git(&dir, &["checkout", "-q", "--detach", "side~2"]);
    ok(&dir, &["cherry-pick", "--ff", "side~1", "side"]);
    assert_eq!(head(&dir), git(&dir, &["rev-parse", "side"]).trim());
}

#[test]
fn cherry_pick_abort_keeps_a_moved_head() {
    let dir = repo("abort-moved");
    conflicting(&dir);
    fails(&dir, &["cherry-pick", "side~2..side"]);
    std::fs::write(dir.join("f"), "resolved\n").unwrap();
    git(&dir, &["add", "f"]);
    git(&dir, &["commit", "-qm", "by hand"]);
    let moved = head(&dir);
    let out = ok(&dir, &["cherry-pick", "--abort"]);
    assert!(out.contains("moved HEAD"), "{out}");
    assert_eq!(head(&dir), moved);
    assert!(!state_file(&dir, "sequencer"));
}

#[test]
fn merge_messages_and_ours_strategy_match_git() {
    let (a, b) = twins("merge-msg", diverged);
    git(&a, &["merge", "--no-edit", "--log", "feat"]);
    let out = ok(&b, &["--human", "merge", "--log", "feat"]);
    assert!(out.contains("2 files changed"), "{out}");
    assert_eq!(result(&a, 1), result(&b, 1));

    for d in [&a, &b] {
        git(d, &["reset", "-q", "--hard", "HEAD~1"]);
        git(d, &["checkout", "-qb", "other"]);
    }
    git(&a, &["merge", "--no-edit", "-s", "ours", "feat"]);
    let out = ok(&b, &["--human", "merge", "-s", "ours", "-n", "feat"]);
    assert!(!out.contains("changed"), "{out}");
    assert_eq!(result(&a, 1), result(&b, 1));
}

/// `diverged`, plus `lone`: a root commit of its own.
fn unrelated(dir: &Path) {
    diverged(dir);
    git(dir, &["checkout", "-q", "--orphan", "lone"]);
    git(dir, &["rm", "-qrf", "."]);
    commit(dir, "z", "z\n", "lone");
    git(dir, &["checkout", "-q", "main"]);
}

#[test]
fn merge_unrelated_histories_needs_the_flag() {
    let (a, b) = twins("unrelated", unrelated);
    let out = fails(&b, &["merge", "lone"]);
    assert!(out.contains("unrelated histories"), "{out}");
    git(
        &a,
        &["merge", "--no-edit", "--allow-unrelated-histories", "lone"],
    );
    ok(&b, &["merge", "--allow-unrelated-histories", "lone"]);
    assert_eq!(result(&a, 1), result(&b, 1));
}

#[test]
fn merge_squash_message_feeds_the_next_commit() {
    let (a, b) = twins("squash-msg", diverged);
    git(&a, &["merge", "--squash", "feat"]);
    ok(&b, &["merge", "--squash", "feat"]);
    assert_eq!(
        std::fs::read_to_string(a.join(".git/SQUASH_MSG")).unwrap(),
        std::fs::read_to_string(b.join(".git/SQUASH_MSG")).unwrap()
    );
    git(&a, &["commit", "-q", "--no-edit"]);
    ok(&b, &["commit"]);
    assert_eq!(result(&a, 1), result(&b, 1));
    assert!(!state_file(&b, "SQUASH_MSG"));
}

#[test]
fn merge_quit_keeps_the_conflicted_files() {
    let dir = repo("merge-quit");
    conflicting(&dir);
    fails(&dir, &["merge", "side"]);
    ok(&dir, &["merge", "--quit"]);
    assert!(!state_file(&dir, "MERGE_HEAD"));
    assert!(
        std::fs::read_to_string(dir.join("f"))
            .unwrap()
            .contains("<<<<<<<")
    );
}

#[test]
fn rebase_stop_is_reported_cleanly_with_progress_in_status() {
    let dir = repo("rebase-clean");
    conflicting(&dir);
    git(&dir, &["checkout", "-q", "side"]);
    let out = fails(&dir, &["--human", "rebase", "main"]);
    assert!(
        !out.contains('\r') && !out.contains("Rebasing ("),
        "{out:?}"
    );
    assert!(
        out.lines()
            .any(|l| l.contains("CONFLICT (content): Merge conflict in f")),
        "{out}"
    );
    let status = ok(&dir, &["--human", "status"]);
    for line in [
        "Last command done (1 command done):",
        "Next command to do (1 remaining command):",
        "You are currently rebasing branch 'side'",
    ] {
        assert!(status.contains(line), "{status}");
    }
    let toon = ok(&dir, &["--toon", "status"]);
    assert!(
        toon.contains("step: 1/2") && toon.contains("remaining: 1"),
        "{toon}"
    );
    let patch = ok(&dir, &["--human", "rebase", "--show-current-patch"]);
    assert!(patch.contains("s1"), "{patch}");
    ok(&dir, &["rebase", "--quit"]);
    assert!(!dir.join(".git/rebase-merge").exists());
    assert_ne!(head(&dir), git(&dir, &["rev-parse", "side"]).trim());
}

#[test]
fn rebase_date_and_force_flags_match_git() {
    let (a, b) = twins("rebase-flags", diverged);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "feat"]);
    }
    git(
        &a,
        &["rebase", "-f", "--committer-date-is-author-date", "main"],
    );
    ok(
        &b,
        &["rebase", "-f", "--committer-date-is-author-date", "main"],
    );
    assert_eq!(head(&a), head(&b));
}

/// main: n1..n6, each writing its number to n.
fn numbers(dir: &Path) {
    for i in 1..=6 {
        commit(dir, "n", &format!("{i}\n"), &format!("n{i}"));
    }
}

#[test]
fn bisect_steps_to_the_first_bad_commit() {
    let dir = repo("bisect");
    numbers(&dir);
    let out = ok(&dir, &["--toon", "bisect", "start", "HEAD", "HEAD~5"]);
    assert!(
        out.contains("remaining:") && out.contains("current:"),
        "{out}"
    );
    let left = ok(&dir, &["--toon", "bisect", "visualize"]);
    assert!(left.contains("remaining: 5"), "{left}");
    let out = ok(
        &dir,
        &["--toon", "bisect", "run", "sh", "-c", "test $(cat n) -lt 4"],
    );
    let n4 = git(&dir, &["rev-parse", "--short=7", "main~2"]);
    assert!(out.contains(&format!("first_bad: {}", n4.trim())), "{out}");
    let log = ok(&dir, &["--human", "bisect", "log"]);
    assert!(log.contains("# first bad commit"), "{log}");
    std::fs::write(dir.join(".git/saved-log"), log).unwrap();
    ok(&dir, &["bisect", "reset"]);
    assert_eq!(
        git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main"
    );
    ok(&dir, &["bisect", "replay", ".git/saved-log"]);
    assert!(state_file(&dir, "BISECT_LOG"));
    let out = fails(&dir, &["bisect", "fixed"]);
    assert!(out.contains("unknown bisect subcommand"), "{out}");
    ok(&dir, &["bisect", "reset"]);
}

#[test]
fn merge_during_bisect_keeps_the_bisect_log() {
    let dir = repo("bisect-log");
    side_branch(&dir);
    git(&dir, &["checkout", "-qb", "other"]);
    commit(&dir, "o", "o\n", "o");
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["bisect", "start", "side", "main"]);
    ok(&dir, &["merge", "other"]);
    assert!(state_file(&dir, "BISECT_LOG"));
}
