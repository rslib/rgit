//! commit, reset, checkout, switch, restore and add take git's forms and
//! leave the repository as git would.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rgit(dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, success) = rgit(dir, args);
    assert!(success, "rgit {args:?}: {out}");
    out
}

fn short(dir: &Path) -> String {
    git(dir, &["status", "--short"])
}

fn write(dir: &Path, path: &str, text: &str) {
    std::fs::write(dir.join(path), text).unwrap();
}

fn read(dir: &Path, path: &str) -> String {
    std::fs::read_to_string(dir.join(path)).unwrap()
}

/// A repo with two commits on main: `a` and `b` go from 1 to 2.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-wtops-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("d")).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    for p in ["a", "b", "d/c"] {
        write(&dir, p, "1\n");
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "one"]);
    for p in ["a", "b"] {
        write(&dir, p, "2\n");
    }
    git(&dir, &["commit", "-qam", "two"]);
    dir
}

/// Everything a command can change, minus commit ids and dates: branch and
/// upstream, index blobs, unstaged patch, untracked files, and each commit's
/// tree, author and message.
fn state(dir: &Path) -> String {
    [
        git(dir, &["status", "--short", "--branch"]),
        git(dir, &["ls-files", "--stage"]),
        git(dir, &["diff"]),
        git(dir, &["log", "--format=%T %an <%ae>%n%B"]),
    ]
    .join("\n")
}

/// Run `git <args>` in one fresh repo and `rgit <rargs>` in another, after
/// the same `setup`, and require the same result.
fn same(tag: &str, setup: impl Fn(&Path), args: &[&str], rargs: &[&str]) -> PathBuf {
    let (g, r) = (repo(&format!("{tag}-git")), repo(&format!("{tag}-rgit")));
    setup(&g);
    setup(&r);
    git(&g, args);
    ok(&r, rargs);
    assert_eq!(state(&r), state(&g), "rgit {rargs:?} vs git {args:?}");
    r
}

fn edit_a_b(dir: &Path) {
    write(dir, "a", "3\n");
    write(dir, "b", "3\n");
    git(dir, &["add", "b"]);
}

#[test]
fn commit_takes_git_message_author_and_path_forms() {
    let none = |_: &Path| {};
    same(
        "paras",
        edit_a_b,
        &["commit", "-q", "-m", "subject", "-m", "body line"],
        &["commit", "-m", "subject", "-m", "body line"],
    );
    same(
        "signoff",
        edit_a_b,
        &["commit", "-q", "-s", "-m", "x"],
        &["commit", "-s", "-m", "x"],
    );
    same(
        "author",
        edit_a_b,
        &["commit", "-q", "--author", "A U <a@u>", "-m", "x"],
        &["commit", "--author", "A U <a@u>", "-m", "x"],
    );
    same(
        "empty",
        none,
        &["commit", "-q", "--allow-empty", "-m", "e"],
        &["commit", "--allow-empty", "-m", "e"],
    );
    same(
        "only",
        edit_a_b,
        &["commit", "-q", "a", "-m", "only a"],
        &["commit", "a", "-m", "only a"],
    );
    same(
        "fixup",
        edit_a_b,
        &["commit", "-q", "--fixup", "HEAD~1"],
        &["commit", "--fixup", "HEAD~1"],
    );
    same(
        "file",
        |d| write(d, "msg.txt", "from a file\n\nwith a body\n"),
        &["commit", "-q", "--allow-empty", "-F", "msg.txt"],
        &["commit", "--allow-empty", "-F", "msg.txt"],
    );
    same(
        "no-edit",
        edit_a_b,
        &["commit", "-q", "--amend", "--no-edit"],
        &["commit", "--amend", "--no-edit"],
    );

    let dir = repo("only-untracked");
    write(&dir, "new", "n\n");
    let (out, success) = rgit(&dir, &["commit", "new", "-m", "x"]);
    assert!(
        !success && out.contains("did not match any file(s) known to git"),
        "{out}"
    );
}

