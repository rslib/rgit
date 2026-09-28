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
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Assert `rgit --human <args>` prints git's bytes and exits with git's code.
fn same(dir: &Path, args: &[&str]) {
    let run = |cmd: &mut Command| {
        let out = cmd
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("RGIT_OPLOG", "0")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status.code(),
        )
    };
    let want = run(&mut Command::new("git"));
    let got = run(Command::new(env!("CARGO_BIN_EXE_rgit")).arg("--human"));
    assert_eq!(got, want, "rgit {args:?}");
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
fn log_and_show_print_git_formats() {
    let dir = repo("log-fmt");
    std::fs::write(dir.join("t.txt"), "t\n").unwrap();
    git_at(&dir, &["add", "t.txt"], 9);
    git_at(
        &dir,
        &[
            "commit",
            "-qm",
            "with body",
            "-m",
            "line one\n\tindented\n\nlast",
        ],
        9,
    );
    git(&dir, &["notes", "add", "-m", "a note", "HEAD~1"]);
    let all = "%H,%h,%T,%t,%P,%p,%an,%ae,%ad,%at,%ai,%aI,%as,%cn,%ce,%cd,%ct,%ci,%cI,%cs,%s,%f,%d,%D,%%,%x41";
    let cases: &[&[&str]] = &[
        &["log", "--pretty=medium"],
        &["log", "--pretty=short"],
        &["log", "--pretty=full"],
        &["log", "--pretty=fuller"],
        &["log", "--pretty=raw"],
        &["log", "--oneline"],
        &["log", "--pretty=oneline"],
        &["log", "--pretty=reference"],
        &["log", "--graph", "-3"],
        &["log", &format!("--format={all}")],
        &["log", "--format=%B"],
        &["log", "--format=[%b]"],
        &["log", "--pretty=tformat:%h%n%s"],
        &["log", "--format=%h%+d%-b% s"],
        &["log", "--date=iso", "--pretty=fuller"],
        &["log", "--date=relative", "--format=%ad,%ar,%cr"],
        &["log", "--date=short", "--format=%ad"],
        &["log", "--date=rfc", "--format=%ad"],
        &["log", "--date=iso-strict", "--format=%ad"],
        &[
            "log",
            "--date=format:%Y/%m/%d %H.%M %a %b %j",
            "--format=%ad",
        ],
        &["log", "--date=local", "--format=%ad"],
        &["log", "--oneline", "--decorate"],
        &["log", "--pretty=medium", "--decorate", "--all"],
        &["log", "--oneline", "-3"],
        &["log", "--oneline", "-p"],
        &["log", "--oneline", "--stat"],
        &["log", "--pretty=medium", "-p"],
        &["log", "--pretty=medium", "--stat", "-p"],
        &["log", "--format=%h", "--name-status"],
        &["log", "--pretty=medium", "--numstat", "--", "dir"],
        &["log", "--follow", "-p", "--pretty=medium", "--", "b.txt"],
        &["log", "--follow", "--stat", "--oneline", "--", "b.txt"],
        &[
            "log",
            "--oneline",
            "--since=2024-01-03",
            "--until=2024-01-06",
        ],
        &["log", "--oneline", "--since=2024-01-03 13:00:00"],
        &["log", "--oneline", "--since=2024-01-03T11:00:00+0000"],
        &["log", "--oneline", "--since=@1704400000"],
        &["show", "--pretty=medium"],
        &["show", "--pretty=fuller", "HEAD~2", "HEAD~3"],
        &["show", "-s", "--format=%s%n%b"],
        &["show", "--stat", "--oneline", "HEAD~3"],
        &["show", "--oneline", "--name-only", "v1"],
        &["show", "--pretty=medium", "-s", "v1", "HEAD"],
    ];
    for args in cases {
        same(&dir, args);
    }
}

