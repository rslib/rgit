//! Human-mode failures read as git's: the same `fatal:`/`error:` lines on
//! stderr and the same exit codes (128 fatal, 129 usage, 1 error). rgit names
//! itself `rgit` where git says `git`, so rgit's text is compared with every
//! `rgit` read as `git`. Usage text after the first line is rgit's own (its
//! options differ from git's), so for those only the error line, the
//! `usage:` line that follows and the exit code are compared.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(bin: &str, dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin);
    if bin == RGIT {
        cmd.arg("--human");
    }
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .env("GIT_ADVICE", "0")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = run("git", dir, args);
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

/// A repo with `a` committed twice on main, a `side` branch that changes `a`
/// the other way, and HEAD on main.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-errors-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    git(&dir, &["add", "a"]);
    git(&dir, &["commit", "-qm", "one"]);
    git(&dir, &["checkout", "-qb", "side"]);
    std::fs::write(dir.join("a"), "side\n").unwrap();
    git(&dir, &["commit", "-qam", "side"]);
    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("a"), "main\n").unwrap();
    git(&dir, &["commit", "-qam", "main"]);
    dir
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("rgit", "git")
}

/// Run `args` in a fresh copy of the repo for git and for rgit, each after
/// `prep`, and return both outputs.
fn both(tag: &str, prep: impl Fn(&Path), args: &[&str]) -> (Output, Output) {
    let g = repo(&format!("{tag}-g"));
    prep(&g);
    let want = run("git", &g, args);
    let r = repo(&format!("{tag}-r"));
    prep(&r);
    let got = run(RGIT, &r, args);
    let _ = std::fs::remove_dir_all(&g);
    let _ = std::fs::remove_dir_all(&r);
    (want, got)
}

fn report(args: &[&str], want: &Output, got: &Output) -> String {
    format!(
        "{args:?}\n  git  ({:?}): {}{}\n  rgit ({:?}): {}{}",
        want.status.code(),
        text(&want.stdout),
        text(&want.stderr),
        got.status.code(),
        text(&got.stdout),
        text(&got.stderr)
    )
}

