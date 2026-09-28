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
        out_of(dir, &["log", "--format=%T %an <%ae>%n%B"]),
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

fn out_of(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `git <args>` and `rgit <args>` in two copies of the same repo print the
/// same, succeed or fail together, and leave the same state.
fn same_out(tag: &str, setup: impl Fn(&Path), args: &[&str]) {
    both_ways(tag, setup, args, true);
}

/// Like [`same_out`], but rgit may word its output its own way.
fn same_res(tag: &str, setup: impl Fn(&Path), args: &[&str]) {
    both_ways(tag, setup, args, false);
}

fn both_ways(tag: &str, setup: impl Fn(&Path), args: &[&str], output: bool) {
    let (g, r) = (repo(&format!("{tag}-git")), repo(&format!("{tag}-rgit")));
    setup(&g);
    setup(&r);
    let gout = Command::new("git")
        .args(args)
        .current_dir(&g)
        .output()
        .unwrap();
    let (rout, success) = rgit(&r, &[&["--human"], args].concat());
    assert_eq!(success, gout.status.success(), "rgit {args:?}: {rout}");
    if output {
        assert_eq!(rout, String::from_utf8_lossy(&gout.stdout), "rgit {args:?}");
    }
    assert_eq!(state(&r), state(&g), "rgit {args:?}");
}

#[test]
fn commit_reuses_messages_authors_and_dates() {
    let by_x = |d: &Path| {
        git(
            d,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "--author",
                "Xavier <x@y>",
                "-m",
                "by x",
            ],
        );
        edit_a_b(d);
    };
    same(
        "reuse",
        by_x,
        &["commit", "-q", "-C", "HEAD"],
        &["commit", "-C", "HEAD"],
    );
    same(
        "reedit",
        by_x,
        &["commit", "-q", "-c", "HEAD", "--no-edit"],
        &["commit", "-c", "HEAD", "--no-edit"],
    );
    same(
        "reset-author",
        by_x,
        &["commit", "-q", "-C", "HEAD", "--reset-author"],
        &["commit", "-C", "HEAD", "--reset-author"],
    );
    same(
        "author-pattern",
        by_x,
        &["commit", "-q", "--author=xav", "-m", "m"],
        &["commit", "--author=xav", "-m", "m"],
    );
    same(
        "include",
        edit_a_b,
        &["commit", "-q", "-i", "a", "-m", "m"],
        &["commit", "-i", "a", "-m", "m"],
    );
    let date = |d: &Path| out_of(d, &["log", "-1", "--format=%ad", "--date=raw"]);
    for (tag, when) in [
        ("date-iso", "2020-01-02T03:04:05+0200"),
        ("date-unix", "@1234567890 -0130"),
    ] {
        let r = same(
            tag,
            edit_a_b,
            &["commit", "-q", "--date", when, "-m", "m"],
            &["commit", "--date", when, "-m", "m"],
        );
        let g = r.with_file_name(format!("rgit-wtops-{}-{tag}-git", std::process::id()));
        assert_eq!(date(&r), date(&g));
    }

    let dir = repo("commit-edit");
    edit_a_b(&dir);
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["commit", "-e", "-m", "draft"])
        .current_dir(&dir)
        .env("GIT_EDITOR", "printf 'edited\\n# gone\\n' >")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(git(&dir, &["log", "-1", "--format=%B"]), "edited\n\n");

    let (_, success) = rgit(&dir, &["commit", "--dry-run"]);
    assert!(!success, "nothing staged");
    git(&dir, &["add", "a"]);
    ok(&dir, &["commit", "--dry-run"]);
    assert_eq!(short(&dir), "M  a\n");
}

#[test]
fn commit_paths_runs_pre_commit_on_the_partial_index() {
    let hook = |d: &Path| {
        let path = d.join(".git/hooks/pre-commit");
        write(
            d,
            ".git/hooks/pre-commit",
            "#!/bin/sh\ngit diff --cached --name-only > .git/seen\n",
        );
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        edit_a_b(d);
    };
    let r = same(
        "hook-paths",
        hook,
        &["commit", "-q", "a", "-m", "a"],
        &["commit", "a", "-m", "a"],
    );
    let g = r.with_file_name(format!("rgit-wtops-{}-hook-paths-git", std::process::id()));
    assert_eq!(read(&r, ".git/seen"), "a\n");
    assert_eq!(read(&r, ".git/seen"), read(&g, ".git/seen"));
}

