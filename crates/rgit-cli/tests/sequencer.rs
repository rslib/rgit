//! cherry-pick, revert, merge and rebase options run natively and give the same
//! result as git. Each case builds the same history twice (fixed dates, so the
//! same commit ids), runs git in one copy and rgit in the other, and compares.

use std::path::{Path, PathBuf};
use std::process::Command;

const DATE: &str = "2020-01-01T00:00:00Z";

/// Environment that keeps the user's own git config out of git and rgit.
fn isolated() -> [(&'static str, PathBuf); 3] {
    let home = std::env::temp_dir().join(format!("rgit-seq-{}-home", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    [
        ("HOME", home.clone()),
        ("XDG_CONFIG_HOME", home),
        ("GIT_CONFIG_GLOBAL", PathBuf::from("/dev/null")),
    ]
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .envs(isolated())
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
        .args(
            ["--human", "--text", "--json", "--toon", "--axi"]
                .iter()
                .all(|m| !args.contains(m))
                .then_some("--toon"),
        )
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .envs(isolated())
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
    // rgit does not sign; keep a user's commit.gpgSign out of the twins.
    git(&dir, &["config", "commit.gpgSign", "false"]);
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

/// main: base, m; a, b: a new file each; c: changes `f`.
fn heads(dir: &Path) {
    commit(dir, "f", "base\n", "base");
    for (branch, file, text) in [("a", "a", "a\n"), ("b", "b", "b\n"), ("c", "f", "c\n")] {
        git(dir, &["checkout", "-qb", branch, "main"]);
        commit(dir, file, text, branch);
    }
    git(dir, &["checkout", "-q", "main"]);
    commit(dir, "m", "m\n", "m");
}

/// git's exit status and output, for runs that may fail.
fn git_try(dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_EDITOR", "true")
        .envs(isolated())
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

#[test]
fn octopus_merges_like_git() {
    let (a, b) = twins("octopus-git", heads);
    let theirs = git(&a, &["merge", "a", "b"]);
    let ours = ok(&b, &["--human", "merge", "a", "b"]);
    assert_eq!(head(&a), head(&b));
    assert!(theirs.starts_with("Trying simple merge with a\nTrying simple merge with b\n"));
    assert!(
        ours.contains("Merge made by the 'octopus' strategy."),
        "{ours}"
    );

    for d in [&a, &b] {
        git(d, &["reset", "-q", "--hard", "HEAD~1"]);
    }
    git(
        &a,
        &[
            "merge",
            "--log",
            "--into-name=feature",
            "--no-stat",
            "a",
            "b",
        ],
    );
    ok(&b, &["merge", "--log", "--into-name=feature", "a", "b"]);
    assert_eq!(head(&a), head(&b));

    // HEAD is contained in a, so it is no parent; a is fast-forwarded to.
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "-b", "ff", "main~2"]);
    }
    git(&a, &["merge", "a", "b"]);
    ok(&b, &["merge", "a", "b"]);
    assert_eq!(head(&a), head(&b));
    assert_eq!(git(&b, &["log", "-1", "--format=%P"]).split(' ').count(), 2);

    // Only the last head may conflict; it stops with both heads recorded.
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "main"]);
        git(d, &["reset", "-q", "--hard", "HEAD~1"]);
        commit(d, "f", "x\n", "x");
    }
    assert!(!git_try(&a, &["merge", "a", "c"]).1);
    fails(&b, &["merge", "a", "c"]);
    for file in ["MERGE_HEAD", "MERGE_MSG"] {
        let read = |d: &Path| std::fs::read_to_string(d.join(".git").join(file)).unwrap();
        assert_eq!(read(&a), read(&b), "{file}");
    }
    assert_eq!(
        git(&a, &["status", "--short"]),
        git(&b, &["status", "--short"])
    );
    std::fs::write(b.join("f"), "resolved\n").unwrap();
    git(&b, &["add", "f"]);
    ok(&b, &["merge", "--continue"]);
    assert_eq!(git(&b, &["log", "-1", "--format=%P"]).split(' ').count(), 3);

    // A conflict before the last head fails and leaves the tree alone.
    for d in [&a, &b] {
        git(d, &["reset", "-q", "--hard", "HEAD~1"]);
    }
    let before = head(&b);
    let out = fails(&b, &["merge", "c", "a"]);
    assert!(out.contains("octopus failed"), "{out}");
    assert_eq!(head(&b), before);
    assert_eq!(git(&b, &["status", "--short"]), "");
}