/// A side branch whose change to `f` is undone before its merge, and a
/// conflicting one merged with `-X theirs`.
fn side_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-history-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    commit(&dir, 1, &[("f", "1\n"), ("g", "1\n")], "base");
    git(&dir, &["checkout", "-q", "-b", "side"]);
    commit(&dir, 2, &[("f", "2\n")], "side: f=2");
    commit(&dir, 3, &[("f", "1\n")], "side: f=1 again");
    commit(&dir, 4, &[("h", "3\n")], "side: h");
    git(&dir, &["checkout", "-q", "main"]);
    commit(&dir, 5, &[("g", "2\n")], "main: g");
    git_at(
        &dir,
        &["merge", "-q", "--no-ff", "side", "-m", "merge side"],
        6,
    );
    commit(&dir, 7, &[("f", "9\n")], "main: f=9");
    git(&dir, &["checkout", "-q", "-b", "other", "HEAD~2"]);
    commit(&dir, 8, &[("f", "5\n")], "other: f=5");
    git(&dir, &["checkout", "-q", "main"]);
    git_at(
        &dir,
        &[
            "merge",
            "-q",
            "--no-ff",
            "other",
            "-X",
            "theirs",
            "-m",
            "merge other",
        ],
        9,
    );
    dir
}

#[test]
fn log_simplifies_history_and_draws_graphs_like_git() {
    let dir = side_repo("graph");
    let cases: &[&[&str]] = &[
        &["log", "--oneline", "--", "f"],
        &["log", "--oneline", "--", "f", "g"],
        &["log", "--oneline", "--", "h"],
        &["log", "--oneline", "--first-parent", "--", "f"],
        &["log", "--oneline", "side", "--", "f"],
        &["log", "--oneline", "--graph"],
        &["log", "--oneline", "--graph", "--all"],
        &["log", "--graph", "--pretty=medium"],
        &["log", "--graph", "--format=%h %p"],
        &["log", "--oneline", "--graph", "--stat"],
        &["log", "--graph", "-p", "--pretty=medium", "-3"],
        &["log", "--oneline", "--graph", "--", "f"],
        &["log", "--oneline", "--graph", "--first-parent"],
        &["log", "--oneline", "--graph", "--reverse"],
    ];
    for args in cases {
        same(&dir, args);
    }
}

#[test]
fn log_and_diff_take_pickaxe_filters_and_reverse() {
    let dir = repo("pickaxe");
    let cases: &[&[&str]] = &[
        &["log", "--oneline", "-Sfive"],
        &["log", "--oneline", "-S5"],
        &["log", "--oneline", "-Gni.e", "--", "b.txt"],
        &["log", "--oneline", "--committer=t"],
        &["log", "--oneline", "--committer=nobody"],
        &["diff", "v1", "HEAD", "-R", "--patch"],
        &["diff", "v1", "HEAD", "-R", "--stat"],
        &["diff", "v1", "HEAD", "--name-status", "--diff-filter=A"],
        &["diff", "v1", "HEAD", "--name-status", "--diff-filter=ad"],
        &["blame", "-p", "HEAD", "--", "b.txt"],
    ];
    for args in cases {
        same(&dir, args);
    }
}