#[test]
fn add_dry_run_verbose_and_intent_to_add() {
    let mess = |d: &Path| {
        write(d, "a", "3\n");
        write(d, "d/new", "n\n");
        std::fs::remove_file(d.join("b")).unwrap();
    };
    same_out("add-n", mess, &["add", "-n", "."]);
    same_out("add-nu", mess, &["add", "-n", "-u"]);
    same_out("add-v", mess, &["add", "-v", "d", "a"]);
    same("add-N", mess, &["add", "-N", "d"], &["add", "-N", "d"]);
}

#[test]
fn rm_keeps_git_safety_rules() {
    let changed = |d: &Path| write(d, "a", "local\n");
    let staged = |d: &Path| {
        write(d, "a", "local\n");
        git(d, &["add", "a"]);
    };
    let both = |d: &Path| {
        staged(d);
        write(d, "a", "again\n");
    };
    let new = |d: &Path| {
        write(d, "n", "n\n");
        git(d, &["add", "n"]);
    };
    for (tag, setup) in [
        ("rm-changed", &changed as &dyn Fn(&Path)),
        ("rm-staged", &staged),
        ("rm-both", &both),
    ] {
        same_out(tag, setup, &["rm", "a"]);
        same_out(&format!("{tag}-cached"), setup, &["rm", "--cached", "a"]);
        same_out(&format!("{tag}-f"), setup, &["rm", "-f", "a"]);
    }
    same_out("rm-new", new, &["rm", "n"]);
    same_out("rm-new-cached", new, &["rm", "--cached", "n"]);
    same_out("rm-n", |_| {}, &["rm", "-n", "a", "b"]);
    same_out("rm-q", |_| {}, &["rm", "-q", "a"]);
    same_out("rm-r", |_| {}, &["rm", "-r", "d"]);
    same_out(
        "rm-unmatch",
        |_| {},
        &["rm", "--ignore-unmatch", "nope", "a"],
    );
    same_out("rm-nomatch", |_| {}, &["rm", "nope", "a"]);
}

#[test]
fn mv_dry_run_verbose_and_skip() {
    same_out("mv-n", |_| {}, &["mv", "-n", "a", "z"]);
    same_out("mv-v", |_| {}, &["mv", "-v", "a", "b", "d"]);
    same_res(
        "mv-k",
        |d| write(d, "u", "u\n"),
        &["mv", "-k", "u", "a", "d"],
    );
}

/// Adds `m`, then `feat` changes its first line, `side` and main change `a`
/// differently, and HEAD is back on main.
fn branches(d: &Path) {
    write(d, "m", "1\n2\n3\n4\n5\n6\n7\n8\n9\n");
    git(d, &["add", "m"]);
    git(d, &["commit", "-qm", "m"]);
    git(d, &["branch", "side"]);
    git(d, &["branch", "feat"]);
    write(d, "a", "main\n");
    git(d, &["commit", "-qam", "main a"]);
    git(d, &["switch", "-q", "feat"]);
    write(d, "m", "one\n2\n3\n4\n5\n6\n7\n8\n9\n");
    git(d, &["commit", "-qam", "feat m"]);
    git(d, &["switch", "-q", "side"]);
    write(d, "a", "side\n");
    git(d, &["commit", "-qam", "side a"]);
    git(d, &["switch", "-q", "main"]);
}

fn conflicted(d: &Path) {
    branches(d);
    let _ = out_of(d, &["merge", "side"]);
}