#[test]
fn octopus_edit_opens_the_editor() {
    let dir = repo("octopus-edit");
    heads(&dir);
    let editor = dir.join(".git/edit.sh");
    std::fs::write(&editor, "#!/bin/sh\necho edited > \"$1\"\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["merge", "-e", "a", "b"])
        .current_dir(&dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_EDITOR", format!("sh {}", editor.display()))
        .envs(isolated())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(git(&dir, &["log", "-1", "--format=%B"]), "edited\n\n");
}

#[test]
fn merge_autostash_into_name_and_cleanup_match_git() {
    let (a, b) = twins("autostash", heads);
    for d in [&a, &b] {
        std::fs::write(d.join("m"), "m\ndirty\n").unwrap();
    }
    git(&a, &["merge", "--autostash", "--no-edit", "b"]);
    let out = ok(&b, &["--human", "merge", "--autostash", "b"]);
    assert!(out.contains("Applied autostash."), "{out}");
    assert_eq!(head(&a), head(&b));
    assert_eq!(std::fs::read_to_string(b.join("m")).unwrap(), "m\ndirty\n");
    assert_eq!(git(&b, &["stash", "list"]), "");

    // A conflicted merge keeps the stash in MERGE_AUTOSTASH until --abort.
    git(&b, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&b, "b", "zz\n", "zz");
    std::fs::write(b.join("m"), "m\ndirty\n").unwrap();
    git(&b, &["config", "merge.autoStash", "true"]);
    fails(&b, &["merge", "b"]);
    assert!(state_file(&b, "MERGE_AUTOSTASH"));
    ok(&b, &["merge", "--abort"]);
    assert!(!state_file(&b, "MERGE_AUTOSTASH"));
    assert_eq!(std::fs::read_to_string(b.join("m")).unwrap(), "m\ndirty\n");

    // --into-name and --cleanup shape the message as in git.
    let (a, b) = twins("into-name", heads);
    let args = [
        "merge",
        "--into-name=release",
        "--cleanup=verbatim",
        "-m",
        "x  \n\n\n# kept",
        "c",
    ];
    git(&a, &args);
    ok(&b, &args);
    assert_eq!(head(&a), head(&b));
    for d in [&a, &b] {
        git(d, &["reset", "-q", "--hard", "HEAD~1"]);
    }
    git(&a, &["merge", "--no-edit", "--into-name=release", "c"]);
    ok(&b, &["merge", "--into-name=release", "c"]);
    assert_eq!(head(&a), head(&b));
}

#[test]
fn cherry_pick_strategy_cleanup_and_drop_match_git() {
    let (a, b) = twins("pick-more", heads);
    for d in [&a, &b] {
        git(d, &["cherry-pick", "c"]);
    }
    let out = ok(&b, &["--human", "cherry-pick", "--empty=drop", "c"]);
    let c = git(&b, &["rev-parse", "c"]);
    assert!(
        out.contains(&format!(
            "dropping {} c -- patch contents already upstream",
            c.trim()
        )),
        "{out}"
    );

    let out = fails(&b, &["cherry-pick", "--strategy=ours", "a"]);
    assert!(out.contains("now empty"), "{out}");
    ok(&b, &["cherry-pick", "--abort"]);

    for d in [&a, &b] {
        git(
            d,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "--cleanup=verbatim",
                "-m",
                "s  \n\n\n# c",
            ],
        );
    }
    git(
        &a,
        &["cherry-pick", "--cleanup=strip", "--allow-empty", "HEAD"],
    );
    ok(
        &b,
        &["cherry-pick", "--cleanup=strip", "--allow-empty", "HEAD"],
    );
    assert_eq!(head(&a), head(&b));
    git(
        &a,
        &[
            "revert",
            "--no-edit",
            "--strategy=resolve",
            "--rerere-autoupdate",
            "c",
        ],
    );
    ok(
        &b,
        &["revert", "--strategy=resolve", "--rerere-autoupdate", "c"],
    );
    assert_eq!(result(&a, 1), result(&b, 1));
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
    assert!(log.contains("# first 'bad' commit"), "{log}");
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

/// Run bisect steps with git in one twin and rgit in the other (or git in
/// both for steps starting with `git`); after each, the report, the commit
/// under test and the bisect state must match.
fn bisect_twins(tag: &str, build: fn(&Path), steps: &[&[&str]]) {
    let (a, b) = twins(tag, build);
    for step in steps {
        let by_git = step.first() == Some(&"git");
        let args = if by_git { &step[1..] } else { step };
        let (want, git_ok) = git_try(&a, args);
        let (got, ok) = if by_git {
            git_try(&b, args)
        } else {
            rgit(&b, &[&["--human"], args].concat())
        };
        assert_eq!(git_ok, ok, "{step:?}\ngit: {want}\nrgit: {got}");
        // git 2.55+ quotes the bisect terms everywhere and prints custom
        // terms in status lines where older git hardcodes good/bad; drop
        // the status lines and the quotes, and compare the rest strictly.
        let mask = |s: &str| {
            s.replace('\'', "")
                .lines()
                .map(|l| {
                    if l.starts_with("status:") || l.starts_with("# status:") {
                        // old git hardcodes good/bad in status lines, even
                        // with custom terms, where 2.55+ prints the terms
                        "<status>"
                    } else if l.starts_with("bisect found first") {
                        // same hardcoding in the run-summary line
                        "<found first>"
                    } else {
                        l
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        if ok && !want.is_empty() {
            assert_eq!(mask(&want).trim_end(), mask(&got).trim_end(), "{step:?}");
        }
        for file in [
            "HEAD",
            "BISECT_HEAD",
            "BISECT_LOG",
            "BISECT_TERMS",
            "BISECT_NAMES",
        ] {
            let read = |d: &Path| std::fs::read_to_string(d.join(".git").join(file)).ok();
            let (left, right) = (read(&a), read(&b));
            assert_eq!(
                left.as_deref().map(mask).as_deref(),
                right.as_deref().map(mask).as_deref(),
                "{file} after {step:?}"
            );
        }
    }
}

/// main: n1..n20, each writing its number to n.
fn twenty(dir: &Path) {
    for i in 1..=20 {
        commit(dir, "n", &format!("{i}\n"), &format!("n{i}"));
    }
}

type Build = fn(&Path);

/// Two side branches merged into main; `dated` gives each commit its own
/// committer date, else they all share one.
fn branchy(dir: &Path, dated: bool) {
    let mut n = 0;
    let mut at = |path: &str, text: &str| {
        n += 1;
        std::fs::write(dir.join(path), text).unwrap();
        git(dir, &["add", path]);
        let date = if dated {
            format!("2020-01-01T{:02}:00:00Z", n % 24)
        } else {
            DATE.to_owned()
        };
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-qm", &format!("{path}{text}")])
            .env("GIT_AUTHOR_DATE", DATE)
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .unwrap();
        assert!(out.status.success());
    };
    for i in 1..=3 {
        at("n", &i.to_string());
    }
    git(dir, &["checkout", "-qb", "side", "HEAD~1"]);
    for i in 1..=4 {
        at("s", &i.to_string());
    }
    git(dir, &["checkout", "-q", "main"]);
    for i in 4..=6 {
        at("n", &i.to_string());
    }
    git(dir, &["merge", "-q", "--no-edit", "side"]);
    git(dir, &["checkout", "-qb", "other", "HEAD~2"]);
    for i in 1..=3 {
        at("o", &i.to_string());
    }
    git(dir, &["checkout", "-q", "main"]);
    at("n", "7");
    at("n", "8");
    git(dir, &["merge", "-q", "--no-edit", "other"]);
    at("n", "9");
    at("p", "1");
    at("n", "10");
}

#[test]
fn bisect_steps_and_skips_match_git() {
    bisect_twins(
        "bisect-linear",
        twenty,
        &[
            &["bisect", "start", "HEAD", "HEAD~19"],
            &["bisect", "skip"],
            &["bisect", "skip"],
            &["bisect", "good"],
            &["bisect", "skip", "HEAD~3..HEAD"],
            &["bisect", "bad"],
            &["bisect", "skip"],
            &["bisect", "good"],
            &["bisect", "bad"],
            &["bisect", "good"],
            &["bisect", "log"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn bisect_across_merges_matches_git() {
    let builds: [(&str, Build); 2] = [
        ("bisect-merges", |d| branchy(d, false)),
        ("bisect-dated", |d| branchy(d, true)),
    ];
    for (tag, build) in builds {
        bisect_twins(
            tag,
            build,
            &[
                &["bisect", "start", "HEAD", "side"],
                &["bisect", "good"],
                &["bisect", "bad"],
                &["bisect", "skip"],
                &["bisect", "good"],
                &["bisect", "reset"],
                &["bisect", "start", "--first-parent", "HEAD", "main~6"],
                &["bisect", "good"],
                &["bisect", "bad"],
                &["bisect", "reset"],
                &["bisect", "start", "HEAD", "main~6", "--", "s", "o"],
                &["bisect", "good"],
                &["bisect", "bad"],
                &["bisect", "reset"],
                // n6 is off side's history, so their merge base goes first.
                &["bisect", "start", "side", "main~7"],
                &["bisect", "good"],
                &["bisect", "bad"],
                &["bisect", "reset"],
                &["bisect", "start", "main~6", "main~7", "side"],
                &["bisect", "reset"],
                &["bisect", "start", "HEAD", "side", "other"],
                &["bisect", "skip"],
                &["bisect", "skip"],
                &["bisect", "skip"],
                &["bisect", "skip"],
                &["bisect", "skip"],
                &["bisect", "skip"],
                &["bisect", "skip"],
            ],
        );
    }
}

#[test]
fn bisect_terms_no_checkout_and_run_match_git() {
    bisect_twins(
        "bisect-run",
        twenty,
        &[
            &[
                "bisect",
                "start",
                "--term-new=fixed",
                "--term-old=broken",
                "--no-checkout",
            ],
            &["bisect", "fixed"],
            &["bisect", "broken", "HEAD~12"],
            &["bisect", "terms"],
            &["bisect", "bad"],
            &[
                "bisect",
                "run",
                "sh",
                "-c",
                "n=$(git cat-file -p BISECT_HEAD:n); test $n = 13 && exit 125; test $n -lt 15",
            ],
            &["bisect", "log"],
            &["bisect", "reset"],
            &["bisect", "start", "HEAD", "HEAD~19"],
            &["bisect", "run", "sh", "-c", "exit 129"],
            &["bisect", "run", "./missing-script"],
            &["bisect", "run", "sh", "-c", "test $(cat n) -lt 7"],
        ],
    );
}

#[test]
fn bisect_replays_and_hands_over_between_git_and_rgit() {
    bisect_twins(
        "bisect-interop",
        |d| branchy(d, false),
        &[
            &["git", "bisect", "start", "HEAD", "side"],
            &["bisect", "good"],
            &["git", "bisect", "bad"],
            &["bisect", "skip"],
            &["git", "bisect", "log"],
            &["git", "bisect", "reset"],
            &["bisect", "start", "HEAD", "side"],
            &["git", "bisect", "good"],
            &["bisect", "bad"],
            &["git", "bisect", "good"],
            &["bisect", "reset"],
        ],
    );
    let (a, b) = twins("bisect-replay", |d| branchy(d, false));
    for dir in [&a, &b] {
        git(dir, &["bisect", "start", "HEAD", "side"]);
        git(dir, &["bisect", "good"]);
        git(dir, &["bisect", "bad"]);
        std::fs::write(dir.join(".git/saved"), git(dir, &["bisect", "log"])).unwrap();
        git(dir, &["bisect", "reset"]);
    }
    let (want, _) = git_try(&a, &["bisect", "replay", ".git/saved"]);
    let got = ok(&b, &["--human", "bisect", "replay", ".git/saved"]);
    assert_eq!(want.trim_end(), got.trim_end());
    assert_eq!(head(&a), head(&b));
    let log = |d: &Path| std::fs::read_to_string(d.join(".git/BISECT_LOG")).unwrap();
    assert_eq!(log(&a), log(&b));
}

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, bool) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_EDITOR", "true")
        .envs(isolated())
        .env("GIT_SEQUENCE_EDITOR", "true")
        .envs(env.iter().copied())
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

fn rgit_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(
            ["--human", "--text", "--json", "--toon", "--axi"]
                .iter()
                .all(|m| !args.contains(m))
                .then_some("--toon"),
        )
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_EDITOR", "true")
        .envs(isolated())
        .envs(env.iter().copied())
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

/// Both repos on `branch`, then the same rebase in git and in rgit.
fn rebase_twins(tag: &str, build: fn(&Path), branch: &str, args: &[&str]) -> (PathBuf, PathBuf) {
    let (a, b) = twins(tag, build);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", branch]);
    }
    let mut argv = vec!["rebase"];
    argv.extend(args);
    git(&a, &argv);
    ok(&b, &argv);
    (a, b)
}

fn refs(dir: &Path) -> String {
    git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
        ],
    )
}

#[test]
fn rebase_matches_git_commit_for_commit() {
    let (a, b) = twins("rebase-plain", diverged);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "feat"]);
    }
    git(&a, &["rebase", "main"]);
    let out = ok(&b, &["--human", "rebase", "main"]);
    assert!(
        out.contains("Successfully rebased and updated refs/heads/feat."),
        "{out}"
    );
    assert_eq!(refs(&a), refs(&b));
    assert!(!b.join(".git/rebase-merge").exists());
    assert_eq!(
        git(&b, &["rev-parse", "ORIG_HEAD"]),
        git(&a, &["rev-parse", "ORIG_HEAD"])
    );
    let out = ok(&b, &["--human", "rebase", "main"]);
    assert!(out.contains("Current branch feat is up to date."), "{out}");
}

#[test]
fn rebase_onto_keep_base_and_root_match_git() {
    let (a, b) = rebase_twins(
        "rebase-onto",
        side_branch,
        "side",
        &["--onto", "main", "side~2"],
    );
    assert_eq!(refs(&a), refs(&b));
    let (a, b) = rebase_twins("rebase-root", side_branch, "side", &["-f", "--root"]);
    assert_eq!(refs(&a), refs(&b));
    let (a, b) = rebase_twins(
        "rebase-keep",
        diverged,
        "feat",
        &["-f", "--keep-base", "main"],
    );
    assert_eq!(refs(&a), refs(&b));
}

#[test]
fn rebase_conflict_continue_skip_abort_match_git() {
    let (a, b) = twins("rebase-conflict", conflicting);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "side"]);
    }
    let (_, done) = git_env(&a, &["rebase", "main"], &[]);
    assert!(!done);
    let out = fails(&b, &["--human", "rebase", "main"]);
    assert!(
        out.contains("CONFLICT (content): Merge conflict in f") && out.contains("Could not apply "),
        "{out}"
    );
    for f in [
        "head-name",
        "onto",
        "orig-head",
        "git-rebase-todo",
        "done",
        "msgnum",
        "end",
        "stopped-sha",
        "message",
        "author-script",
        "interactive",
    ] {
        assert!(b.join(".git/rebase-merge").join(f).exists(), "{f}");
    }
    assert!(state_file(&b, "REBASE_HEAD"));
    assert!(git(&b, &["status"]).contains("rebase in progress"));
    for d in [&a, &b] {
        std::fs::write(d.join("f"), "resolved\n").unwrap();
        git(d, &["add", "f"]);
    }
    git(&a, &["rebase", "--continue"]);
    ok(&b, &["rebase", "--continue"]);
    assert_eq!(refs(&a), refs(&b));
    assert_eq!(
        git(&b, &["log", "-1", "--format=%an", "HEAD~1"]).trim(),
        "sider"
    );

    // --skip drops the stopped commit.
    for d in [&a, &b] {
        git(d, &["reset", "-q", "--hard", "ORIG_HEAD"]);
    }
    git_env(&a, &["rebase", "main"], &[]);
    git(&a, &["rebase", "--skip"]);
    fails(&b, &["rebase", "main"]);
    ok(&b, &["rebase", "--skip"]);
    assert_eq!(refs(&a), refs(&b));

    // --abort goes back to the branch as it was.
    git(&b, &["reset", "-q", "--hard", "ORIG_HEAD"]);
    let tip = head(&b);
    fails(&b, &["rebase", "main"]);
    ok(&b, &["rebase", "--abort"]);
    assert_eq!(head(&b), tip);
    assert!(!b.join(".git/rebase-merge").exists());
    assert_eq!(git(&b, &["symbolic-ref", "HEAD"]).trim(), "refs/heads/side");
}

