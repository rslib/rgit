//! `log`, `diff`, `show`, `blame` and `describe` take git's forms and give
//! git's answers.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git_at(dir: &Path, args: &[&str], day: u32) -> String {
    let date = format!("2024-01-{day:02}T12:00:00+0000");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_at(dir, args, 1)
}

fn rgit(dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("--human")
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    (text, out.status.success())
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, success) = rgit(dir, args);
    assert!(success, "rgit {args:?}: {out}");
    out
}

fn commit(dir: &Path, day: u32, files: &[(&str, &str)], msg: &str) {
    for (p, text) in files {
        std::fs::write(dir.join(p), text).unwrap();
    }
    git_at(dir, &["add", "-A"], day);
    git_at(dir, &["commit", "-qm", msg], day);
}

const A: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";

/// main: init (tag v1) - fix: bug one - [side merged] - Fix bug two - merge -
/// rename a to b - edit b. `topic` branches off after "fix: bug one".
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-history-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dir")).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    commit(&dir, 1, &[("a.txt", A), ("dir/x", "x\n")], "init");
    git_at(&dir, &["tag", "-a", "v1", "-m", "v1"], 1);
    commit(
        &dir,
        2,
        &[("a.txt", &A.replace("2\n", "two\n"))],
        "fix: bug one",
    );
    git(&dir, &["branch", "side"]);
    git(&dir, &["branch", "topic"]);
    git(&dir, &["checkout", "-q", "side"]);
    commit(&dir, 3, &[("dir/y", "y\n")], "side work");
    git(&dir, &["checkout", "-q", "topic"]);
    commit(&dir, 4, &[("t.txt", "t\n")], "topic change");
    git(&dir, &["checkout", "-q", "main"]);
    commit(
        &dir,
        5,
        &[("a.txt", &A.replace("2\n", "two\n").replace("5\n", "five\n"))],
        "Fix bug two",
    );
    git_at(
        &dir,
        &["merge", "-q", "--no-ff", "side", "-m", "Merge side"],
        6,
    );
    git_at(&dir, &["mv", "a.txt", "b.txt"], 7);
    git_at(&dir, &["commit", "-qm", "rename a to b"], 7);
    let b = std::fs::read_to_string(dir.join("b.txt")).unwrap();
    commit(&dir, 8, &[("b.txt", &b.replace("9\n", "nine\n"))], "edit b");
    dir
}

