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
        // Identity comes from the repo's own config in these tests; an
        // ambient GIT_AUTHOR_*/GIT_COMMITTER_* (e.g. CI-wide env) would
        // override it.
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
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
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status.code(),
        )
    };
    let want = run(&mut Command::new("git"));
    let got = run(Command::new(env!("CARGO_BIN_EXE_rgit")).arg("--human"));
    let at = got.0.lines().zip(want.0.lines()).find(|(a, b)| a != b);
    assert_eq!(got, want, "rgit {args:?}, first differing lines {at:?}");
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_at(dir, args, 1)
}

/// Whether the oracle's `log -L` prints the shape rgit tracks: full diff
/// headers (git 2.54) and no blank line between an oneline message and its
/// diff (git 2.55). An older git cannot reproduce that output.
fn line_log_modern() -> bool {
    static MODERN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MODERN.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rgit-linelog-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), "a\nb\n").unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "user.email", "t@t"]);
        run(&["add", "f"]);
        run(&["commit", "-qm", "one"]);
        std::fs::write(dir.join("f"), "a\nc\n").unwrap();
        run(&["add", "f"]);
        run(&["commit", "-qm", "two"]);
        let out = run(&["log", "--oneline", "-L1,1:f"]);
        let text = String::from_utf8_lossy(&out.stdout);
        let blank_before_diff = text
            .lines()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w[1].starts_with("diff --git") && w[0].is_empty());
        text.contains("index ") && !blank_before_diff
    })
}

/// The log cases an old oracle can still compare: rgit's `-L` output
/// tracks git 2.55, so drop `log -L` cases when the oracle predates it.
fn cmp_cases<'b, 'c>(cases: &[&'b [&'c str]]) -> Vec<&'b [&'c str]> {
    if line_log_modern() {
        return cases.to_vec();
    }
    eprintln!("skipping log -L cases: git predates 2.55 line-log output");
    cases
        .iter()
        .copied()
        .filter(|a| a.first() != Some(&"log") || !a.iter().any(|s| s.starts_with("-L")))
        .collect()
}