#[test]
fn rebase_state_is_shared_with_git() {
    let (a, b) = twins("rebase-cross", conflicting);
    let c = repo("rebase-cross-c");
    conflicting(&c);
    for d in [&a, &b, &c] {
        git(d, &["checkout", "-q", "side"]);
    }
    // git alone.
    git_env(&a, &["rebase", "main"], &[]);
    std::fs::write(a.join("f"), "resolved\n").unwrap();
    git(&a, &["add", "f"]);
    git(&a, &["rebase", "--continue"]);
    // rgit starts, git continues.
    fails(&b, &["rebase", "main"]);
    std::fs::write(b.join("f"), "resolved\n").unwrap();
    git(&b, &["add", "f"]);
    git(&b, &["rebase", "--continue"]);
    assert_eq!(refs(&a), refs(&b));
    // git starts, rgit continues.
    let (_, done) = git_env(&c, &["rebase", "main"], &[]);
    assert!(!done);
    assert!(ok(&c, &["--toon", "status"]).contains("rebasing"));
    std::fs::write(c.join("f"), "resolved\n").unwrap();
    git(&c, &["add", "f"]);
    ok(&c, &["rebase", "--continue"]);
    assert_eq!(refs(&a), refs(&c));
    // git aborts what rgit started.
    git(&b, &["reset", "-q", "--hard", "ORIG_HEAD"]);
    let tip = head(&b);
    fails(&b, &["rebase", "main"]);
    git(&b, &["rebase", "--abort"]);
    assert_eq!(head(&b), tip);
}