#[test]
fn checkout_force_merge_orphan_and_sides() {
    let local = |d: &Path| {
        branches(d);
        write(d, "a", "local\n");
        write(d, "m", "1\n2\n3\n4\n5\n6\n7\n8\nnine\n");
    };
    same_res("co-f", local, &["checkout", "-q", "-f", "feat"]);
    same_res(
        "sw-discard",
        local,
        &["switch", "-q", "--discard-changes", "feat"],
    );
    same_res("co-f-alone", local, &["checkout", "-q", "-f"]);
    let lines = |d: &Path| {
        branches(d);
        write(d, "m", "1\n2\n3\n4\n5\n6\n7\n8\nnine\n");
    };
    same_res("co-m", lines, &["checkout", "-q", "-m", "feat"]);
    same_res("sw-m", lines, &["switch", "-q", "-m", "feat"]);
    let clash = |d: &Path| {
        branches(d);
        write(d, "m", "uno\n2\n3\n4\n5\n6\n7\n8\n9\n");
    };
    let (g, r) = (repo("co-m-clash-git"), repo("co-m-clash-rgit"));
    clash(&g);
    clash(&r);
    let _ = out_of(&g, &["checkout", "-q", "-m", "feat"]);
    ok(&r, &["checkout", "-m", "feat"]);
    assert_eq!(short(&r), short(&g));
    assert_eq!(
        out_of(&r, &["ls-files", "-s"]),
        out_of(&g, &["ls-files", "-s"])
    );
    assert!(read(&r, "m").contains("<<<<<<< feat"), "{}", read(&r, "m"));

    same_res(
        "co-orphan",
        |_| {},
        &["checkout", "-q", "--orphan", "fresh"],
    );
    same_res(
        "co-orphan-start",
        |_| {},
        &["checkout", "-q", "--orphan", "fresh", "HEAD~1"],
    );
    same_res("sw-orphan", |_| {}, &["switch", "-q", "--orphan", "fresh"]);
    same_res("co-ours", conflicted, &["checkout", "--ours", "a"]);
    same_res(
        "co-theirs",
        conflicted,
        &["checkout", "--theirs", "--", "a"],
    );
    same_res("rs-ours", conflicted, &["restore", "--ours", "a"]);
    same_res("rs-theirs", conflicted, &["restore", "--theirs", "a"]);
}

#[test]
fn checkout_no_track_and_no_guess() {
    let src = repo("notrack-src");
    git(&src, &["branch", "other"]);
    let dst = src.with_file_name(format!("rgit-wtops-{}-notrack-dst", std::process::id()));
    let _ = std::fs::remove_dir_all(&dst);
    git(&src, &["clone", "-q", ".", dst.to_str().unwrap()]);
    let upstream = |b: &str| out_of(&dst, &["rev-parse", "--abbrev-ref", &format!("{b}@{{u}}")]);
    ok(
        &dst,
        &["checkout", "--no-track", "-b", "mine", "origin/other"],
    );
    assert_eq!(upstream("mine"), "");
    let (_, success) = rgit(&dst, &["switch", "--no-guess", "other"]);
    assert!(
        !success,
        "--no-guess must not create other from origin/other"
    );
    ok(&dst, &["switch", "--no-track", "other"]);
    assert_eq!(upstream("other"), "");
}

#[test]
fn reset_merge_and_quiet() {
    same_res("reset-merge-abort", conflicted, &["reset", "--merge"]);
    let kept = |d: &Path| {
        conflicted(d);
        write(d, "m", "local\n");
    };
    same_res("reset-merge-keeps", kept, &["reset", "--merge"]);
    let staged = |d: &Path| {
        write(d, "a", "3\n");
        git(d, &["add", "a"]);
        write(d, "d/c", "local\n");
    };
    same_res("reset-merge-rev", staged, &["reset", "--merge", "HEAD~1"]);
    let risky = |d: &Path| write(d, "a", "local\n");
    same_res("reset-merge-refuse", risky, &["reset", "--merge", "HEAD~1"]);
    same_out("reset-q", edit_a_b, &["reset", "-q", "HEAD~1"]);
}

#[test]
fn restore_source_removes_files_it_lacks() {
    let new = |d: &Path| {
        write(d, "n", "n\n");
        git(d, &["add", "n"]);
    };
    same_res("rs-src-new", new, &["restore", "--source=HEAD~1", "."]);
    same_res(
        "rs-src-new-sw",
        new,
        &["restore", "--source=HEAD~1", "-SW", "."],
    );
    same_res(
        "rs-overlay",
        new,
        &["restore", "--overlay", "--source=HEAD~1", "."],
    );
}

#[test]
fn deleted_paths_from_a_subfolder() {
    let dir = repo("sub-deleted");
    std::fs::remove_file(dir.join("d/c")).unwrap();
    ok(&dir.join("d"), &["checkout", "c"]);
    assert_eq!(read(&dir, "d/c"), "1\n");
    git(&dir, &["rm", "-q", "--cached", "d/c"]);
    std::fs::remove_file(dir.join("d/c")).unwrap();
    ok(&dir.join("d"), &["reset", "--", "c"]);
    assert_eq!(short(&dir), " D d/c\n");
}

#[test]
fn status_formats_match_git() {
    let dir = repo("st");
    edit_a_b(&dir);
    write(&dir, "u", "u\n");
    for args in [
        &["status", "--porcelain=v2", "-b"][..],
        &["status", "-v"],
        &["status", "-sb", "--no-ahead-behind"],
        &["status", "-sb", "--ahead-behind"],
    ] {
        assert_eq!(ok(&dir, args), git(&dir, args), "{args:?}");
    }
}