fn rgit(dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["--human", "--compact"])
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
fn log_walks_like_git() {
    let dir = repo("walk");
    git(&dir, &["checkout", "-q", "topic"]);
    git_at(&dir, &["cherry-pick", "main~3"], 9);
    git(&dir, &["checkout", "-q", "main"]);
    let cases: &[&[&str]] = &[
        &["log", "--oneline", "--left-right", "topic...main"],
        &["log", "--oneline", "--cherry-mark", "topic...main"],
        &["log", "--oneline", "--cherry-pick", "topic...main"],
        &["log", "--oneline", "--cherry", "topic...main"],
        &["log", "--oneline", "--right-only", "topic...main"],
        &["log", "--format=%m %h %s", "--boundary", "topic...main"],
        &["log", "--oneline", "--boundary", "-2"],
        &[
            "log",
            "--pretty=medium",
            "--left-right",
            "--boundary",
            "-3",
            "topic...main",
        ],
        &[
            "log",
            "--oneline",
            "--graph",
            "--left-right",
            "--cherry-mark",
            "topic...main",
        ],
        &["log", "--oneline", "--graph", "--boundary", "topic...main"],
        &["log", "--oneline", "--ancestry-path", "v1..main"],
        &["log", "--oneline", "--simplify-by-decoration", "--all"],
        &["log", "--oneline", "--full-history", "--", "a.txt"],
        &["log", "--oneline", "--sparse", "--", "dir"],
        &[
            "log",
            "--oneline",
            "--simplify-merges",
            "--",
            "a.txt",
            "dir",
        ],
        &["log", "--oneline", "--parents", "--", "dir"],
        &["log", "--pretty=medium", "--parents", "-2"],
        &["log", "--oneline", "--source", "--all"],
        &["log", "--format=%S %h", "--source", "main", "topic"],
        &["log", "--oneline", "--no-walk", "topic", "v1", "main"],
        &[
            "log",
            "--oneline",
            "--no-walk=unsorted",
            "v1",
            "topic",
            "main",
        ],
        &["log", "--oneline", "--topo-order", "--all"],
        &["log", "--oneline", "--date-order", "--all"],
        &["log", "--oneline", "--author-date-order", "--all"],
        &[
            "log",
            "--oneline",
            "--grep=fix",
            "--grep=bug",
            "--all-match",
            "-i",
        ],
        &["log", "--oneline", "--grep=fix", "--invert-grep"],
        &[
            "log",
            "--oneline",
            "--since=Jan 4 2024",
            "--until=2024-01-07 12:00",
        ],
        &["log", "--oneline", "--since=5.years.ago"],
        &["log", "--date=human", "--format=%ad %ah"],
        &["log", "--oneline", "-g", "-3"],
        &["log", "-g", "-2", "--format=%gd %gs %h"],
        &["log", "--pretty=medium", "-g", "-1"],
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

#[test]
fn describe_walks_as_git_does() {
    let dir = side_repo("describe-walk");
    let describe =
        |extra: &[&'static str]| -> Vec<&'static str> { [&["describe"][..], extra].concat() };
    // Tags only on the side branch and the base: the merge reaches both.
    let none: &[&[&str]] = &[
        &["describe"],
        &["describe", "--always"],
        &["describe", "--tags"],
    ];
    all_same(&dir, none);
    git_at(&dir, &["tag", "-a", "base", "-m", "base", "main~3"], 1);
    git_at(&dir, &["tag", "-a", "s1", "-m", "s1", "side~1"], 3);
    git_at(&dir, &["tag", "-a", "s2", "-m", "s2", "side~1"], 4);
    git(&dir, &["tag", "light", "main~1"]);
    let cases: Vec<Vec<&str>> = vec![
        describe(&[]),
        describe(&["--tags"]),
        describe(&["--all"]),
        describe(&["--all", "side"]),
        describe(&["--all", "--match", "s*"]),
        describe(&["--first-parent"]),
        describe(&["--first-parent", "--tags"]),
        describe(&["--candidates=1"]),
        describe(&["--candidates=0"]),
        describe(&["--candidates=0", "s1"]),
        describe(&["--exclude", "s*"]),
        describe(&["--exclude", "s*", "--exclude", "base"]),
        describe(&["--match", "base", "--match", "s1"]),
        describe(&["--match", "s1"]),
        describe(&["--match", "s2"]),
        describe(&["--exclude", "s2"]),
        describe(&["--long", "s2"]),
        describe(&["--abbrev=4"]),
        describe(&["--abbrev=0", "--tags"]),
        describe(&["--dirty"]),
        describe(&["--dirty=-mod", "--broken"]),
        describe(&["--dirty", "HEAD"]),
        describe(&["--long", "--abbrev=0"]),
        describe(&["other"]),
    ];
    let refs: Vec<&[&str]> = cases.iter().map(Vec::as_slice).collect();
    all_same(&dir, &refs);
    std::fs::write(dir.join("f"), "changed\n").unwrap();
    all_same(
        &dir,
        &[
            &["describe", "--dirty"],
            &["describe", "--dirty=.mod"],
            &["describe", "--broken"],
            &["describe"],
        ],
    );
}

/// Whitespace, moved, copied and renamed lines by three authors, for blame.
fn blame_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-history-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let who = |name: &str| {
        git(&dir, &["config", "user.name", name]);
        git(&dir, &["config", "user.email", &format!("{name}@x")]);
    };
    who("ann");
    let c = "int main() {\n  return 0;\n}\n\nvoid helper(void) {\n  puts(\"hi\");\n}\n\n\
             static int other = 1;\n";
    let b = "alpha beta gamma delta\nepsilon zeta eta theta\niota kappa lambda mu\n";
    commit(&dir, 1, &[("a.c", c), ("b.txt", b)], "init");
    who("bob");
    let c2 = c
        .replace("return 0", "return  0")
        .replace("\"hi\"", "\"hello\"");
    commit(&dir, 2, &[("a.c", &c2)], "spacing and greeting");
    let b2 =
        "iota kappa lambda mu\nalpha beta gamma delta\nepsilon zeta eta theta\nnew line here\n";
    commit(&dir, 3, &[("b.txt", b2)], "move a line");
    who("carol");
    let c3 = c2
        .replace("int main() {", "/* banner */\nint  main()  {")
        .replace("= 1", "= 2");
    let copy = format!("{b2}extra words for the copy\n");
    commit(&dir, 4, &[("a.c", &c3), ("c.txt", &copy)], "copy b");
    git_at(&dir, &["mv", "b.txt", "d.txt"], 5);
    git_at(&dir, &["commit", "-qm", "rename b"], 5);
    dir
}

/// Every `rgit --human <args>` in `cases` prints git's bytes and exit code.
fn all_same(dir: &Path, cases: &[&[&str]]) {
    let run = |cmd: &mut Command, args: &[&str]| {
        let out = cmd
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("RGIT_OPLOG", "0")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status.code(),
        )
    };
    let failed: Vec<String> = cases
        .iter()
        .filter_map(|args| {
            let want = run(&mut Command::new("git"), args);
            let got = run(
                Command::new(env!("CARGO_BIN_EXE_rgit")).arg("--human"),
                args,
            );
            (got != want).then(|| format!("{args:?}\n--- git\n{want:?}\n--- rgit\n{got:?}"))
        })
        .collect();
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn blame_follows_git_options() {
    let dir = blame_repo("blame-options");
    let ws = git(&dir, &["rev-parse", "HEAD~3"]);
    std::fs::write(dir.join("ignore-revs"), format!("# spacing\n{ws}")).unwrap();
    all_same(
        &dir,
        &[
            &["blame", "-t", "a.c"],
            &["blame", "--root", "a.c"],
            &["blame", "-w", "-t", "a.c"],
            &["blame", "-n", "-f", "-t", "d.txt"],
            &["blame", "-M", "-t", "d.txt"],
            &["blame", "-M5", "-t", "d.txt"],
            &["blame", "-C", "-t", "c.txt"],
            &["blame", "-C", "-C", "-t", "c.txt"],
            &["blame", "-C", "-C", "-C", "-t", "c.txt"],
            &["blame", "-C10", "-C", "-n", "-t", "c.txt"],
            &["blame", "--porcelain", "-M", "d.txt"],
            &["blame", "--line-porcelain", "-C", "-C", "c.txt"],
            &["blame", "-L", ":helper", "-t", "a.c"],
            &["blame", "-L", "/return/,+2", "-t", "a.c"],
            &["blame", "-L", "1,2", "-L", "5,6", "-t", "a.c"],
            &["blame", "-L", "5,6", "-L", "/other/", "-t", "a.c"],
            &["blame", "-L", "^/int/,-1", "-t", "a.c"],
            &["blame", "-L", "2,1", "--porcelain", "a.c"],
            &["blame", "HEAD~2..", "-t", "a.c"],
            &["blame", "--first-parent", "-t", "a.c"],
            &["blame", "--reverse", "HEAD~4..HEAD~1", "-t", "a.c"],
            &["blame", "--reverse", "HEAD~4", "-t", "--", "a.c"],
            &["blame", "--ignore-rev", "HEAD~3", "-t", "a.c"],
            &["blame", "--ignore-revs-file", "ignore-revs", "-t", "a.c"],
            &["blame", "-c", "a.c"],
            &["blame", "-b", "HEAD~1", "--", "a.c"],
            &["blame", "--date=short", "-e", "a.c"],
            &["blame", "--date=rfc", "-s", "a.c"],
            &["blame", "-l", "-t", "a.c"],
            &["blame", "--abbrev=10", "-t", "a.c"],
            &["blame", "--show-stats", "-t", "HEAD", "--", "d.txt"],
        ],
    );
    git(&dir, &["config", "blame.markIgnoredLines", "true"]);
    git(&dir, &["config", "blame.markUnblamableLines", "true"]);
    all_same(&dir, &[&["blame", "--ignore-rev", "HEAD~1", "-t", "a.c"]]);
    git(&dir, &["config", "blame.ignoreRevsFile", "ignore-revs"]);
    all_same(
        &dir,
        &[
            &["blame", "-t", "a.c"],
            &["blame", "--ignore-revs-file", "", "-t", "a.c"],
        ],
    );

    let dir = repo("blame-merges");
    all_same(
        &dir,
        &[
            &["blame", "-t", "b.txt"],
            &["blame", "--first-parent", "-n", "-t", "b.txt"],
            &["blame", "-C", "-t", "HEAD~3", "--", "a.txt"],
            &["blame", "--reverse", "v1..HEAD~3", "-t", "--", "a.txt"],
            &["blame", "--porcelain", "v1..", "--", "b.txt"],
        ],
    );
}

const F: &str = "int main() {\n\ta();\n\tb();\n\tc();\n\td();\n\te();\n}\n\nstatic void helper(void)\n{\n\tone();\n\ttwo();\n\tthree();\n\tfour();\n\tfive();\n\tsix();\n}\n";

/// A merge with a hand-resolved conflict, an evil line, a file added on
/// both sides, one it deletes and one whose mode it changes, then a clean
/// octopus.
fn merge_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-history-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    let x = "x1\nx2\nx3\nx4\nx5\nx6\nx7\nx8\nx9\nx10\nx11\nx12\n";
    commit(
        &dir,
        1,
        &[
            ("f.c", F),
            ("h", A),
            ("e", "e\n"),
            ("run.sh", "run\n"),
            ("x", x),
        ],
        "base",
    );
    git(&dir, &["checkout", "-q", "-b", "side"]);
    let side_f = F.replace("b();", "B_side();").replace("five();", "FIVE();");
    commit(
        &dir,
        2,
        &[
            ("f.c", &side_f),
            ("h", &A.replace("2\n", "two\n")),
            ("both", "side\n"),
        ],
        "side work",
    );
    git(&dir, &["checkout", "-q", "main"]);
    let main_f = F
        .replace("b();", "B_main();")
        .replace("three();\n", "three();\n\tthree_and_half();\n");
    commit(
        &dir,
        3,
        &[
            ("f.c", &main_f),
            ("h", &A.replace("9\n", "nine\n")),
            ("both", "main\n"),
        ],
        "main work",
    );
    git_at(
        &dir,
        &[
            "merge",
            "-q",
            "--no-ff",
            "--no-commit",
            "-s",
            "ours",
            "side",
        ],
        4,
    );
    let merged = main_f
        .replace("B_main();", "B_merged();")
        .replace("five();", "FIVE();")
        + "/* evil */\n";
    std::fs::write(dir.join("f.c"), merged).unwrap();
    std::fs::write(
        dir.join("h"),
        A.replace("2\n", "two\n").replace("9\n", "nine\n"),
    )
    .unwrap();
    std::fs::write(dir.join("both"), "merged\n").unwrap();
    std::fs::remove_file(dir.join("e")).unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["update-index", "--chmod=+x", "run.sh"]);
    git_at(&dir, &["commit", "-qm", "merge side"], 4);
    for (i, (b, from)) in [("o1", "x2\n"), ("o2", "x6\n"), ("o3", "x10\n")]
        .iter()
        .enumerate()
    {
        git(&dir, &["checkout", "-q", "-b", b, "main"]);
        let to = from.to_uppercase();
        commit(&dir, 5 + i as u32, &[("x", &x.replace(from, &to))], b);
    }
    git(&dir, &["checkout", "-q", "main"]);
    git_at(&dir, &["merge", "-q", "o1", "o2", "o3", "-m", "octopus"], 9);
    dir
}