#[test]
fn interactive_rebase_with_a_scripted_editor_matches_git() {
    let (a, b) = twins("rebase-i", side_branch);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "side"]);
    }
    // s3 first, then s1 with s2 folded in.
    let script =
        r"perl -0pi -e 's/^(pick \S+ # s1\n)pick( \S+ # s2\n)(pick \S+ # s3\n)/$3$1fixup$2/m'";
    let env = [("GIT_SEQUENCE_EDITOR", script)];
    let (out, done) = git_env(&a, &["rebase", "-i", "main"], &env);
    assert!(done, "{out}");
    let (out, done) = rgit_env(&b, &["rebase", "-i", "main"], &env);
    assert!(done, "{out}");
    assert_eq!(refs(&a), refs(&b));
    assert_eq!(git(&b, &["log", "--format=%s", "main.."]), "s1\ns3\n");
}

#[test]
fn interactive_edit_reword_break_and_exec() {
    let dir = repo("rebase-edit");
    side_branch(&dir);
    git(&dir, &["checkout", "-q", "side"]);
    let script = "sed -i.bak -e '1s/^pick/edit/' -e '2s/^pick/reword/' -e '2a\\
break' -e '3a\\
exec touch exec-ran'";
    let msg = "sh -c 'echo reworded > \"$1\"' -";
    let env = [("GIT_SEQUENCE_EDITOR", script), ("GIT_EDITOR", msg)];
    let (out, done) = rgit_env(&dir, &["--human", "rebase", "-f", "-i", "main"], &env);
    assert!(done, "{out}");
    assert!(out.contains("Stopped at"), "{out}");
    assert!(state_file(&dir, "rebase-merge/amend"));
    std::fs::write(dir.join("s1"), "edited\n").unwrap();
    git(&dir, &["add", "s1"]);
    let (out, done) = rgit_env(&dir, &["--human", "rebase", "--continue"], &env);
    assert!(done && out.contains("Stopped at"), "{out}");
    let (out, done) = rgit_env(&dir, &["rebase", "--continue"], &env);
    assert!(done, "{out}");
    assert!(dir.join("exec-ran").exists());
    assert_eq!(
        git(&dir, &["log", "--format=%s", "main.."]),
        "s3\nreworded\ns1\n"
    );
    assert_eq!(git(&dir, &["show", "side~2:s1"]), "edited\n");
}

/// main: base; work: a, b, "fixup! a", "squash! a" with a body, "amend! b".
fn squashes(dir: &Path) {
    fixups(dir);
    commit(dir, "a", "a3\n", "squash! a\n\nmore about a");
    commit(dir, "b", "b2\n", "amend! b\n\nb, reworded\n\nwith a body");
}

#[test]
fn rebase_autosquash_squash_and_amend_match_git() {
    let (a, b) = twins("autosquash-all", squashes);
    git(&a, &["rebase", "-i", "--autosquash", "main"]);
    ok(&b, &["rebase", "--autosquash", "main"]);
    assert_eq!(result(&a, 2), result(&b, 2));
    assert_eq!(refs(&a), refs(&b));
}

#[test]
fn rebase_exec_and_update_refs_match_git() {
    let (a, b) = twins("exec-refs", fixups);
    for d in [&a, &b] {
        git(d, &["branch", "lower", "work~1"]);
        git(d, &["checkout", "-q", "main"]);
        commit(d, "m", "m\n", "m");
        git(d, &["checkout", "-q", "work"]);
    }
    git(&a, &["rebase", "--update-refs", "-x", "true", "main"]);
    let out = ok(
        &b,
        &["--human", "rebase", "--update-refs", "-x", "true", "main"],
    );
    assert!(out.contains("refs/heads/lower"), "{out}");
    assert_eq!(refs(&a), refs(&b));
}

/// main: base, m; side: s1, a merge of topic (t1), s2.
fn with_merge(dir: &Path) {
    commit(dir, "base", "base\n", "base");
    git(dir, &["checkout", "-qb", "side"]);
    commit(dir, "s1", "1\n", "s1");
    git(dir, &["checkout", "-qb", "topic"]);
    commit(dir, "t1", "1\n", "t1");
    git(dir, &["checkout", "-q", "side"]);
    git(
        dir,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge branch 'topic' into side",
            "topic",
        ],
    );
    commit(dir, "s2", "2\n", "s2");
    git(dir, &["checkout", "-q", "main"]);
    commit(dir, "m", "m\n", "m");
}

#[test]
fn rebase_merges_recreates_the_merge_like_git() {
    let (a, b) = rebase_twins("rebase-r", with_merge, "side", &["-r", "main"]);
    assert_eq!(refs(&a), refs(&b));
    assert_eq!(
        git(&b, &["rev-list", "--merges", "--count", "main..side"]).trim(),
        "1"
    );
    assert!(git(&b, &["for-each-ref", "refs/rewritten"]).is_empty());
    // Without -r the merge is flattened.
    let (a, b) = rebase_twins("rebase-flat", with_merge, "side", &["main"]);
    assert_eq!(refs(&a), refs(&b));
}

/// main: base, then x again; feat: x, y.
fn picked_upstream(dir: &Path) {
    commit(dir, "base", "base\n", "base");
    git(dir, &["checkout", "-qb", "feat"]);
    commit(dir, "x", "x\n", "x");
    commit(dir, "y", "y\n", "y");
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["cherry-pick", "feat~1"]);
}

#[test]
fn rebase_skips_commits_already_upstream_unless_asked() {
    let (a, b) = rebase_twins("rebase-cherry", picked_upstream, "feat", &["main"]);
    assert_eq!(refs(&a), refs(&b));
    assert_eq!(git(&b, &["log", "--format=%s", "main..feat"]), "y\n");
    let (a, b) = rebase_twins(
        "rebase-reapply",
        picked_upstream,
        "feat",
        &["--reapply-cherry-picks", "--empty=keep", "main"],
    );
    assert_eq!(refs(&a), refs(&b));
}

#[test]
fn rebase_strategy_option_autostash_and_signoff_match_git() {
    let (a, b) = rebase_twins("rebase-x", conflicting, "side", &["-X", "theirs", "main"]);
    assert_eq!(refs(&a), refs(&b));
    assert_eq!(git(&b, &["show", "side~1:f"]), "side\n");

    let (a, b) = twins("rebase-stash", diverged);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "feat"]);
        std::fs::write(d.join("b"), "dirty\n").unwrap();
    }
    let out = fails(&b, &["rebase", "main"]);
    assert!(out.contains("autostash"), "{out}");
    git(&a, &["rebase", "--autostash", "--signoff", "main"]);
    let out = ok(
        &b,
        &["--human", "rebase", "--autostash", "--signoff", "main"],
    );
    assert!(out.contains("Applied autostash."), "{out}");
    assert_eq!(refs(&a), refs(&b));
    assert_eq!(std::fs::read_to_string(b.join("b")).unwrap(), "dirty\n");
    assert!(git(&b, &["stash", "list"]).is_empty());
}