#[test]
fn reset_takes_paths_without_dashes_mixed_and_keep() {
    same(
        "reset-path",
        edit_a_b,
        &["reset", "-q", "b"],
        &["reset", "b"],
    );
    same(
        "reset-mixed",
        edit_a_b,
        &["reset", "-q", "--mixed", "HEAD~1"],
        &["reset", "--mixed", "HEAD~1"],
    );
    let local = |d: &Path| write(d, "d/c", "local\n");
    same(
        "reset-keep",
        local,
        &["reset", "-q", "--keep", "HEAD~1"],
        &["reset", "--keep", "HEAD~1"],
    );

    let dir = repo("reset-keep-refuses");
    write(&dir, "a", "local\n");
    let (_, success) = rgit(&dir, &["reset", "--keep", "HEAD~1"]);
    assert!(!success, "keep must not overwrite a local change to a");
    assert_eq!(read(&dir, "a"), "local\n");
    assert_eq!(git(&dir, &["log", "--format=%s", "-1"]), "two\n");
}

#[test]
fn checkout_restores_paths_and_switches_like_git() {
    same(
        "co-rev-paths",
        edit_a_b,
        &["checkout", "-q", "HEAD~1", "--", "a", "b"],
        &["checkout", "HEAD~1", "--", "a", "b"],
    );
    same(
        "co-index",
        edit_a_b,
        &["checkout", "-q", "--", "a"],
        &["checkout", "--", "a"],
    );
    same(
        "co-bare-path",
        edit_a_b,
        &["checkout", "-q", "a"],
        &["checkout", "a"],
    );
    same(
        "co-rev-path",
        edit_a_b,
        &["checkout", "-q", "HEAD~1", "a"],
        &["checkout", "HEAD~1", "a"],
    );
    same(
        "co-dot",
        edit_a_b,
        &["checkout", "-q", "."],
        &["checkout", "."],
    );
    same(
        "co-detach",
        |_| {},
        &["checkout", "-q", "--detach"],
        &["checkout", "--detach"],
    );
    let feat = |d: &Path| git(d, &["branch", "feat", "HEAD~1"]).truncate(0);
    same(
        "co-B",
        feat,
        &["checkout", "-q", "-B", "feat"],
        &["checkout", "-B", "feat"],
    );
    same(
        "co-prev",
        |d| git(d, &["checkout", "-q", "-b", "feat"]).truncate(0),
        &["checkout", "-q", "-"],
        &["checkout", "-"],
    );

    let dir = repo("co-sub");
    write(&dir, "d/c", "changed\n");
    ok(&dir.join("d"), &["checkout", "c"]);
    assert_eq!(read(&dir, "d/c"), "1\n");

    let (out, success) = rgit(&dir, &["checkout", "nope"]);
    assert!(!success && out.contains("did not match"), "{out}");
}

#[test]
fn switch_creates_detaches_and_goes_back() {
    same(
        "sw-c",
        |_| {},
        &["switch", "-q", "-c", "new", "HEAD~1"],
        &["switch", "-c", "new", "HEAD~1"],
    );
    let feat = |d: &Path| git(d, &["branch", "feat", "HEAD~1"]).truncate(0);
    same(
        "sw-C",
        feat,
        &["switch", "-q", "-C", "feat"],
        &["switch", "-C", "feat"],
    );
    same(
        "sw-branch",
        feat,
        &["switch", "-q", "feat"],
        &["switch", "feat"],
    );
    same(
        "sw-detach",
        |_| {},
        &["switch", "-q", "--detach", "HEAD~1"],
        &["switch", "--detach", "HEAD~1"],
    );
    let away = |d: &Path| {
        git(d, &["switch", "-q", "-c", "feat"]);
        git(d, &["switch", "-q", "main"]);
    };
    let dir = same("sw-prev", away, &["switch", "-q", "-"], &["switch", "-"]);
    assert_eq!(git(&dir, &["branch", "--show-current"]), "feat\n");

    let (out, success) = rgit(&dir, &["switch", "HEAD~1"]);
    assert!(!success && out.contains("a branch is expected"), "{out}");
}