#[test]
fn log_and_show_diff_merges_like_git() {
    let dir = merge_repo("diff-merges");
    let cases: &[&[&str]] = &[
        &["show", "--format=medium", "HEAD~1"],
        &["show", "--format=medium"],
        &["show", "-c", "--format=medium", "HEAD~1"],
        &["show", "-c", "--oneline"],
        &["show", "--cc", "--stat", "--oneline", "HEAD~1"],
        &["show", "--stat", "-p", "--format=medium", "HEAD~1"],
        &["show", "--name-status", "--oneline", "HEAD~1"],
        &["show", "-c", "--name-only", "--format=%h", "HEAD~1"],
        &["show", "-m", "--oneline", "HEAD~1"],
        &["show", "-m", "--stat", "--format=medium", "HEAD~1"],
        &["show", "--first-parent", "--oneline", "--stat", "HEAD~1"],
        &["show", "--remerge-diff", "--format=medium", "HEAD~1"],
        &["show", "--diff-merges=off", "--format=medium", "HEAD~1"],
        &["log", "-p", "--oneline"],
        &["log", "--cc", "--oneline"],
        &["log", "-c", "--format=%h %s"],
        &["log", "-m", "--oneline"],
        &["log", "-m", "-p", "--oneline"],
        &["log", "-m", "--name-status", "--format=medium"],
        &["log", "--diff-merges=first-parent", "--stat", "--oneline"],
        &["log", "--first-parent", "-p", "--oneline"],
        &["log", "--dd", "--oneline", "-2"],
        &["log", "--remerge-diff", "--oneline"],
        &["log", "-m", "-p", "--oneline", "--graph"],
        &["log", "--cc", "--stat", "--oneline", "--graph"],
        &["log", "--cc", "--oneline", "--", "h"],
        &["log", "-c", "-m", "--oneline"],
        &["log", "-m", "-c", "--oneline"],
        &["log", "-m", "-p", "--format=medium", "--graph", "-5"],
        &[
            "log",
            "--cc",
            "--stat",
            "-p",
            "--format=medium",
            "--graph",
            "-5",
        ],
        &["show", "--cc", "--numstat", "--format=%h", "HEAD~1"],
        &["show", "-c", "--name-status", "--format=%h"],
        &["log", "-g", "-p", "-4"],
        &["show", "--color=always", "--format=medium", "HEAD~1"],
        &["show", "--color=always", "-c", "--oneline", "HEAD~1"],
        &["log", "--color=always", "-L:helper:f.c", "--oneline"],
        &["log", "-g", "--stat", "--oneline", "-6"],
    ];
    for args in cmp_cases(cases) {
        same(&dir, args);
    }
}