#[test]
fn rebase_runs_the_pre_rebase_and_post_rewrite_hooks() {
    let dir = repo("rebase-hooks");
    diverged(&dir);
    git(&dir, &["checkout", "-q", "feat"]);
    let hooks = dir.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = |name: &str, body: &str| {
        let path = hooks.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    };
    hook("pre-rebase", "exit 1");
    let out = fails(&dir, &["rebase", "main"]);
    assert!(out.contains("pre-rebase"), "{out}");
    hook("post-rewrite", "cat > .git/rewritten");
    ok(&dir, &["rebase", "--no-verify", "main"]);
    let rewritten = std::fs::read_to_string(dir.join(".git/rewritten")).unwrap();
    assert_eq!(rewritten.lines().count(), 2, "{rewritten}");
}

/// main: base; work: a (f=a), b (f=b), "fixup! a" (f=c), so the moved fixup
/// and then b conflict.
fn crossed_fixup(dir: &Path) {
    commit(dir, "base", "base\n", "base");
    git(dir, &["checkout", "-qb", "work"]);
    commit(dir, "f", "a\n", "a");
    commit(dir, "f", "b\n", "b");
    commit(dir, "f", "c\n", "fixup! a");
}

#[test]
fn autosquash_conflicts_continue_like_git() {
    let (a, b) = twins("fixup-conflict", crossed_fixup);
    let (_, done) = git_env(&a, &["rebase", "-i", "--autosquash", "main"], &[]);
    assert!(!done);
    fails(&b, &["rebase", "--autosquash", "main"]);
    for (text, last) in [("c\n", false), ("b\n", true)] {
        for d in [&a, &b] {
            std::fs::write(d.join("f"), text).unwrap();
            git(d, &["add", "f"]);
        }
        let (out, done) = git_env(&a, &["rebase", "--continue"], &[]);
        assert_eq!(done, last, "{out}");
        let (out, done) = rgit_env(&b, &["rebase", "--continue"], &[]);
        assert_eq!(done, last, "{out}");
    }
    assert_eq!(refs(&a), refs(&b));
    assert_eq!(git(&b, &["log", "--format=%s", "main.."]), "b\na\n");
}

#[test]
fn rebase_uses_the_fork_point_of_a_rewritten_upstream() {
    let build = |dir: &Path| {
        commit(dir, "base", "base\n", "base");
        git(dir, &["checkout", "-qb", "up"]);
        commit(dir, "u", "u\n", "u1");
        git(dir, &["checkout", "-qb", "feat"]);
        git(dir, &["config", "branch.feat.remote", "."]);
        git(dir, &["config", "branch.feat.merge", "refs/heads/up"]);
        commit(dir, "f", "f\n", "f1");
        git(dir, &["checkout", "-q", "up"]);
        git(dir, &["commit", "-q", "--amend", "-m", "u1 reworded"]);
        git(dir, &["checkout", "-q", "feat"]);
    };
    let (a, b) = twins("fork-point", build);
    git(&a, &["rebase"]);
    ok(&b, &["rebase"]);
    assert_eq!(refs(&a), refs(&b));
    assert_eq!(git(&b, &["log", "--format=%s", "up..feat"]), "f1\n");
}

#[test]
fn rebase_empty_stop_and_rebase_cousins() {
    let dir = repo("empty-stop");
    commit(&dir, "base", "base\n", "base");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "x", "x\n", "x");
    git(&dir, &["checkout", "-q", "main"]);
    commit(&dir, "x", "x\n", "x again");
    git(&dir, &["checkout", "-q", "feat"]);
    let out = fails(
        &dir,
        &["rebase", "--reapply-cherry-picks", "--empty=stop", "main"],
    );
    assert!(out.contains("now empty"), "{out}");
    ok(&dir, &["rebase", "--skip"]);
    assert_eq!(head(&dir), git(&dir, &["rev-parse", "main"]).trim());

    let (a, b) = rebase_twins(
        "cousins",
        with_merge,
        "side",
        &["--rebase-merges=rebase-cousins", "main"],
    );
    assert_eq!(refs(&a), refs(&b));
}

/// stdout and stderr apart, and success, of `git` (`tool` "git") or rgit.
fn run3(tool: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, String, bool) {
    let mut cmd = if tool == "git" {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir);
        c
    } else {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rgit"));
        c.arg("--human").current_dir(dir).env("RGIT_OPLOG", "0");
        c
    };
    let out = cmd
        .args(args)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .envs(isolated())
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.success())
}

/// Every rr-cache file with its content.
fn rr_cache(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let cache = std::fs::read_dir(dir.join(".git/rr-cache"));
    for e in cache.into_iter().flatten().flatten() {
        for f in std::fs::read_dir(e.path()).unwrap().flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(f.path()).unwrap_or_default();
            out.push(format!("{}/{name}:{text}", e.file_name().to_string_lossy()));
        }
    }
    out.sort();
    out
}

/// `conflicting` with rerere on.
fn rerere_repo(dir: &Path) {
    conflicting(dir);
    git(dir, &["config", "rerere.enabled", "true"]);
}