#[test]
fn log_shows_each_commits_changes() {
    let dir = repo("log-patch");
    let out = ok(&dir, &["log", "-n", "1", "-p"]);
    assert!(out.contains("-9\n+nine"), "{out}");
    let out = ok(&dir, &["log", "-n", "2", "--stat"]);
    assert!(
        out.contains(" b.txt | 2 +-") && out.contains(" a.txt => b.txt | 0"),
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
fn diff_prints_git_formats_and_exit_codes() {
    let dir = repo("diff-git");
    std::fs::create_dir_all(dir.join("dir/deep")).unwrap();
    let b = std::fs::read_to_string(dir.join("b.txt")).unwrap();
    std::fs::write(dir.join("dir/deep/c.txt"), b.replace("3\n", "three\n")).unwrap();
    std::fs::remove_file(dir.join("b.txt")).unwrap();
    std::fs::write(dir.join("bin.dat"), b"\0\x01bin").unwrap();
    std::fs::write(dir.join("noeol"), "last").unwrap();
    let long = "a-rather-long-folder-name/with-another-long-folder/and-a-long-file-name.txt";
    std::fs::create_dir_all(dir.join(long).parent().unwrap()).unwrap();
    std::fs::write(dir.join(long), "1\n".repeat(120)).unwrap();
    git(&dir, &["add", "-A"]);
    for fmt in [
        "--stat",
        "--numstat",
        "--name-status",
        "--shortstat",
        "--patch",
        "--stat --patch",
    ] {
        let mut args = vec!["diff", "--cached"];
        args.extend(fmt.split(' '));
        same(&dir, &args);
    }
    same(&dir, &["diff", "v1", "HEAD", "--stat"]);
    same(&dir, &["diff", "HEAD~2", "HEAD~1", "--stat"]);
    same(&dir, &["diff", "--cached", "--exit-code", "--stat"]);
    same(&dir, &["diff", "--cached", "--quiet"]);
    same(&dir, &["diff", "--quiet"]);
    same(&dir, &["diff", "--exit-code", "--stat"]);
    std::fs::write(dir.join("x1"), "a\nb\n").unwrap();
    std::fs::write(dir.join("x2"), "a\nc\n").unwrap();
    for fmt in ["--patch", "--stat", "--name-status", "--numstat"] {
        same(&dir, &["diff", "--no-index", fmt, "x1", "x2"]);
    }
    same(&dir, &["diff", "--no-index", "--stat", "x1", "x1"]);
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

    let dir = side_repo("describe-contains");
    git_at(&dir, &["tag", "-a", "t-late", "-m", "late", "main"], 20);
    git_at(&dir, &["tag", "-a", "t-early", "-m", "early", "side"], 10);
    git(&dir, &["tag", "light", "other"]);
    for rev in [
        "HEAD",
        "HEAD~1",
        "HEAD~2",
        "HEAD~3",
        "side~1",
        "side~2",
        "main^2",
        "main~2^2~1",
        "other",
    ] {
        same(&dir, &["describe", "--contains", rev]);
    }
    same(
        &dir,
        &["describe", "--contains", "--match", "t-l*", "HEAD~3"],
    );
    same(
        &dir,
        &[
            "describe",
            "--contains",
            "--always",
            "--match",
            "no",
            "HEAD~3",
        ],
    );
    let (_, success) = rgit(&dir, &["describe", "--contains", "--match", "no", "HEAD~3"]);
    assert!(!success);
}

#[test]
fn blame_prints_git_porcelain() {
    let dir = repo("blame-porcelain");
    let b = std::fs::read_to_string(dir.join("b.txt")).unwrap();
    std::fs::write(dir.join("b.txt"), b.replace("3\n", "three\nextra\n")).unwrap();
    for args in [
        &["blame", "--porcelain", "v1", "--", "a.txt"][..],
        &["blame", "--porcelain", "HEAD", "--", "b.txt"],
        &["blame", "--line-porcelain", "HEAD", "--", "b.txt"],
        &["blame", "--porcelain", "-L", "4,8", "HEAD", "--", "b.txt"],
    ] {
        same(&dir, args);
    }
    // The working tree's own lines are "not committed yet"; the rest match git.
    let text = |args: &[&str]| {
        let out = ok(&dir, args);
        out.lines()
            .filter(|l| !l.contains("-time "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let want: Vec<String> = git(&dir, &["blame", "--porcelain", "b.txt"])
        .lines()
        .filter(|l| !l.contains("-time "))
        .map(str::to_owned)
        .collect();
    assert_eq!(text(&["blame", "--porcelain", "b.txt"]), want);

    let out = ok(&dir, &["blame", "-s", "-l", "b.txt"]);
    let first = out.lines().next().unwrap();
    assert_eq!(first.split(' ').next().unwrap().len(), 40, "{out}");
    assert!(first.ends_with(" 1") && !first.contains(" t "), "{out}");
    let out = ok(&dir, &["blame", "-e", "b.txt"]);
    assert!(out.lines().next().unwrap().contains(" <t@t> "), "{out}");
}

#[test]
fn format_leaves_off_the_final_newline_and_tformat_keeps_it() {
    let dir = repo("log-format-end");
    same(&dir, &["log", "-2", "--format=format:%h %s"]);
    same(&dir, &["log", "-2", "--format=tformat:%h %s"]);
    same(&dir, &["show", "-s", "--format=format:%h", "HEAD"]);
}