#[test]
fn log_traces_line_ranges_like_git() {
    if !line_log_modern() {
        eprintln!("skipping: git predates 2.55 line-log output");
        return;
    }
    let dir = merge_repo("line-log");
    let cases: &[&[&str]] = &[
        &["log", "-L:helper:f.c"],
        &["log", "-L:main:f.c", "--oneline"],
        &["log", "-L/three/,+3:f.c", "--format=%h %s"],
        &["log", "-L1,3:h", "-L8,10:h", "--oneline"],
        &["log", "-L2,2:x", "-L1,1:h", "--oneline"],
        &["log", "-L12,:f.c", "--oneline", "-n", "2"],
        &["log", "-L3,+2:x", "--first-parent", "--oneline"],
        &["log", "-L1,5:h", "--oneline", "side"],
    ];
    for args in cases {
        same(&dir, args);
    }
    let dir = repo("line-log-rename");
    std::fs::write(dir.join("b.txt"), "1\n3\n4\nfive\n6\n7\n8\nnine\n10\n").unwrap();
    git_at(&dir, &["commit", "-qam", "drop a line"], 9);
    for args in [
        &["log", "-L7,9:b.txt", "--oneline"][..],
        &["log", "-L1,4:b.txt", "--format=medium"],
        &["log", "-L/five/,/nine/:b.txt", "--oneline", "--reverse"],
    ] {
        same(&dir, args);
    }
}