#[test]
fn rerere_records_and_replays_like_git_both_ways() {
    let (a, b) = twins("rerere", rerere_repo);
    let (_, gerr, _) = run3("git", &a, &["merge", "side"], &[]);
    let (_, rerr, done) = run3("rgit", &b, &["merge", "side"], &[]);
    assert!(!done);
    assert!(gerr.contains("Recorded preimage for 'f'"), "{gerr}");
    assert!(rerr.contains("Recorded preimage for 'f'"), "{rerr}");
    assert_eq!(rr_cache(&a), rr_cache(&b));
    assert_eq!(
        std::fs::read(a.join(".git/MERGE_RR")).unwrap(),
        std::fs::read(b.join(".git/MERGE_RR")).unwrap()
    );
    for sub in ["status", "remaining", "diff"] {
        let g = run3("git", &a, &["rerere", sub], &[]);
        let r = run3("rgit", &b, &["rerere", sub], &[]);
        assert_eq!(g.0, r.0, "rerere {sub}");
    }
    for d in [&a, &b] {
        std::fs::write(d.join("f"), "resolved\n").unwrap();
    }
    assert_eq!(
        run3("git", &a, &["rerere", "diff"], &[]).0,
        run3("rgit", &b, &["rerere", "diff"], &[]).0
    );
    for d in [&a, &b] {
        git(d, &["add", "f"]);
    }
    let (_, gerr, _) = run3("git", &a, &["commit", "--no-edit"], &[]);
    let (_, rerr, _) = run3("rgit", &b, &["commit", "-m", "merge"], &[]);
    assert!(gerr.contains("Recorded resolution for 'f'."), "{gerr}");
    assert!(rerr.contains("Recorded resolution for 'f'."), "{rerr}");
    assert_eq!(rr_cache(&a), rr_cache(&b));

    // Each replays what the other recorded: swap the caches.
    let cache = |d: &Path| d.join(".git/rr-cache");
    std::fs::rename(cache(&a), a.join("cache")).unwrap();
    std::fs::rename(cache(&b), cache(&a)).unwrap();
    std::fs::rename(a.join("cache"), cache(&b)).unwrap();
    for d in [&a, &b] {
        git(d, &["reset", "-q", "--hard", "HEAD~1"]);
    }
    let (_, gerr, _) = run3("git", &a, &["merge", "side"], &[]);
    let (_, rerr, _) = run3("rgit", &b, &["merge", "side"], &[]);
    let replayed = "Resolved 'f' using previous resolution.";
    assert!(gerr.contains(replayed), "{gerr}");
    assert!(rerr.contains(replayed), "{rerr}");
    for d in [&a, &b] {
        assert_eq!(std::fs::read_to_string(d.join("f")).unwrap(), "resolved\n");
        assert!(git(d, &["status", "--short"]).contains("UU f"));
    }

    // forget, clear, abort and gc leave what git leaves.
    let g = run3("git", &a, &["rerere", "forget", "f"], &[]);
    let r = run3("rgit", &b, &["rerere", "forget", "f"], &[]);
    assert_eq!(g.1, r.1);
    assert_eq!(rr_cache(&a), rr_cache(&b));
    for args in [&["rerere", "clear"][..], &["merge", "--abort"]] {
        run3("git", &a, args, &[]);
        run3("rgit", &b, args, &[]);
        assert_eq!(rr_cache(&a), rr_cache(&b), "{args:?}");
    }
    for d in [&a, &b] {
        git(d, &["config", "gc.rerereResolved", "-1"]);
    }
    run3("git", &a, &["rerere", "gc"], &[]);
    run3("rgit", &b, &["rerere", "gc"], &[]);
    assert_eq!(rr_cache(&a), rr_cache(&b));
}

#[test]
fn rerere_autoupdate_and_every_sequencer_hook() {
    let (a, b) = twins("rerere-auto", rerere_repo);
    for d in [&a, &b] {
        git_try(d, &["merge", "side"]);
        std::fs::write(d.join("f"), "resolved\n").unwrap();
        git(d, &["add", "f"]);
        git(d, &["commit", "-q", "--no-edit"]);
        git(d, &["reset", "-q", "--hard", "HEAD~1"]);
    }
    let args = ["cherry-pick", "--rerere-autoupdate", "side~1"];
    let (_, gerr, _) = run3("git", &a, &args, &[]);
    let (_, rerr, _) = run3("rgit", &b, &args, &[]);
    let staged = "Staged 'f' using previous resolution.";
    assert!(gerr.contains(staged), "{gerr}");
    assert!(rerr.contains(staged), "{rerr}");
    for d in [&a, &b] {
        assert_eq!(git(d, &["diff", "--name-only", "--diff-filter=U"]), "");
        assert_eq!(std::fs::read_to_string(d.join("f")).unwrap(), "resolved\n");
    }
    run3("git", &a, &["cherry-pick", "--abort"], &[]);
    run3("rgit", &b, &["cherry-pick", "--abort"], &[]);

    // A rebase step and a stash apply go through rerere too.
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "side~1"]);
    }
    let (_, gerr, _) = run3("git", &a, &["rebase", "main"], &[]);
    let (_, rerr, _) = run3("rgit", &b, &["rebase", "main"], &[]);
    let replayed = "Resolved 'f' using previous resolution.";
    assert!(gerr.contains(replayed), "{gerr}");
    assert!(rerr.contains(replayed), "{rerr}");
    run3("git", &a, &["rebase", "--abort"], &[]);
    run3("rgit", &b, &["rebase", "--abort"], &[]);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "main"]);
        std::fs::write(d.join("f"), "side\n").unwrap();
        git(d, &["stash", "-q"]);
        commit(d, "f", "other\n", "other");
    }
    let (_, gerr, _) = run3("git", &a, &["stash", "apply"], &[]);
    let (_, rerr, done) = run3("rgit", &b, &["stash", "apply"], &[]);
    assert!(!done);
    assert!(gerr.contains("Recorded preimage for 'f'"), "{gerr}");
    assert!(rerr.contains("Recorded preimage for 'f'"), "{rerr}");
    assert_eq!(rr_cache(&a), rr_cache(&b));
    assert_eq!(git(&b, &["stash", "list"]).lines().count(), 1);
}

/// Two merge bases: main and side each merge the other's first commit.
fn criss_cross(dir: &Path) {
    commit(dir, "f", "1\n2\n3\n4\n5\n", "base");
    git(dir, &["checkout", "-qb", "side"]);
    commit(dir, "f", "1\nS\n3\n4\n5\n", "s1");
    git(dir, &["checkout", "-q", "main"]);
    commit(dir, "f", "1\n2\n3\n4\nM\n", "m1");
    git(dir, &["branch", "m1"]);
    git(dir, &["merge", "-q", "--no-edit", "side"]);
    commit(dir, "f", "1\nS\n3\nX\nM\n", "m2");
    git(dir, &["checkout", "-q", "side"]);
    git(dir, &["merge", "-q", "--no-edit", "m1"]);
    commit(dir, "g", "g\n", "s2");
    git(dir, &["checkout", "-q", "main"]);
}

#[test]
fn merges_with_several_bases_use_a_virtual_base_like_git() {
    let (a, b) = twins("criss-cross", criss_cross);
    let bases = git(&a, &["merge-base", "--all", "main", "side"]);
    assert_eq!(bases.lines().count(), 2);
    git(&a, &["merge", "--no-edit", "side"]);
    ok(&b, &["merge", "side"]);
    assert_eq!(result(&a, 1), result(&b, 1));
}

fn sorted_lines(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s.lines().map(str::to_owned).collect();
    v.sort();
    v
}

#[test]
fn conflict_reports_match_git_word_for_word() {
    for (tag, args) in [
        ("say-pick", &["cherry-pick", "side~1"][..]),
        ("say-revert", &["revert", "--no-edit", "main~1"]),
        ("say-merge", &["merge", "side"]),
    ] {
        let (a, b) = twins(tag, conflicting);
        let (gout, gerr, _) = run3("git", &a, args, &[]);
        let (rout, rerr, done) = run3("rgit", &b, args, &[]);
        assert!(!done, "{tag}: {rout}{rerr}");
        // rgit adds its own closing line and hint on a merge stop.
        let mut rgit_lines = sorted_lines(&(rout + &rerr));
        rgit_lines.retain(|l| !l.contains("rgit"));
        assert_eq!(sorted_lines(&(gout + &gerr)), rgit_lines, "{tag}");
        for file in ["f", ".git/MERGE_MSG"] {
            assert_eq!(
                std::fs::read_to_string(a.join(file)).unwrap(),
                std::fs::read_to_string(b.join(file)).unwrap(),
                "{tag} {file}"
            );
        }
    }
    let (a, b) = twins("say-rebase", conflicting);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "side"]);
    }
    let (gout, gerr, _) = run3("git", &a, &["rebase", "main"], &[]);
    let (rout, rerr, _) = run3("rgit", &b, &["rebase", "main"], &[]);
    let git_text = (gout + &gerr).replace("Rebasing (1/2)\r", "");
    assert_eq!(sorted_lines(&git_text), sorted_lines(&(rout + &rerr)));
}