/// `git status <args>` and `rgit status <args>` print the same bytes in
/// `dir`, without the user's git config.
fn status_same(dir: &Path, args: &[&str]) {
    let run = |bin: &str, extra: &[&str]| {
        let out = Command::new(bin)
            .args(extra)
            .arg("status")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("RGIT_OPLOG", "0")
            .env("COLUMNS", "60")
            .output()
            .unwrap();
        (out.stdout, out.status.success())
    };
    let (g, gok) = run("git", &[]);
    let (r, rok) = run(env!("CARGO_BIN_EXE_rgit"), &["--human"]);
    assert_eq!(rok, gok, "status {args:?} in {dir:?}");
    assert_eq!(
        String::from_utf8_lossy(&r),
        String::from_utf8_lossy(&g),
        "status {args:?} in {dir:?}"
    );
}

const STATUS_FORMS: &[&[&str]] = &[
    &["--long"],
    &["-v"],
    &["-vv"],
    &["-s"],
    &["-sb"],
    &["--porcelain"],
    &["--porcelain", "-b", "-z"],
    &["--porcelain=v2", "-b", "--show-stash"],
    &["--porcelain=v2", "-z"],
    &["--long", "-uno"],
    &["--long", "-uall", "--ignored"],
    &["-s", "--ignored=matching"],
    &["-s", "--no-renames"],
    &["--long", "--no-ahead-behind"],
];

fn status_all_forms(dir: &Path) {
    for args in STATUS_FORMS {
        status_same(dir, args);
    }
}