#[test]
fn colors_match_gits_palette() {
    let dir = repo("colors");
    // An octopus merge, for its dashes.
    git(&dir, &["checkout", "-q", "-b", "o1", "HEAD~3"]);
    commit(&dir, 9, &[("o1", "o\n")], "o1");
    git(&dir, &["checkout", "-q", "-b", "o2", "HEAD~1"]);
    commit(&dir, 9, &[("o2", "o\n")], "o2");
    git(&dir, &["checkout", "-q", "main"]);
    git_at(&dir, &["merge", "-q", "--no-edit", "o1", "o2", "topic"], 9);
    std::fs::write(dir.join("b.txt"), "1\ntrailing  \n\nnew\n").unwrap();
    git(&dir, &["config", "color.ui", "always"]);
    let cases: &[&[&str]] = &[
        &["log", "--graph", "--oneline"][..],
        &["log", "--graph", "--oneline", "--decorate", "--all"],
        &["log", "--graph", "--stat", "-4"],
        &["log", "--oneline", "--decorate"],
        &["log", "--format=%C(auto)%h%d %C(red)%s%C(reset) %D"],
        &["log", "--pretty=medium", "-p", "-2"],
        &["diff", "-p"],
        &["diff", "--stat"],
        &["diff", "-p", "v1", "HEAD", "--", "dir"],
        &["grep", "-n", "o"],
        &["grep", "-c", "e"],
        &["grep", "-l", "e"],
        &["grep", "--heading", "-n", "-C1", "five"],
        &["grep", "-o", "t.o"],
        &["grep", "-n", "e", "HEAD~1"],
        &[
            "grep", "-n", "-e", "two", "--or", "-e", "nine", "--and", "--not", "-e", "x",
        ],
        &["grep", "-p", "-n", "nine"],
        &["grep", "-W", "five"],
        &[
            "log",
            "--graph",
            "--oneline",
            "--left-right",
            "--boundary",
            "main...topic",
        ],
        &[
            "log",
            "--graph",
            "--oneline",
            "--cherry-mark",
            "main...side",
        ],
        &["show", "--format=medium"],
        &["show", "--format=medium", "HEAD~3"],
        &["show", "-c", "--oneline", "HEAD~3"],
        &["log", "-m", "-p", "--oneline", "-1", "HEAD~3"],
        &["log", "-L2,4:b.txt", "--oneline"],
    ];
    for args in cmp_cases(cases) {
        same(&dir, args);
    }
    same(&dir, &["log", "--graph", "--oneline", "--no-color"]);
    same(&dir, &["log", "--graph", "--oneline", "--color=never"]);
    git(&dir, &["config", "color.diff", "never"]);
    same(&dir, &["log", "--graph", "--oneline"]);
    same(&dir, &["diff", "-p"]);
    same(&dir, &["grep", "-n", "o"]);
    git(&dir, &["config", "--unset", "color.ui"]);
    git(&dir, &["config", "--unset", "color.diff"]);
    // Piped output stays plain unless asked.
    same(&dir, &["log", "--graph", "--oneline", "--decorate"]);
    same(&dir, &["log", "--graph", "--oneline", "--color"]);
    same(&dir, &["diff", "-p", "--color=always"]);
}