fn ids(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn log_takes_ranges_revs_paths_and_filters() {
    let dir = repo("log");
    let cases: &[&[&str]] = &[
        &[],
        &["v1..main"],
        &["main..topic"],
        &["topic..main"],
        &["main...topic"],
        &["topic", "^v1"],
        &["topic", "side"],
        &["--first-parent"],
        &["--merges"],
        &["--no-merges"],
        &["--reverse"],
        &["-n", "2", "--reverse"],
        &["--skip", "2", "-n", "2"],
        &[
            "--after",
            "2024-01-05T00:00:00",
            "--before",
            "2024-01-07T00:00:00",
        ],
        &["--grep", "fix"],
        &["--grep", "fix", "-i"],
        &["--grep", "^side", "--grep", "rename"],
        &["--", "dir"],
        &["--", "dir/y", "b.txt"],
        &["dir"],
        &["topic", "--", "t.txt"],
        &["--follow", "--", "b.txt"],
        &["--", "b.txt"],
        &["."],
    ];
    for args in cases {
        let mut want = vec!["log", "--format=%h"];
        want.extend_from_slice(args);
        let mut got = vec!["log"];
        got.extend_from_slice(args);
        assert_eq!(
            ids(&ok(&dir, &got)),
            ids(&git(&dir, &want)),
            "rgit log {args:?}"
        );
    }
    assert_eq!(
        ids(&ok(&dir.join("dir"), &["log", "x"])),
        ids(&git(&dir, &["log", "--format=%h", "--", "dir/x"]))
    );
    let (out, success) = rgit(&dir, &["log", "nope"]);
    assert!(!success, "{out}");
}

#[test]
fn log_shows_each_commits_changes() {
    let dir = repo("log-patch");
    let out = ok(&dir, &["log", "-n", "1", "-p"]);
    assert!(out.contains("-9\n+nine"), "{out}");
    let out = ok(&dir, &["log", "-n", "2", "--stat"]);
    assert!(
        out.contains("+1 -1 b.txt") && out.contains("+0 -0 b.txt"),
        "{out}"
    );
    let out = ok(&dir, &["log", "--name-only", "--", "dir"]);
    assert!(out.contains("dir/y") && !out.contains("a.txt"), "{out}");
    let out = ok(&dir, &["log", "--merges", "-p"]);
    assert_eq!(out.lines().count(), 1, "{out}");
}

fn hunks(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter(|l| !l.starts_with("+++ ") && !l.starts_with("--- "))
        .filter_map(|l| {
            if l.starts_with("@@") {
                l.split("@@").nth(1).map(str::to_owned)
            } else if l.starts_with([' ', '+', '-']) {
                Some(l.to_owned())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn diff_takes_ranges_paths_and_formats() {
    let dir = repo("diff");
    std::fs::write(dir.join("dir/x"), "x2\n").unwrap();
    std::fs::write(dir.join("dir/y"), "y2\n").unwrap();
    git(&dir, &["add", "dir/y"]);
    let cases: &[&[&str]] = &[
        &[],
        &["--cached"],
        &["v1", "main"],
        &["v1..main"],
        &["topic...main"],
        &["main...topic"],
        &["v1"],
        &["v1", "--cached"],
        &["--", "dir/x"],
        &["dir"],
        &["v1", "HEAD", "--", "dir"],
        &["--cached", "--", "dir"],
        &["--", "."],
    ];
    for args in cases {
        let mut want = vec!["diff", "--name-only"];
        want.extend_from_slice(args);
        let mut got = vec!["diff", "--name-only"];
        got.extend_from_slice(args);
        assert_eq!(
            ok(&dir, &got).trim(),
            git(&dir, &want).trim(),
            "rgit diff {args:?}"
        );
    }
    for args in [
        &["v1", "topic", "--name-status"][..],
        &["HEAD~2", "HEAD~1", "--name-status"],
        &["v1", "topic", "--numstat"],
    ] {
        let mut want = vec!["diff"];
        want.extend_from_slice(args);
        let mut got = vec!["diff"];
        got.extend_from_slice(args);
        assert_eq!(ok(&dir, &got).trim(), git(&dir, &want).trim(), "{args:?}");
    }

    let b = std::fs::read_to_string(dir.join("b.txt")).unwrap();
    std::fs::write(
        dir.join("b.txt"),
        b.replace("3\n", "3  \n").replace("7\n", "seven\n"),
    )
    .unwrap();
    for args in [
        &["--patch"][..],
        &["--patch", "-U1"],
        &["--patch", "-w"],
        &["--patch", "-U0", "-w"],
        &["--patch", "-b", "--", "b.txt"],
    ] {
        let mut got = vec!["diff"];
        got.extend_from_slice(args);
        let want: Vec<&str> = args.iter().copied().filter(|a| *a != "--patch").collect();
        let mut want_args = vec!["diff"];
        want_args.extend_from_slice(&want);
        assert_eq!(
            hunks(&ok(&dir, &got)),
            hunks(&git(&dir, &want_args)),
            "{args:?}"
        );
    }
}

#[test]
fn show_prints_files_trees_and_several_commits() {
    let dir = repo("show");
    for spec in ["v1:a.txt", "HEAD:b.txt", "HEAD:dir", "HEAD:"] {
        assert_eq!(
            ok(&dir, &["show", spec]).trim(),
            git(&dir, &["show", spec]).trim(),
            "{spec}"
        );
    }
    std::fs::write(dir.join("b.txt"), "staged\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    assert_eq!(ok(&dir, &["show", ":b.txt"]).trim(), "staged");

    let out = ok(&dir, &["show", "HEAD~1", "HEAD", "--name-only"]);
    assert_eq!(out.trim(), "b.txt\n\nb.txt", "{out}");
    let out = ok(&dir, &["show", "--stat"]);
    assert!(out.contains("edit b") && out.contains("b.txt"), "{out}");
    let out = ok(&dir, &["show", "-s"]);
    assert!(out.contains("edit b") && !out.contains("b.txt"), "{out}");
    let out = ok(&dir, &["show", "side", "--name-only", "--", "nope"]);
    assert_eq!(out.trim(), "", "{out}");
}

#[test]
fn blame_takes_a_revision() {
    let dir = repo("blame");
    let v1 = git(&dir, &["rev-parse", "--short=7", "v1^{commit}"]);
    for args in [
        &["blame", "v1", "--", "a.txt"][..],
        &["blame", "v1", "a.txt"],
    ] {
        let out = ok(&dir, args);
        assert_eq!(out.lines().count(), 10, "{out}");
        assert!(out.lines().all(|l| l.starts_with(v1.trim())), "{out}");
    }
    let out = ok(&dir, &["blame", "b.txt", "-L", "8,+3"]);
    let texts: Vec<&str> = out.lines().map(|l| l.rsplit(' ').next().unwrap()).collect();
    assert_eq!(texts, ["8", "nine", "10"]);
    let at_topic = ok(&dir, &["blame", "topic", "--", "a.txt"]);
    assert!(at_topic.contains("two") && !at_topic.contains("five"));
}

#[test]
fn describe_takes_git_flags() {
    let dir = repo("describe");
    for args in [
        &["describe", "--always"][..],
        &["describe", "--tags", "--abbrev=0"],
        &["describe", "--match", "v*"],
        &["describe", "--exact-match", "v1"],
    ] {
        assert_eq!(ok(&dir, args).trim(), git(&dir, args).trim(), "{args:?}");
    }
    let (out, success) = rgit(&dir, &["describe", "--exact-match"]);
    assert!(!success, "{out}");
}