#[test]
fn status_long_short_and_porcelain_match_git_in_many_states() {
    let dir = repo("st-mixed");
    write(&dir, "a", "3\n");
    git(&dir, &["add", "a"]);
    write(&dir, "a", "4\n");
    std::fs::remove_file(dir.join("d/c")).unwrap();
    git(&dir, &["mv", "b", "b2"]);
    write(&dir, "new", "n\n");
    git(&dir, &["add", "new"]);
    write(&dir, "sp ace", "s\n");
    std::fs::create_dir_all(dir.join("ud/x")).unwrap();
    write(&dir, "ud/x/z", "z\n");
    write(&dir, ".gitignore", "*.o\nig/\n");
    write(&dir, "x.o", "o\n");
    std::fs::create_dir_all(dir.join("ig")).unwrap();
    write(&dir, "ig/f", "f\n");
    write(&dir, "later", "l\n");
    git(&dir, &["add", "-N", "later"]);
    git(&dir, &["branch", "up", "HEAD~1"]);
    git(&dir, &["branch", "-u", "up"]);
    status_all_forms(&dir);
    status_all_forms(&dir.join("d"));
    status_same(&dir.join("d"), &["-s", ".."]);
    status_same(&dir, &["-s", "ud/x"]);

    let dir = repo("st-merge");
    conflicted(&dir);
    status_all_forms(&dir);

    let dir = repo("st-rebase");
    branches(&dir);
    git(&dir, &["switch", "-q", "side"]);
    let _ = out_of(&dir, &["rebase", "main"]);
    status_all_forms(&dir);

    let dir = repo("st-detached");
    git(&dir, &["checkout", "-q", "HEAD~1"]);
    write(&dir, "b", "x\n");
    status_all_forms(&dir);

    let dir = std::env::temp_dir().join(format!("rgit-wtops-{}-st-init", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    write(&dir, "a", "a\n");
    git(&dir, &["add", "a"]);
    write(&dir, "b", "b\n");
    status_all_forms(&dir);
}

#[test]
fn status_columns_and_color_match_git() {
    let dir = repo("st-col");
    for i in 0..25 {
        write(&dir, &format!("file{i}"), "x\n");
    }
    for col in [
        "--column",
        "--column=row",
        "--column=dense",
        "--column=plain",
    ] {
        status_same(&dir, &["--long", col]);
    }
    git(&dir, &["config", "color.status", "always"]);
    git(&dir, &["config", "color.ui", "always"]);
    write(&dir, "a", "x  \n");
    status_same(&dir, &["--long"]);
    status_same(&dir, &["-sb"]);
    status_same(&dir, &["-vv"]);
}

/// Run `git commit <args>` and `rgit commit <args>` in twin repos under an
/// editor that saves the template it was given; require the same output,
/// exit, template and resulting commit.
fn commit_same(tag: &str, setup: impl Fn(&Path), args: &[&str]) {
    let (g, r) = (repo(&format!("{tag}-git")), repo(&format!("{tag}-rgit")));
    let editor = g.with_file_name(format!("rgit-wtops-{}-{tag}-editor", std::process::id()));
    std::fs::write(&editor, "#!/bin/sh\ncp \"$1\" \"$1.seen\"\n").unwrap();
    std::fs::set_permissions(&editor, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let run = |dir: &Path, bin: &str, extra: &[&str]| {
        // The twins were made seconds apart; --amend shows HEAD's date.
        git(
            dir,
            &[
                "commit",
                "-q",
                "--amend",
                "--no-edit",
                "--date=2020-01-01T00:00:00Z",
            ],
        );
        setup(dir);
        let out = Command::new(bin)
            .args(extra)
            .arg("commit")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_EDITOR", &editor)
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
            .env("RGIT_OPLOG", "0")
            .output()
            .unwrap();
        let seen =
            std::fs::read_to_string(dir.join(".git/COMMIT_EDITMSG.seen")).unwrap_or_default();
        let dry = args
            .iter()
            .any(|a| ["--dry-run", "--short", "--porcelain", "--long", "-z"].contains(a));
        (
            if dry {
                String::from_utf8_lossy(&out.stdout).into_owned()
            } else {
                String::new()
            },
            out.status.success(),
            seen,
            [
                git(dir, &["status", "--short", "--branch"]),
                git(dir, &["ls-files", "--stage"]),
                git(dir, &["diff"]),
                // First parents only: twin merges may order their sides apart.
                out_of(dir, &["log", "--first-parent", "--format=%T %an <%ae>%n%B"]),
            ]
            .join("\n"),
        )
    };
    assert_eq!(
        run(&r, env!("CARGO_BIN_EXE_rgit"), &["--human"]),
        run(&g, "git", &[]),
        "commit {args:?}"
    );
}

#[test]
fn commit_dry_run_and_editor_template_match_git() {
    let mixed = |d: &Path| {
        write(d, "a", "3\n");
        git(d, &["add", "a"]);
        write(d, "b", "3\n");
        write(d, "u", "u\n");
        git(d, &["branch", "-q", "up", "HEAD~1"]);
        git(d, &["branch", "-q", "-u", "up"]);
    };
    for args in [
        &["--dry-run"][..],
        &["--dry-run", "-v"],
        &["--short"],
        &["--porcelain", "--branch"],
        &["-z"],
        &["--dry-run", "-a"],
        &["--dry-run", "b"],
        &["--dry-run", "--amend"],
        &["--dry-run", "-uno"],
        &["-e", "-m", "hello"],
        &["-e", "-v", "-m", "hello"],
        &["-e", "-vv", "-m", "hello"],
        &["-e", "--no-status", "-m", "hello"],
        &["-e", "-a", "-m", "hello"],
        &["-e", "--author=A U <a@u>", "-m", "hello"],
        &["--amend", "-e"],
    ] {
        let tag: String = args
            .concat()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        commit_same(&format!("tmpl-{tag}"), mixed, args);
    }
    commit_same("tmpl-clean", |_| {}, &["--dry-run"]);
    commit_same("tmpl-merge", conflicted, &["--dry-run"]);
    let merge = |d: &Path| {
        conflicted(d);
        write(d, "a", "fixed\n");
        git(d, &["add", "a"]);
    };
    commit_same("tmpl-merge-e", merge, &["-e"]);
    commit_same("tmpl-merge-no-edit", merge, &["--no-edit"]);
    let template = |d: &Path| {
        mixed(d);
        write(d, ".git/tmpl", "Template line\n\n# comment\n");
        git(d, &["config", "commit.template", ".git/tmpl"]);
        git(d, &["config", "commit.verbose", "true"]);
    };
    commit_same("tmpl-template", template, &["-e"]);
    commit_same("tmpl-template-m", template, &["-e", "-m", "x"]);
    let ita = |d: &Path| {
        write(d, "n", "n\n");
        git(d, &["add", "-N", "n"]);
        write(d, "a", "3\n");
        git(d, &["add", "a"]);
    };
    commit_same("ita", ita, &["-m", "x"]);
}

#[test]
fn checkout_merge_recreates_conflicts() {
    let edited = |d: &Path| {
        conflicted(d);
        write(d, "a", "junk\n");
    };
    let resolved = |d: &Path| {
        conflicted(d);
        write(d, "a", "fixed\n");
        git(d, &["add", "a"]);
    };
    same_res("co-m-paths", edited, &["checkout", "-m", "a"]);
    same_res("co-diff3", edited, &["checkout", "--conflict=diff3", "a"]);
    same_res(
        "co-zdiff3",
        edited,
        &["checkout", "--conflict=zdiff3", "--", "a"],
    );
    same_res("co-m-resolved", resolved, &["checkout", "-m", "a"]);
    same_res("rs-merge", edited, &["restore", "--merge", "a"]);
    same_res(
        "rs-conflict",
        resolved,
        &["restore", "--conflict=diff3", "a"],
    );
}

#[test]
fn add_chmod_and_renormalize() {
    same_res(
        "add-chmod",
        |d| write(d, "n", "x\n"),
        &["add", "--chmod=+x", "n", "a"],
    );
    same_res(
        "add-chmod-off",
        |d| {
            git(d, &["update-index", "--chmod=+x", "a"]);
        },
        &["add", "--chmod=-x", "a"],
    );
    let crlf = |d: &Path| {
        write(d, "w", "x\r\ny\r\n");
        git(d, &["add", "w"]);
        git(d, &["commit", "-qm", "w"]);
        write(d, ".gitattributes", "* text=auto\n");
    };
    same_res("add-renormalize", crlf, &["add", "--renormalize", "."]);
}

#[test]
fn add_edit_stages_the_edited_patch() {
    let editor = std::env::temp_dir().join(format!("rgit-wtops-{}-add-e", std::process::id()));
    // Keep the change to a, drop the one to b.
    std::fs::write(
        &editor,
        "#!/bin/sh\nawk '/^diff --git a\\/b/{skip=1} /^diff --git a\\/a/{skip=0} !skip' \"$1\" > \"$1.new\" && mv \"$1.new\" \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let run = |bin: &str, extra: &[&str], tag: &str| {
        let dir = repo(tag);
        write(&dir, "a", "3\n");
        write(&dir, "b", "3\n");
        let out = Command::new(bin)
            .args(extra)
            .args(["add", "-e"])
            .current_dir(&dir)
            .env("GIT_EDITOR", &editor)
            .env("RGIT_OPLOG", "0")
            .output()
            .unwrap();
        (out.status.success(), state(&dir))
    };
    assert_eq!(
        run(env!("CARGO_BIN_EXE_rgit"), &["--human"], "add-e-rgit"),
        run("git", &[], "add-e-git")
    );
}

#[test]
fn status_pairs_intent_to_add_renames_like_git() {
    let dir = repo("st-ita-rename");
    std::fs::rename(dir.join("a"), dir.join("moved")).unwrap();
    git(&dir, &["add", "-N", "moved"]);
    std::fs::rename(dir.join("d/c"), dir.join("d/c2")).unwrap();
    write(&dir, "d/c2", "1\nmore\n");
    git(&dir, &["add", "-N", "d/c2"]);
    write(&dir, "fresh", "unlike anything\n");
    git(&dir, &["add", "-N", "fresh"]);
    status_all_forms(&dir);
    status_same(&dir, &["--long", "--no-renames"]);
}

#[test]
fn status_submodule_summary_matches_git() {
    let dir = repo("st-sm");
    let lib = repo("st-sm-lib");
    let add = |path: &str| {
        let out = Command::new("git")
            .args(["-c", "protocol.file.allow=always", "-C"])
            .arg(&dir)
            .args(["submodule", "add", "-q"])
            .arg(&lib)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    add("sub");
    git(&dir, &["commit", "-qm", "add sub"]);
    add("sub2");
    let sub = dir.join("sub");
    for n in ["3", "4"] {
        write(&sub, "a", &format!("{n}\n"));
        git(&sub, &["commit", "-qam", &format!("lib {n}")]);
    }
    git(&dir, &["add", "sub"]);
    write(&sub, "a", "5\n");
    git(&sub, &["commit", "-qam", "lib 5"]);
    for value in ["true", "1", "false"] {
        git(&dir, &["config", "status.submoduleSummary", value]);
        status_same(&dir, &["--long"]);
        status_same(&dir, &["-v"]);
    }
    git(&dir, &["config", "status.submoduleSummary", "true"]);
    status_same(&dir, &["--long", "--ignore-submodules=all"]);
    git(&dir, &["config", "submodule.sub.ignore", "all"]);
    status_same(&dir, &["--long"]);
}