/// A sequence editor that copies the todo it is given to `out`.
fn todo_copy(out: &Path) -> String {
    format!("f() {{ cp \"$1\" '{}'; }}; f", out.display())
}

#[test]
fn rebase_todo_text_matches_git_byte_for_byte() {
    for config in [
        &[][..],
        &[("rebase.abbreviateCommands", "true")],
        &[("rebase.instructionFormat", "%s (%an <%ae>) %h")],
        &[("rebase.missingCommitsCheck", "error")],
    ] {
        let (a, b) = twins("todo-text", side_branch);
        for d in [&a, &b] {
            git(d, &["checkout", "-q", "side"]);
            for (k, v) in config {
                git(d, &["config", k, v]);
            }
        }
        let (ga, rb) = (a.join("todo"), b.join("todo"));
        let args = ["rebase", "-i", "--exec", "true", "main"];
        run3(
            "git",
            &a,
            &args,
            &[("GIT_SEQUENCE_EDITOR", &todo_copy(&ga))],
        );
        run3(
            "rgit",
            &b,
            &args,
            &[("GIT_SEQUENCE_EDITOR", &todo_copy(&rb))],
        );
        assert_eq!(
            std::fs::read_to_string(&ga).unwrap(),
            std::fs::read_to_string(&rb).unwrap(),
            "{config:?}"
        );
        assert_eq!(refs(&a), refs(&b));
    }
    let (a, b) = twins("todo-root", side_branch);
    for d in [&a, &b] {
        git(d, &["checkout", "-q", "side"]);
    }
    let (ga, rb) = (a.join("todo"), b.join("todo"));
    let args = ["rebase", "-i", "--root"];
    run3(
        "git",
        &a,
        &args,
        &[("GIT_SEQUENCE_EDITOR", &todo_copy(&ga))],
    );
    run3(
        "rgit",
        &b,
        &args,
        &[("GIT_SEQUENCE_EDITOR", &todo_copy(&rb))],
    );
    assert_eq!(
        std::fs::read_to_string(&ga).unwrap(),
        std::fs::read_to_string(&rb).unwrap()
    );
}

#[test]
fn missing_commits_check_warns_stops_and_resumes_like_git() {
    let drop_s2 = [("GIT_SEQUENCE_EDITOR", "sed -i.bak -e /s2/d")];
    for level in ["warn", "error"] {
        let (a, b) = twins(&format!("missing-{level}"), side_branch);
        for d in [&a, &b] {
            git(d, &["checkout", "-q", "side"]);
            git(d, &["config", "rebase.missingCommitsCheck", level]);
        }
        let g = run3("git", &a, &["rebase", "-i", "main"], &drop_s2);
        let r = run3("rgit", &b, &["rebase", "-i", "main"], &drop_s2);
        assert_eq!(g.2, r.2, "{level}");
        let warning = &g.1[g.1.find("Warning").unwrap()..];
        let warning = &warning[..warning.find("rebase --abort'.\n").unwrap() + 17];
        assert!(r.1.contains(warning), "{}\n{}", g.1, r.1);
        if level == "warn" {
            assert_eq!(refs(&a), refs(&b));
            continue;
        }
        assert!(b.join(".git/rebase-merge/dropped").exists());
        // Still missing: --continue refuses, as git's.
        assert!(!run3("git", &a, &["rebase", "--continue"], &[]).2);
        assert!(!run3("rgit", &b, &["rebase", "--continue"], &[]).2);
        // --edit-todo shows git's text; dropping it explicitly lets it go on.
        let (ga, rb) = (a.join("todo"), b.join("todo"));
        let args = ["rebase", "--edit-todo"];
        run3(
            "git",
            &a,
            &args,
            &[("GIT_SEQUENCE_EDITOR", &todo_copy(&ga))],
        );
        run3(
            "rgit",
            &b,
            &args,
            &[("GIT_SEQUENCE_EDITOR", &todo_copy(&rb))],
        );
        assert_eq!(
            std::fs::read_to_string(&ga).unwrap(),
            std::fs::read_to_string(&rb).unwrap()
        );
        let s2 = git(&a, &["rev-parse", "--short", "side~1"]);
        let add_drop = format!("f() {{ echo 'drop {}' >> \"$1\"; }}; f", s2.trim());
        let env = [("GIT_SEQUENCE_EDITOR", add_drop.as_str())];
        assert!(run3("git", &a, &args, &env).2);
        assert!(run3("rgit", &b, &args, &env).2);
        assert!(run3("git", &a, &["rebase", "--continue"], &[]).2);
        assert!(run3("rgit", &b, &["rebase", "--continue"], &[]).2);
        assert_eq!(refs(&a), refs(&b));
    }
}

#[test]
fn squash_chain_editor_opens_after_continue_like_git() {
    let (a, b) = twins("squash-edit", |d| {
        commit(d, "base", "base\n", "base");
        git(d, &["checkout", "-qb", "work"]);
        commit(d, "f", "a\n", "a");
        commit(d, "f", "b\n", "squash! a");
        git(d, &["checkout", "-q", "main"]);
        commit(d, "f", "main\n", "m");
        git(d, &["checkout", "-q", "work"]);
    });
    let edit = [("GIT_EDITOR", "f() { printf 'edited\\n' > \"$1\"; }; f")];
    let args = ["rebase", "-i", "--autosquash", "main"];
    assert!(!run3("git", &a, &args, &edit).2);
    assert!(!run3("rgit", &b, &args, &edit).2);
    for text in ["a\n", "b\n"] {
        for d in [&a, &b] {
            std::fs::write(d.join("f"), text).unwrap();
            git(d, &["add", "f"]);
        }
        let g = run3("git", &a, &["rebase", "--continue"], &edit);
        let r = run3("rgit", &b, &["rebase", "--continue"], &edit);
        assert_eq!(g.2, r.2, "{}{}\n{}{}", g.0, g.1, r.0, r.1);
    }
    assert_eq!(git(&a, &["log", "-1", "--format=%B"]), "edited\n\n");
    assert_eq!(refs(&a), refs(&b));
}