#[test]
fn revisions_take_gits_forms() {
    let dir = repo("revs");
    git(
        &dir,
        &["remote", "add", "origin", "https://example.invalid/r"],
    );
    git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
    git(&dir, &["config", "branch.main.remote", "origin"]);
    git(&dir, &["config", "branch.main.merge", "refs/heads/main"]);
    all_same(
        &dir,
        &[
            &["rev-parse", "HEAD^!", "HEAD~2^@", "HEAD^-", "HEAD~2^-2"],
            &["rev-parse", "--symbolic", "HEAD~3^!"],
            &[
                "rev-parse",
                "@{-1}",
                "@{-2}",
                "@{u}",
                "@{push}",
                "main@{upstream}",
            ],
            &["rev-parse", "--abbrev-ref", "@{u}", "@{push}"],
            &[
                "rev-parse",
                "HEAD^{/bug one}",
                ":/side work",
                ":/!-e",
                "HEAD^{/!-Fix}",
            ],
            &[
                "rev-parse",
                "HEAD^{/}",
                "v1^{tag}",
                "v1^{commit}",
                "HEAD^{tree}",
            ],
            &[
                "rev-parse",
                "HEAD@{2024-01-05}",
                "main@{2024-01-03 13:00}",
                ":0:b.txt",
            ],
            &["rev-parse", "side@{u}"],
            &["rev-parse", "--show-object-format"],
            &["log", "--oneline", "HEAD~3^!"],
            &["log", "--oneline", "HEAD~3^-"],
            &["log", "--oneline", "@{u}.."],
            &["diff", "--stat", "HEAD^!"],
            &["show", "-s", "--format=%s", ":/side"],
            &["cat-file", "-p", "HEAD:./b.txt"],
        ],
    );
    let sub = dir.join("dir");
    for spec in ["HEAD:./x", "HEAD:../b.txt", ":./x", "HEAD~1:./"] {
        same(&sub, &["rev-parse", spec]);
    }
}

#[test]
fn reflog_walks_and_limits_like_git() {
    let dir = repo("reflog");
    git_at(&dir, &["checkout", "-q", "side"], 9);
    git_at(&dir, &["checkout", "-q", "main"], 10);
    all_same(
        &dir,
        &[
            &["log", "-g", "--oneline", "main", "side"],
            &[
                "log",
                "-g",
                "--format=%gd %gD %gs",
                "main",
                "refs/heads/side",
                "HEAD",
            ],
            &["log", "-g", "-1", "main"],
            &["log", "-g", "--oneline", "main@{2}"],
            &["log", "-g", "--oneline", "HEAD@{2024-01-06}"],
            &["log", "-g", "--oneline", "--date=iso", "HEAD"],
            &["log", "-g", "--format=%gd|%gD", "--date=short"],
            &["reflog", "--date=iso", "main"],
            &["reflog", "-2", "--format=%h %gd"],
            &["reflog", "--date=unix"],
            &["log", "--oneline", "--since=2024-01-05"],
            &["log", "--oneline", "--since=2024-01-05", "--topo-order"],
            &[
                "log",
                "--oneline",
                "--cherry-pick",
                "--left-right",
                "side...topic",
                "--",
                "dir",
            ],
        ],
    );
    std::fs::write(dir.join("b.txt"), "stash\n").unwrap();
    git_at(&dir, &["stash", "-q"], 11);
    all_same(
        &dir,
        &[
            &["stash", "list"],
            &["stash", "list", "--date=iso"],
            &["stash", "list", "--date=raw", "--format=%gd %gs"],
        ],
    );
}

#[test]
fn blame_reads_contents_and_prints_incremental() {
    let dir = repo("blame-inc");
    std::fs::write(dir.join("alt"), "1\ntwo\nnew\n4\n").unwrap();
    all_same(
        &dir,
        &[
            &["blame", "--incremental", "b.txt"],
            &["blame", "--incremental", "-L", "2,5", "b.txt"],
            &["blame", "--incremental", "HEAD~2", "--", "a.txt"],
            &["blame", "--root", "--incremental", "b.txt"],
            &[
                "blame",
                "--progress",
                "--date=short",
                "--contents",
                "alt",
                "b.txt",
            ],
            &[
                "blame",
                "-e",
                "--date=short",
                "--contents",
                "alt",
                "HEAD~1",
                "--",
                "b.txt",
            ],
            &[
                "blame",
                "--encoding=UTF-8",
                "--date=short",
                "--contents",
                "alt",
                "b.txt",
            ],
        ],
    );
}

const RUST: &str = "use std::io;\n\npub fn alpha(x: u32) -> u32 {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    let d = 4;\n    let e = 5;\n    a + b + c + d + e + x\n}\n\nimpl Foo {\n    fn beta(&self) {\n        println!(\"hello world\");\n        let q = 7;\n        let r = 8;\n        let s = 9;\n    }\n}\n\nstruct Bar;\n";
const PY: &str = "import os\n\nclass K:\n    def m(self):\n        x = 1\n        y = 2\n        z = 3\n        w = 4\n        return x\n\n    async def n(self):\n        return 5\n";
const C: &str = "#include <stdio.h>\n\nint main(int argc, char **argv)\n{\n\tint i = 0;\n\tint j = 1;\n\tint k = 2;\nlabel:\n\tint l = 3;\n\tint m = 4;\n\tint n = 5;\n\treturn i;\n}\n";