#[test]
fn switch_and_checkout_track_remote_branches() {
    let src = repo("track-src");
    git(&src, &["branch", "feat", "HEAD~1"]);
    git(&src, &["branch", "other"]);
    let clone = |tag: &str| {
        let dst = src.with_file_name(format!("rgit-wtops-{}-track-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dst);
        git(&src, &["clone", "-q", ".", dst.to_str().unwrap()]);
        dst
    };
    let upstream = |d: &Path, b: &str| {
        git(
            d,
            &["rev-parse", "--abbrev-ref", &format!("{b}@{{upstream}}")],
        )
    };

    let (g, r) = (clone("git"), clone("rgit"));
    git(&g, &["checkout", "-q", "feat"]);
    ok(&r, &["checkout", "feat"]);
    assert_eq!(state(&r), state(&g));
    assert_eq!(upstream(&r, "feat"), "origin/feat\n");

    let r = clone("switch");
    ok(&r, &["switch", "feat"]);
    assert_eq!(upstream(&r, "feat"), "origin/feat\n");
    ok(&r, &["switch", "-c", "mine", "origin/other"]);
    assert_eq!(upstream(&r, "mine"), "origin/other\n");
    ok(&r, &["checkout", "-t", "origin/other"]);
    assert_eq!(upstream(&r, "other"), "origin/other\n");
    assert_eq!(git(&r, &["branch", "--show-current"]), "other\n");
}

#[test]
fn restore_takes_staged_worktree_and_source() {
    same(
        "rs-wt",
        edit_a_b,
        &["restore", "a", "b"],
        &["restore", "a", "b"],
    );
    same(
        "rs-staged",
        edit_a_b,
        &["restore", "--staged", "b"],
        &["restore", "--staged", "b"],
    );
    same(
        "rs-both",
        edit_a_b,
        &["restore", "-SW", "b"],
        &["restore", "-SW", "b"],
    );
    same(
        "rs-source",
        edit_a_b,
        &["restore", "--source", "HEAD~1", "a", "b"],
        &["restore", "--source", "HEAD~1", "a", "b"],
    );
    same(
        "rs-source-staged",
        edit_a_b,
        &["restore", "--source=HEAD~1", "--staged", "--worktree", "b"],
        &["restore", "--source=HEAD~1", "--staged", "--worktree", "b"],
    );
    same(
        "rs-dot",
        edit_a_b,
        &["restore", "--staged", "."],
        &["restore", "--staged", "."],
    );
    same(
        "co-rev-dot",
        edit_a_b,
        &["checkout", "-q", "HEAD~1", "--", "."],
        &["checkout", "HEAD~1", "--", "."],
    );
    same(
        "commit-dot",
        edit_a_b,
        &["commit", "-q", ".", "-m", "all"],
        &["commit", ".", "-m", "all"],
    );
    same(
        "reset-dot",
        edit_a_b,
        &["reset", "-q", "."],
        &["reset", "."],
    );
    same("stage-dot", edit_a_b, &["add", "."], &["stage", "."]);
    same(
        "discard-dot",
        edit_a_b,
        &["checkout", "-q", "."],
        &["discard", "."],
    );
    let dir = repo("rs-none");
    let (out, success) = rgit(&dir, &["restore", "nope"]);
    assert!(!success && out.contains("did not match"), "{out}");
}

#[test]
fn add_takes_git_forms() {
    let mess = |d: &Path| {
        write(d, "a", "3\n");
        write(d, "d/new", "n\n");
        write(d, "top.md", "m\n");
        write(d, ".gitignore", "*.log\n");
        write(d, "x.log", "x\n");
        std::fs::remove_file(d.join("b")).unwrap();
    };
    same("add-dot", mess, &["add", "."], &["add", "."]);
    same("add-A", mess, &["add", "-A"], &["add", "-A"]);
    same("add-u", mess, &["add", "-u"], &["add", "-u"]);
    same(
        "add-paths",
        mess,
        &["add", "d", "*.md", "b"],
        &["add", "d", "*.md", "b"],
    );
    same(
        "add-force",
        mess,
        &["add", "-f", "x.log"],
        &["add", "-f", "x.log"],
    );

    let dir = repo("add-sub");
    mess(&dir);
    ok(&dir.join("d"), &["add", "."]);
    assert_eq!(
        short(&dir),
        " M a\n D b\nA  d/new\n?? .gitignore\n?? top.md\n"
    );

    let (_, success) = rgit(&dir, &["add", "x.log"]);
    assert!(!success, "an ignored file needs -f");
    let (out, success) = rgit(&dir, &["add"]);
    assert!(!success && out.contains("nothing specified"), "{out}");
}

#[test]
fn reset_rev_paths_resets_the_index_from_rev() {
    let dir = repo("reset-rev");
    ok(&dir, &["reset", "HEAD~1", "--", "a"]);
    assert_eq!(short(&dir), "MM a\n");
    assert_eq!(git(&dir, &["show", ":a"]), "1\n");
    assert_eq!(read(&dir, "a"), "2\n");
    assert_eq!(git(&dir, &["log", "--format=%s", "-1"]), "two\n");
}