/// Same stderr, stdout and exit code as git for each case.
fn same(tag: &str, prep: impl Fn(&Path) + Copy, cases: &[&[&str]]) {
    let mut bad = Vec::new();
    for (i, args) in cases.iter().enumerate() {
        let (want, got) = both(&format!("{tag}{i}"), prep, args);
        if text(&want.stderr) != text(&got.stderr)
            || text(&want.stdout) != text(&got.stdout)
            || want.status.code() != got.status.code()
        {
            bad.push(report(args, &want, &got));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Same error line and exit code, followed by a usage line, for each case.
fn same_usage(tag: &str, cases: &[&[&str]]) {
    let mut bad = Vec::new();
    for (i, args) in cases.iter().enumerate() {
        let (want, got) = both(&format!("{tag}{i}"), |_| {}, args);
        let head = |o: &Output| {
            let t = text(&o.stderr);
            let mut lines = t.lines();
            let first = lines.next().unwrap_or("").to_owned();
            let usage = lines.next().is_some_and(|l| l.starts_with("usage: "));
            (first, usage, o.status.code())
        };
        if head(&want) != head(&got) {
            bad.push(report(args, &want, &got));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn unknown_revisions_and_paths_fail_like_git() {
    same(
        "rev",
        |_| {},
        &[
            &["log", "nope"],
            &["show", "nope"],
            &["diff", "nope"],
            &["reset", "nope"],
            &["rev-parse", "--verify", "nope"],
            &["cat-file", "-p", "nope"],
            &["tag", "v1", "nope"],
            &["branch", "x", "nope"],
            &["merge", "nope"],
            &["switch", "nope"],
            &["checkout", "nope"],
            &["branch", "-d", "nope"],
            &["add", "nope"],
            &["rm", "nope"],
            &["checkout", "--", "nope"],
            &["restore", "nope"],
            &["mv", "nope", "x"],
            &["blame", "nope"],
            &["stash", "pop"],
            &["config", "--get", "nope.x"],
            &["commit", "-m", "x"],
            &["diff", ":(bogus)a"],
            &["log", "--format=%s", ":(top)a"],
            &["log", "--format=%s", ":!a"],
            &["add", ":(glob,literal)a"],
            &["log", "@{u}"],
            &["rev-parse", "--verify", "@{u}"],
            &["diff", "@{u}"],
            &["reset", "--hard", "@{u}"],
            &["switch", "-d", "@{u}"],
            &["log", "nope@{upstream}"],
            &["show", "main@{9}"],
        ],
    );
}

/// `hook run` of an unknown event: git changed the wording after 2.50
/// ("cannot find a hook named X" -> "unknown hook event 'X'; use
/// --allow-unknown-hook-name ..."), so accept either, like the exit code
/// and empty stdout, as long as git and rgit agree.
#[test]
fn unknown_hook_name_fails_like_git() {
    let (want, got) = both("hook", |_| {}, &["hook", "run", "nope"]);
    let known = |o: &Output| {
        let e = text(&o.stderr);
        e == "error: cannot find a hook named nope\n"
            || e.starts_with("error: unknown hook event 'nope'")
    };
    assert!(
        known(&want)
            && known(&got)
            && want.status.code() == got.status.code()
            && want.stdout == got.stdout,
        "{}",
        report(&["hook", "run", "nope"], &want, &got)
    );
}

#[test]
fn local_changes_and_conflicts_fail_like_git() {
    let dirty = |d: &Path| std::fs::write(d.join("a"), "dirty\n").unwrap();
    same("dirty", dirty, &[&["checkout", "side"], &["merge", "side"]]);
    same("conflict", |_| {}, &[&["merge", "side"]]);
    let detached = |d: &Path| git(d, &["checkout", "-q", "HEAD~1"]);
    same("detached", detached, &[&["commit", "-m", "x"], &["pull"]]);
}

#[test]
fn outside_a_repository_and_bad_config_are_fatal() {
    let dir = std::env::temp_dir().join(format!("rgit-errors-{}-norepo", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for args in [&["status"][..], &["log"]] {
        let (want, got) = (run("git", &dir, args), run(RGIT, &dir, args));
        assert_eq!(text(&want.stderr), text(&got.stderr), "{args:?}");
        assert_eq!(want.status.code(), got.status.code(), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);

    let broken = |d: &Path| std::fs::write(d.join(".git/config"), "[bad\n").unwrap();
    same("config", broken, &[&["status"]]);
}

#[test]
fn bad_options_print_git_errors_and_usage() {
    same_usage(
        "opt",
        &[
            &["status", "--bogus"],
            &["commit", "--bogus"],
            &["branch", "-x"],
            &["diff", "--bogus"],
        ],
    );
    same(
        "opt-fatal",
        |_| {},
        &[&["log", "--bogus"], &["commit", "-m"], &["nosuchcommand"]],
    );
}

#[test]
fn dash_h_prints_usage_and_exits_129() {
    let dir = repo("dash-h");
    for cmd in ["status", "commit", "log", "stash", "branch"] {
        let got = run(RGIT, &dir, &[cmd, "-h"]);
        assert_eq!(got.status.code(), Some(129), "{cmd}");
        let out = String::from_utf8_lossy(&got.stdout);
        assert!(out.starts_with(&format!("usage: rgit {cmd}")), "{out}");
    }
    let help = run(RGIT, &dir, &["help", "status"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: rgit status"));
    let all = run(RGIT, &dir, &["help", "-a"]);
    assert!(String::from_utf8_lossy(&all.stdout).contains("   commit "));
    let version = run(RGIT, &dir, &["version"]);
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("rgit version "));
    let _ = std::fs::remove_dir_all(&dir);
}