#[test]
fn diff_names_functions_and_diffs_words_like_git() {
    let dir = std::env::temp_dir().join(format!("rgit-history-{}-userdiff", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    git(
        &dir,
        &["config", "diff.cfg.xfuncname", "!^START\n^[A-Za-z].*$"],
    );
    git(&dir, &["config", "diff.basic.funcname", "^BEGIN\\(.*\\)$"]);
    let body = |heads: &[&str]| -> String {
        heads
            .iter()
            .map(|h| {
                format!(
                    "{h}\n{}",
                    "    body 1\n    body 2\n    body 3\n    body 4\n    body 5\n    body 6\n"
                )
            })
            .collect()
    };
    let long = format!("int {}(void)", "x".repeat(90));
    let files = [
        (".gitattributes", "*.rs diff=rust\n*.py diff=python\n*.c diff=cpp\n*.xx diff=cfg\n*.yy diff=basic\n*.tex diff=tex\n*.sh diff=bash\n".to_owned()),
        ("a.rs", RUST.to_owned()),
        ("a.py", PY.to_owned()),
        ("a.c", C.to_owned()),
        ("plain.txt", "x\ny\n".to_owned()),
        ("cfg.xx", body(&["START here", "middle", "start lower"])),
        ("b.yy", body(&["BEGIN(x)", "other"])),
        ("a.tex", body(&["\\section{Intro}", "\\subsection*{More}"])),
        ("a.sh", body(&["foo() {", "function bar {"])),
        ("long.c", body(&[&long])),
    ];
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    commit(&dir, 1, &refs, "one");
    let edit = |p: &str, f: &dyn Fn(String) -> String| {
        let t = std::fs::read_to_string(dir.join(p)).unwrap();
        std::fs::write(dir.join(p), f(t)).unwrap();
    };
    edit("a.rs", &|t| {
        t.replace("let e = 5;", "let e = 55;")
            .replace("hello world", "hello there world")
            .replace("let s = 9;", "let s = 0x1F + s;")
    });
    edit("a.py", &|t| {
        t.replace("w = 4", "w = 44").replace("return 5", "return 6")
    });
    edit("a.c", &|t| t.replace("int m = 4;", "int m = 44;"));
    edit("plain.txt", &|_| "x\nz".to_owned());
    for p in ["cfg.xx", "b.yy", "a.tex", "a.sh", "long.c"] {
        edit(p, &|t| {
            t.replace("body 5", "body five")
                .replace("body 2", "body two")
        });
    }
    all_same(
        &dir,
        &[
            &["diff", "-p"],
            &["diff", "-p", "-U1"],
            &["diff", "-p", "-U0"],
            &["diff", "-p", "-W"],
            &["diff", "-p", "--function-context", "-U1"],
            &["diff", "-p", "--word-diff"],
            &["diff", "-p", "--word-diff=porcelain"],
            &["diff", "-p", "--word-diff=color"],
            &["diff", "-p", "--color-words"],
            &["diff", "-p", "--color-words=."],
            &["diff", "-p", "--word-diff-regex=[a-z]+"],
            &["diff", "-p", "--word-diff", "-W"],
        ],
    );
    git(&dir, &["config", "diff.wordRegex", "[^ ]"]);
    git(&dir, &["config", "diff.rust.wordRegex", "[a-z]+"]);
    all_same(&dir, &[&["diff", "-p", "--word-diff"]]);
    git_at(&dir, &["commit", "-qam", "two"], 2);
    // The log -L cases filter themselves out on a pre-2.55 oracle; blame's
    // -L is blame's own range option and always runs.
    all_same(
        &dir,
        &cmp_cases(&[
            &["log", "--format=medium", "-p", "-W"],
            &["log", "--format=medium", "-p", "--word-diff"],
            &["show", "--format=medium", "--color-words"],
            &["show", "--format=medium", "-W"],
            &["log", "-L:beta:a.rs", "--oneline"],
            &["log", "-L:m:a.py", "--format=%s"],
            &["blame", "-n", "-L:main", "a.c"],
            &["blame", "-n", "-L:beta", "a.rs"],
        ]),
    );
}

#[test]
fn diff_options_match_git() {
    let dir = repo("diff-opts");
    std::fs::create_dir_all(dir.join("dir/deep")).unwrap();
    let moved: String = (1..=30)
        .map(|i| format!("line {i} of the moved block here\n"))
        .collect();
    let big: String = (1..=60)
        .map(|i| format!("original content line number {i}\n"))
        .collect();
    commit(
        &dir,
        9,
        &[
            ("m.txt", &moved),
            ("big.txt", &big),
            ("dir/deep/w.txt", "x\ny\n"),
            (
                "alg.c",
                "int a(void)\n{\n\treturn 1;\n}\n\nint b(void)\n{\n\treturn 2;\n}\n",
            ),
            ("gone.txt", "bye\n"),
        ],
        "base",
    );
    let lines: Vec<&str> = moved.lines().collect();
    let mut shuffled: Vec<&str> = lines[..4].to_vec();
    shuffled.extend(&lines[15..]);
    shuffled.extend(&lines[4..15]);
    std::fs::write(dir.join("m.txt"), shuffled.join("\n") + "\n").unwrap();
    let rewrite: String = (1..=60)
        .map(|i| format!("completely different {i}\n"))
        .collect();
    std::fs::write(dir.join("big.txt"), rewrite).unwrap();
    std::fs::write(
        dir.join("dir/deep/w.txt"),
        "x\n \ty\nz  \n    eight\n\ttabbed\n\n\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("alg.c"),
        "int b(void)\n{\n\treturn 2;\n}\n\nint a(void)\n{\n\treturn 1;\n}\n",
    )
    .unwrap();
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    for args in [
        &["diff", "-p", "--minimal"][..],
        &["diff", "-p", "--patience"],
        &["diff", "-p", "--histogram"],
        &["diff", "-p", "--diff-algorithm=histogram"],
        &["diff", "-p", "--anchored=int"],
        &["diff", "-p", "--inter-hunk-context=5"],
        &["diff", "-p", "--src-prefix=X/", "--dst-prefix=Y/"],
        &["diff", "-p", "--no-prefix"],
        &["diff", "-p", "--full-index"],
        &["diff", "-p", "-D"],
        &["diff", "-p", "-B"],
        &["diff", "-B", "--name-status"],
        &["diff", "-p", "--relative=dir"],
        &["diff", "-p", "--line-prefix=> "],
        &["diff", "--check"],
        &["diff", "--dirstat=0"],
        &["diff", "--dirstat=files,lines,cumulative"],
        &["diff", "--compact-summary"],
        &["diff", "--stat=50,10,2"],
        &["diff", "--stat", "--stat-graph-width=3"],
        &["diff", "--color", "-p"],
        &["diff", "--color", "-p", "--ws-error-highlight=all"],
        &["diff", "--color", "-p", "--color-moved"],
        &["diff", "--color", "-p", "--color-moved=dimmed-zebra"],
        &["diff", "--color", "-p", "--color-moved=blocks"],
        &[
            "diff",
            "--color",
            "-p",
            "--color-moved",
            "--color-moved-ws=allow-indentation-change",
        ],
    ] {
        same(&dir, args);
    }
    git(&dir, &["add", "-A"]);
    git_at(&dir, &["commit", "-qm", "change"], 10);
    for args in [
        &["log", "-1", "--format=%s", "-p", "--histogram"][..],
        &["show", "--format=%s", "--dirstat=0"],
        &["show", "--format=%s", "--compact-summary"],
        &["show", "--format=%s", "--check"],
        &["show", "--format=%s", "--binary", "--", "dir"],
    ] {
        same(&dir, args);
    }
    // textconv and external diff tools.
    std::fs::write(dir.join(".gitattributes"), "*.c diff=upper\n").unwrap();
    git(&dir, &["config", "diff.upper.textconv", "tr a-z A-Z <"]);
    same(&dir, &["show", "--format=%s", "--", "alg.c"]);
    same(
        &dir,
        &["show", "--format=%s", "--no-textconv", "--", "alg.c"],
    );
    let ext = dir.join("ext.sh");
    std::fs::write(
        &ext,
        "#!/bin/sh\necho \"EXT $# [$1] [$3] [$4] [$6] [$7] $GIT_DIFF_PATH_COUNTER/$GIT_DIFF_PATH_TOTAL\"\ncat \"$5\"\n",
    )
    .unwrap();
    git(
        &dir,
        &[
            "config",
            "diff.upper.command",
            &format!("sh {}", ext.display()),
        ],
    );
    same(&dir, &["show", "--format=%s", "--ext-diff"]);
    same(&dir, &["diff", "-p", "HEAD~1"]);
    same(&dir, &["diff", "-p", "HEAD~1", "--no-ext-diff"]);
    // --output writes the patch to a file.
    let (a, b) = (dir.join("out-git"), dir.join("out-rgit"));
    git(
        &dir,
        &[
            "diff",
            "HEAD~1",
            "--no-ext-diff",
            &format!("--output={}", a.display()),
        ],
    );
    ok(
        &dir,
        &[
            "diff",
            "-p",
            "HEAD~1",
            "--no-ext-diff",
            &format!("--output={}", b.display()),
        ],
    );
    assert_eq!(std::fs::read(a).unwrap(), std::fs::read(b).unwrap());
}