#[test]
fn am_reads_hg_stgit_series_and_maildirs_like_git() {
    let (a, b) = twins("am-formats", |d| commit(d, "f", "a\n", "base"));
    let patch =
        |line: &str| format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1,2 @@\n a\n+{line}\n");
    let hg = format!(
        "# HG changeset patch\n# User Foo Bar <foo@bar.org>\n# Date 1577836800 -3600\n#      \
         Wed Jan 01 01:00:00 2020 +0100\n# Node ID 0123\nhg subject\n\nhg body\n\n{}",
        patch("hg")
    );
    let stg = format!(
        "stgit subject\n\nAuthor: Stg Er <s@t.g>\nDate: Wed, 1 Jan 2020 00:00:00 +0000\n\nstgit \
         body\n---\n\n{}",
        patch("stg")
    );
    for d in [&a, &b] {
        std::fs::write(d.join("hg.patch"), &hg).unwrap();
        std::fs::write(d.join("st.patch"), &stg).unwrap();
        std::fs::create_dir_all(d.join("series-dir")).unwrap();
        std::fs::write(d.join("series-dir/st.patch"), &stg).unwrap();
        let series = "# This series applies on GIT commit 0000\nst.patch\n";
        std::fs::write(d.join("series-dir/series"), series).unwrap();
    }
    let log = |d: &Path| git(d, &["log", "--format=%an|%ae|%ad|%s|%b|%T"]);
    for args in [
        &["am", "hg.patch"][..],
        &["am", "st.patch"],
        &["am", "series-dir/series"],
    ] {
        let g = run3("git", &a, args, &[]);
        let r = run3("rgit", &b, args, &[]);
        assert!(g.2 && r.2, "{args:?}: {}{}", r.0, r.1);
        assert_eq!(g.0, r.0, "{args:?}");
        assert_eq!(log(&a), log(&b), "{args:?}");
        for d in [&a, &b] {
            git(d, &["reset", "-q", "--hard", "HEAD~1"]);
        }
    }

    // A maildir's mails go in git's order: digit runs by value, dot files out.
    for d in [&a, &b] {
        std::fs::create_dir_all(d.join("md/cur")).unwrap();
        std::fs::create_dir_all(d.join("md/new")).unwrap();
        for name in ["2", "10", "1:2,S"] {
            let file = name.replace([':', ','], "");
            let mail = format!(
                "From: M <m@m>\nDate: Wed, 1 Jan 2020 00:00:00 +0000\nSubject: [PATCH] mail \
                 {name}\n\n---\ndiff --git a/{file} \
                 b/{file}\nnew file mode 100644\n--- /dev/null\n+++ b/{file}\n@@ -0,0 +1 @@\n+x\n"
            );
            std::fs::write(d.join("md/cur").join(name), mail).unwrap();
        }
        std::fs::write(d.join("md/new/3"), "junk").unwrap();
        std::fs::write(d.join("md/cur/.hidden"), "junk").unwrap();
        std::fs::remove_file(d.join("md/new/3")).unwrap();
    }
    let g = run3("git", &a, &["am", "md"], &[]);
    let r = run3("rgit", &b, &["am", "md"], &[]);
    assert!(g.2 && r.2, "{}{}", r.0, r.1);
    assert_eq!(g.0, r.0);
    assert_eq!(log(&a), log(&b));
}

#[test]
fn am_three_way_reports_like_git_and_mailinfo_keeps_the_inbody_subject() {
    let (a, b) = twins("am-3way", |d| {
        commit(d, "f", "a\n", "base");
        git(d, &["checkout", "-qb", "side"]);
        commit(d, "f", "a\nSIDE\n", "side");
        git(d, &["format-patch", "-q", "-1", "-o", "p"]);
        git(d, &["checkout", "-q", "main"]);
        commit(d, "f", "a\nMAIN\n", "main");
    });
    let args = ["am", "-3", "p/0001-side.patch"];
    let g = run3("git", &a, &args, &[]);
    let r = run3("rgit", &b, &args, &[]);
    assert!(!g.2 && !r.2);
    let lines = |s: String| {
        let mut v: Vec<String> = s.lines().map(str::to_owned).collect();
        v.sort();
        v
    };
    assert_eq!(lines(g.0 + &g.1), lines(r.0 + &r.1));
    assert_eq!(
        std::fs::read_to_string(a.join("f")).unwrap(),
        std::fs::read_to_string(b.join("f")).unwrap()
    );

    let mail = "From: A <a@b>\nSubject: [PATCH] outer\n\nSubject: inner\nFrom: B <b@c>\n\n\
                body\n---\ndiff --git a/f b/f\n";
    std::fs::write(a.join("mail"), mail).unwrap();
    let info = |program: &str, args: &[&str]| {
        let out = Command::new(program)
            .args(args)
            .current_dir(&a)
            .stdin(std::fs::File::open(a.join("mail")).unwrap())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let git_info = info("git", &["mailinfo", "-k", "msg", "patch"]);
    assert!(
        git_info.contains("Subject: inner\nSubject: \n"),
        "{git_info}"
    );
    let rgit_info = info(
        env!("CARGO_BIN_EXE_rgit"),
        &["--human", "mailinfo", "-k", "msg", "patch"],
    );
    assert_eq!(git_info, rgit_info);
}

/// An octopus merge follows git's git-merge-octopus.sh: the same lines, exit
/// code and resulting tree, including a criss-cross history and a strategy
/// failure that git undoes.
#[test]
fn octopus_merges_like_gits_script() {
    fn build(dir: &Path, case: &str) {
        let sh = |script: &str| {
            let ok = Command::new("sh")
                .arg("-c")
                .arg(script)
                .current_dir(dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
                .status()
                .unwrap()
                .success();
            assert!(ok, "{script}");
        };
        sh("printf '1\\n2\\n3\\n' > f && git add f && git commit -qm base");
        sh(match case {
            "clean" => {
                "for b in a b c; do git checkout -qb $b main && echo $b > $b.txt && git add \
                 $b.txt && git commit -qm $b; done; git checkout -q main"
            }
            "last" => {
                "git checkout -qb a main && echo a > a.txt && git add a.txt && git commit -qm a \
                 && git checkout -qb b main && printf '1\\nB\\n3\\n' > f && git commit -qam b \
                 && git checkout -qb c main && printf '1\\nC\\n3\\n' > f && git commit -qam c \
                 && git checkout -q main"
            }
            "middle" => {
                "git checkout -qb a main && printf '1\\nA\\n3\\n' > f && git commit -qam a \
                 && git checkout -qb b main && printf '1\\nB\\n3\\n' > f && git commit -qam b \
                 && git checkout -qb c main && echo c > c.txt && git add c.txt && git commit -qm \
                 c && git checkout -q main"
            }
            _ => {
                "git checkout -qb a main && printf '1\\n2\\n3\\nx\\n' > f && git commit -qam x1 \
                 && git checkout -qb b main && printf 'y\\n1\\n2\\n3\\n' > f && git commit -qam \
                 y1 && git checkout -q a && git merge -q --no-edit b && git checkout -q b && git \
                 merge -q --no-edit a~1 && printf 'y\\n1\\nY\\n3\\n' > f && git commit -qam y2 \
                 && git checkout -q a && printf '1\\nX\\n3\\nx\\n' > f && git commit -qam x2 && \
                 git checkout -qb c main && echo z > z.txt && git add z.txt && git commit -qm z \
                 && git checkout -q main"
            }
        });
    }
    let run = |dir: &Path, bin: &str, extra: &[&str]| {
        let out = Command::new(bin)
            .args(extra)
            .args(["merge", "a", "b", "c"])
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
            .env("GIT_EDITOR", "true")
            .env("RGIT_OPLOG", "0")
            .output()
            .unwrap();
        let file = std::fs::read_to_string(dir.join("f")).unwrap();
        // git names the conflict sides after random temp files.
        let file: String = file
            .lines()
            .map(|l| l.split(".merge_file_").next().unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        (
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            out.status.code(),
            git(dir, &["status", "--porcelain"]),
            file,
            git(dir, &["log", "-1", "--format=%T %P"]),
        )
    };
    for case in ["clean", "last", "middle", "criss-cross"] {
        let (g, r) = (
            repo(&format!("oct-{case}-git")),
            repo(&format!("oct-{case}-rgit")),
        );
        build(&g, case);
        build(&r, case);
        assert_eq!(
            run(&r, env!("CARGO_BIN_EXE_rgit"), &["--human"]),
            run(&g, "git", &[]),
            "{case}"
        );
    }
}
