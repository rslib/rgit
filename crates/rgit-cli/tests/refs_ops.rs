//! branch, tag, stash, remote and worktree take git's forms and give git's
//! results.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
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

fn rgit(dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

fn fails(dir: &Path, args: &[&str]) -> String {
    let (out, success) = rgit(dir, args);
    assert!(!success, "rgit {args:?} should fail: {out}");
    out
}

/// A repo on `main` with `a`, `b` and `dir/c` committed.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-refs-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dir")).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    for p in ["a", "b", "dir/c"] {
        std::fs::write(dir.join(p), format!("{p}\n")).unwrap();
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

fn short(dir: &Path) -> String {
    git(dir, &["status", "--short"])
}

#[test]
fn stash_push_stashes_only_the_given_paths() {
    let dir = repo("stash-paths");
    for p in ["a", "b", "dir/c"] {
        std::fs::write(dir.join(p), "changed\n").unwrap();
    }
    ok(&dir, &["stash", "push", "-m", "only a", "--", "a"]);
    assert_eq!(short(&dir), " M b\n M dir/c\n");
    assert_eq!(
        git(&dir, &["stash", "list", "--format=%gs"]),
        "On main: only a\n"
    );
    assert_eq!(git(&dir, &["stash", "show", "--name-only"]), "a\n");

    ok(&dir.join("dir"), &["stash", "push", "c"]);
    assert_eq!(short(&dir), " M b\n");
    assert_eq!(git(&dir, &["stash", "show", "--name-only"]), "dir/c\n");

    let out = fails(&dir, &["stash", "push", "nope"]);
    assert!(out.contains("nope"), "{out}");
}

#[test]
fn stash_takes_git_flags_and_stash_refs() {
    let dir = repo("stash-forms");
    std::fs::write(dir.join("new"), "n\n").unwrap();
    ok(&dir, &["stash", "-u", "-m", "untracked"]);
    assert_eq!(short(&dir), "");
    std::fs::write(dir.join("b"), "b2\n").unwrap();
    std::fs::write(dir.join("a"), "a2\n").unwrap();
    ok(&dir, &["stash", "--", "b"]);
    assert_eq!(short(&dir), " M a\n");

    git(&dir, &["add", "a"]);
    ok(&dir, &["stash", "push", "-k"]);
    assert_eq!(short(&dir), "M  a\n");
    git(&dir, &["reset", "-q", "--hard"]);

    let names = |s: &str| {
        (
            ok(&dir, &["--human", "stash", "show", s, "--name-only"]),
            git(&dir, &["stash", "show", "--name-only", s]),
        )
    };
    let (ours, theirs) = names("stash@{1}");
    assert_eq!(ours.trim(), theirs.trim());
    assert_eq!(ours.trim(), "b");
    let patch = ok(&dir, &["--human", "stash", "show", "-p", "1"]);
    assert!(patch.contains("+b2"), "{patch}");

    ok(&dir, &["stash", "drop", "stash@{0}"]);
    ok(&dir, &["stash", "pop", "stash@{1}"]);
    assert_eq!(short(&dir), "?? new\n");
    assert_eq!(
        git(&dir, &["stash", "list", "--format=%gs"]),
        format!("WIP on main: {} init\n", &rev(&dir, "HEAD")[..7])
    );
    git(&dir, &["clean", "-qfd"]);

    std::fs::write(dir.join("a"), "staged\n").unwrap();
    git(&dir, &["add", "a"]);
    git(&dir, &["stash", "-q"]);
    ok(&dir, &["stash", "apply", "--index"]);
    assert_eq!(short(&dir), "M  a\n");
    git(&dir, &["reset", "-q", "--hard"]);

    commit(&dir, "c.txt", "moved on");
    ok(&dir, &["stash", "branch", "from-stash", "stash@{0}"]);
    assert_eq!(git(&dir, &["branch", "--show-current"]), "from-stash\n");
    assert_eq!(rev(&dir, "HEAD"), rev(&dir, "main~1"));
    assert_eq!(short(&dir), "M  a\n");
    assert_eq!(git(&dir, &["stash", "list"]).lines().count(), 1);

    ok(&dir, &["stash", "clear"]);
    assert_eq!(git(&dir, &["stash", "list"]), "");

    let out = fails(&dir, &["stash", "save", "x"]);
    assert!(out.contains("rgit stash push -m"), "{out}");
}

/// Two identical repos with `a` staged and `b` and `dir/c` changed, one for
/// git and one for rgit.
fn twins(tag: &str) -> (PathBuf, PathBuf) {
    let make = |t: &str| {
        let dir = repo(t);
        std::fs::write(dir.join("a"), "a2\n").unwrap();
        git(&dir, &["add", "a"]);
        std::fs::write(dir.join("b"), "b2\n").unwrap();
        std::fs::write(dir.join("dir/c"), "c2\n").unwrap();
        dir
    };
    (make(&format!("{tag}-git")), make(&format!("{tag}-rgit")))
}

fn stash_state(dir: &Path) -> (String, String, String) {
    (
        short(dir),
        git(dir, &["stash", "list", "--format=%gs"]),
        git(dir, &["stash", "show", "-p", "--format="]),
    )
}

#[test]
fn stash_push_takes_staged_pathspec_files_and_parts() {
    let (g, r) = twins("stash-staged");
    git(&g, &["stash", "push", "--staged", "-m", "only staged"]);
    ok(&r, &["stash", "push", "--staged", "-m", "only staged"]);
    assert_eq!(stash_state(&r), stash_state(&g));
    let out = fails(&r, &["stash", "-S"]);
    assert!(out.contains("no staged changes"), "{out}");

    let (g, r) = twins("stash-from-file");
    for d in [&g, &r] {
        std::fs::write(d.join("list"), "b\ndir/c\n").unwrap();
    }
    git(&g, &["stash", "push", "--pathspec-from-file=list"]);
    ok(&r, &["stash", "push", "--pathspec-from-file", "list"]);
    assert_eq!(stash_state(&r), stash_state(&g));

    let out = fails(&r, &["stash", "push", "-p"]);
    assert!(out.contains("terminal"), "{out}");
}

#[cfg(target_os = "macos")]
#[test]
fn stash_push_patch_picks_hunks_on_a_terminal() {
    let dir = repo("stash-patch");
    let lines: String = (1..=30).map(|n| format!("{n}\n")).collect();
    std::fs::write(dir.join("a"), &lines).unwrap();
    git(&dir, &["commit", "-qam", "thirty"]);
    std::fs::write(
        dir.join("a"),
        lines
            .replace("\n2\n", "\ntwo\n")
            .replace("25\n", "twentyfive\n"),
    )
    .unwrap();
    std::fs::write(dir.join("b"), "b2\n").unwrap();
    let out = Command::new("script")
        .args(["-q", "/dev/null", env!("CARGO_BIN_EXE_rgit")])
        .args(["stash", "push", "-p", "-m", "picked"])
        .current_dir(&dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            let mut stdin = c.stdin.take().unwrap();
            for answer in ["y", "n", "y"] {
                std::thread::sleep(std::time::Duration::from_millis(300));
                writeln!(stdin, "{answer}")?;
            }
            c.wait_with_output()
        })
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(short(&dir), " M a\n");
    assert!(git(&dir, &["diff"]).contains("+twentyfive"));
    assert_eq!(
        git(&dir, &["stash", "show", "--name-only", "--format="]),
        "a\nb\n"
    );
    assert!(git(&dir, &["stash", "show", "-p"]).contains("+two"));
    assert_eq!(
        git(&dir, &["stash", "list", "--format=%gs"]),
        "On main: picked\n"
    );
}

#[test]
fn stash_creates_stores_and_shows_untracked_like_git() {
    let (g, r) = twins("stash-create");
    let made = ok(&r, &["--human", "stash", "create", "my", "msg"]);
    let theirs = git(&g, &["stash", "create", "my msg"]);
    let tree = |d: &Path, id: &str| {
        [
            rev(d, &format!("{}^{{tree}}", id.trim())),
            rev(d, &format!("{}^2^{{tree}}", id.trim())),
            git(d, &["log", "-1", "--format=%s", id.trim()]),
        ]
    };
    assert_eq!(tree(&r, &made), tree(&g, &theirs));
    assert_eq!(short(&r), short(&g));

    ok(&r, &["stash", "store", made.trim()]);
    git(&g, &["stash", "store", theirs.trim()]);
    assert_eq!(stash_state(&r), stash_state(&g));
    std::fs::write(r.join("b"), "b4\n").unwrap();
    let made = ok(&r, &["--human", "stash", "create"]);
    ok(&r, &["stash", "store", "-m", "named", made.trim()]);
    assert_eq!(
        git(&r, &["stash", "list", "--format=%gs"]).lines().next(),
        Some("named")
    );
    fails(&r, &["stash", "store", "HEAD"]);
    git(&r, &["stash", "clear"]);
    git(&r, &["reset", "-q", "--hard"]);
    assert_eq!(ok(&r, &["--human", "stash", "create"]).trim(), "");

    std::fs::write(r.join("b"), "b3\n").unwrap();
    std::fs::write(r.join("new"), "n\n").unwrap();
    git(&r, &["stash", "push", "-u", "-m", "with untracked"]);
    std::fs::write(r.join("a"), "a3\n").unwrap();
    git(&r, &["stash", "push", "-m", "plain"]);
    for args in [
        &["stash", "show", "-u", "--name-only", "stash@{1}"][..],
        &[
            "stash",
            "show",
            "--only-untracked",
            "--name-only",
            "stash@{1}",
        ],
        &["stash", "show", "-u", "--name-only"],
        &["stash", "list", "--format=%gd %gs %h %s %an", "-n", "1"],
        &["stash", "list", "--format=%gd%n%H"],
        &["stash", "list", "-n", "1"],
        &["stash", "show", "--name-status", "stash@{1}"],
        &["stash", "show", "--numstat"],
    ] {
        same(&r, args);
    }
}

#[test]
fn tag_delete_removes_every_named_tag() {
    let dir = repo("tag-delete");
    for t in ["a", "b", "c"] {
        git(&dir, &["tag", t]);
    }
    ok(&dir, &["tag", "-d", "a", "b"]);
    assert_eq!(git(&dir, &["tag"]), "c\n");

    let out = fails(&dir, &["tag", "-d", "c", "nope"]);
    assert!(out.contains("nope"), "{out}");
    assert_eq!(git(&dir, &["tag"]), "");

    let out = ok(&dir, &["tag", "-d", "gone", "--toon"]);
    assert!(out.contains("no-op"), "{out}");
    fails(&dir, &["tag", "x", "y", "z"]);
}

fn commit(dir: &Path, file: &str, msg: &str) {
    std::fs::write(dir.join(file), format!("{msg}\n")).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", msg]);
}

fn rev(dir: &Path, rev: &str) -> String {
    git(dir, &["rev-parse", rev]).trim().to_owned()
}

fn sorted(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text.lines().collect();
    v.sort_unstable();
    v
}

#[test]
fn tag_creates_at_a_rev_lists_patterns_and_filters() {
    let dir = repo("tag-forms");
    commit(&dir, "a", "second");
    ok(&dir, &["tag", "v1", "HEAD~1"]);
    assert_eq!(rev(&dir, "v1"), rev(&dir, "HEAD~1"));
    ok(&dir, &["tag", "-a", "v2", "-m", "two\n\nbody", "HEAD"]);
    assert_eq!(git(&dir, &["cat-file", "-t", "v2"]), "tag\n");
    ok(&dir, &["tag", "other"]);

    ok(&dir, &["tag", "-f", "v1", "HEAD"]);
    assert_eq!(rev(&dir, "v1"), rev(&dir, "HEAD"));
    fails(&dir, &["tag", "v1", "HEAD~1"]);
    fails(&dir, &["tag", "-a", "v3"]);

    let out = ok(&dir, &["--human", "tag", "-l", "v*"]);
    assert_eq!(sorted(&out), ["v1", "v2"]);
    assert_eq!(git(&dir, &["tag", "-l", "v*"]), "v1\nv2\n");
    let out = ok(&dir, &["--human", "tag", "-n"]);
    assert!(out.lines().any(|l| l == "v2              two"), "{out}");

    ok(&dir, &["tag", "old", "HEAD~1"]);
    let out = ok(&dir, &["--human", "tag", "--contains", "HEAD"]);
    assert_eq!(
        sorted(&out),
        sorted(&git(&dir, &["tag", "--contains", "HEAD"]))
    );
    let out = ok(&dir, &["--human", "tag", "--points-at", "HEAD~1"]);
    assert_eq!(out.trim(), "old");
    let out = ok(&dir, &["tag", "-l", "o*", "--toon"]);
    assert!(
        out.contains("old") && out.contains("other") && !out.contains("v1"),
        "{out}"
    );
}

#[test]
fn tag_lists_sorts_formats_and_annotates_like_git() {
    let dir = repo("tag-list");
    git(&dir, &["tag", "-a", "v1.10", "-m", "ten\n\nbody\nmore"]);
    git(&dir, &["tag", "V1.9"]);
    commit(&dir, "a", "second\n\nsecond body");
    git(&dir, &["tag", "v1.2"]);
    git(&dir, &["tag", "-a", "v1.1", "-m", "one"]);
    git(&dir, &["tag", "alpha"]);
    for args in [
        &["tag"][..],
        &["tag", "-l", "v*"],
        &["tag", "-i", "-l", "v*"],
        &["tag", "--sort=version:refname"],
        &["tag", "--sort=-v:refname", "-i"],
        &["tag", "--sort=-creatordate", "--sort=refname"],
        &["tag", "-n3"],
        &["tag", "-n2", "-l", "v1.1*"],
        &["tag", "--format=%(refname:short) %(objecttype) %(subject)"],
        &["tag", "--merged", "HEAD~1"],
        &["tag", "--no-merged", "HEAD~1"],
        &["tag", "--no-contains", "HEAD"],
        &["tag", "--points-at", "HEAD"],
        &["tag", "--column", "--no-column"],
    ] {
        same(&dir, args);
    }
    let out = ok(&dir, &["--human", "tag", "-n", "3", "-l", "v1.10"]);
    assert_eq!(
        out.trim_end(),
        git(&dir, &["tag", "-n3", "-l", "v1.10"]).trim_end()
    );

    let body = |t: &str| {
        let raw = git(&dir, &["cat-file", "-p", t]);
        raw.split_once("\n\n").unwrap().1.to_owned()
    };
    std::fs::write(dir.join("msg"), "from file\n# dropped\n\n\nend  \n").unwrap();
    ok(&dir, &["tag", "-F", "msg", "f1"]);
    git(&dir, &["tag", "-F", "msg", "f2"]);
    assert_eq!(body("f1"), body("f2"));
    ok(&dir, &["tag", "-F", "msg", "--cleanup=whitespace", "f3"]);
    git(&dir, &["tag", "-F", "msg", "--cleanup=whitespace", "f4"]);
    assert_eq!(body("f3"), body("f4"));
    ok(
        &dir,
        &["tag", "-m", " keep\n# this ", "--cleanup=verbatim", "f5"],
    );
    git(
        &dir,
        &["tag", "-m", " keep\n# this ", "--cleanup=verbatim", "f6"],
    );
    assert_eq!(body("f5"), body("f6"));

    let out = fails(&dir, &["tag", "-v", "v1.1"]);
    assert!(out.contains("no signature found"), "{out}");
    git(&dir, &["config", "gpg.program", "false"]);
    let out = fails(&dir, &["tag", "-s", "-m", "signed", "s1"]);
    assert!(out.contains("gpg failed to sign"), "{out}");
    git(&dir, &["config", "tag.gpgSign", "true"]);
    fails(&dir, &["tag", "-m", "signed", "s2"]);
    ok(&dir, &["tag", "--no-sign", "-m", "unsigned", "s3"]);
    ok(&dir, &["tag", "light"]);
    assert_eq!(git(&dir, &["cat-file", "-t", "light"]), "commit\n");
}

/// Branch names from `rgit --human branch ...` (dropping the `* ` mark).
fn branch_names(dir: &Path, args: &[&str]) -> Vec<String> {
    let mut all = vec!["--human", "branch"];
    all.extend(args);
    let mut v: Vec<String> = ok(dir, &all).lines().map(|l| l[2..].to_owned()).collect();
    v.sort_unstable();
    v
}

fn git_branches(dir: &Path, args: &[&str]) -> Vec<String> {
    let mut all = vec!["branch"];
    all.extend(args);
    let mut v: Vec<String> = git(dir, &all).lines().map(|l| l[2..].to_owned()).collect();
    v.sort_unstable();
    v
}

/// `dir` with a bare `origin` holding its `main`.
fn with_origin(dir: &Path) {
    let remote = dir.with_extension("origin.git");
    let _ = std::fs::remove_dir_all(&remote);
    git(
        dir,
        &["clone", "-q", "--bare", ".", remote.to_str().unwrap()],
    );
    git(dir, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(dir, &["fetch", "-q", "origin"]);
}

#[test]
fn branch_takes_git_forms() {
    let dir = repo("branch-forms");
    commit(&dir, "a", "second");
    with_origin(&dir);

    ok(&dir, &["branch", "old", "HEAD~1"]);
    ok(&dir, &["branch", "feat"]);
    assert_eq!(rev(&dir, "old"), rev(&dir, "HEAD~1"));
    assert_eq!(rev(&dir, "feat"), rev(&dir, "HEAD"));
    assert_eq!(
        ok(&dir, &["--human", "branch", "--show-current"]).trim(),
        "main"
    );
    let out = ok(&dir, &["branch", "feat", "--toon"]);
    assert!(out.contains("no-op"), "{out}");
    fails(&dir, &["--human", "branch", "feat"]);

    for filter in [
        &["--merged", "HEAD~1"][..],
        &["--no-merged", "HEAD~1"],
        &["--contains", "HEAD"],
        &["-l", "f*"],
        &["-a"],
        &["-r"],
    ] {
        assert_eq!(
            branch_names(&dir, filter),
            git_branches(&dir, filter),
            "{filter:?}"
        );
    }

    ok(&dir, &["branch", "-u", "origin/main", "feat"]);
    assert_eq!(
        git(&dir, &["config", "branch.feat.merge"]).trim(),
        "refs/heads/main"
    );
    let out = ok(&dir, &["--human", "branch", "-vv"]);
    let line = out.lines().find(|l| l.contains("feat")).unwrap();
    assert!(line.contains("[origin/main] second"), "{out}");
    commit(&dir, "b", "third");
    ok(&dir, &["--human", "branch", "-u", "origin/main"]);
    let out = ok(&dir, &["--human", "branch", "-v"]);
    assert!(out.contains("[ahead 1] third"), "{out}");
    ok(&dir, &["branch", "--unset-upstream", "feat"]);
    assert!(!git(&dir, &["config", "--list"]).contains("branch.feat.merge"));

    ok(&dir, &["branch", "track", "origin/main"]);
    assert_eq!(
        git(&dir, &["config", "branch.track.merge"]).trim(),
        "refs/heads/main"
    );

    ok(&dir, &["branch", "-m", "feat", "feature"]);
    ok(&dir, &["branch", "-M", "feature", "old"]);
    assert_eq!(rev(&dir, "old"), rev(&dir, "HEAD~1"));
    ok(&dir, &["branch", "-c", "old", "copy"]);
    assert_eq!(rev(&dir, "copy"), rev(&dir, "old"));
    ok(&dir, &["branch", "-d", "copy", "old", "track"]);
    assert_eq!(git_branches(&dir, &[]), ["main"]);

    git(&dir, &["checkout", "-q", "-b", "side"]);
    commit(&dir, "c", "side");
    git(&dir, &["checkout", "-q", "main"]);
    let out = fails(&dir, &["branch", "-d", "side", "nope"]);
    assert!(out.contains("nope") && out.contains("side"), "{out}");
    ok(&dir, &["branch", "-D", "side"]);
    assert_eq!(git_branches(&dir, &[]), ["main"]);

    ok(&dir, &["branch", "-m", "trunk"]);
    assert_eq!(git(&dir, &["branch", "--show-current"]), "trunk\n");
    ok(&dir, &["branch", "create", "topic", "HEAD~1"]);
    assert_eq!(git(&dir, &["branch", "--show-current"]), "topic\n");
    assert_eq!(rev(&dir, "HEAD"), rev(&dir, "trunk~1"));
    ok(&dir, &["branch", "delete", "trunk", "--force"]);
}

/// `rgit --human <args>` and `git <args>` print the same text.
fn same(dir: &Path, args: &[&str]) {
    let mut all = vec!["--human"];
    all.extend(args);
    assert_eq!(
        ok(dir, &all).trim_end(),
        git(dir, args).trim_end(),
        "{args:?}"
    );
}

#[test]
fn branch_lists_sorts_formats_and_copies_like_git() {
    let dir = repo("branch-list");
    commit(&dir, "a", "second");
    with_origin(&dir);
    git(&dir, &["remote", "set-head", "origin", "main"]);
    git(&dir, &["branch", "Zeta", "HEAD~1"]);
    git(&dir, &["branch", "alpha"]);
    git(&dir, &["branch", "v1.10"]);
    git(&dir, &["branch", "v1.9", "HEAD~1"]);
    for args in [
        &["branch"][..],
        &["branch", "-a"],
        &["branch", "-r"],
        &["branch", "-av"],
        &["branch", "-rv"],
        &["branch", "-i"],
        &["branch", "--sort=-refname"],
        &["branch", "--sort=version:refname", "-l", "v*"],
        &["branch", "--sort=-committerdate", "--sort=refname"],
        &[
            "branch",
            "--format=%(refname:short) %(objectname:short) %(HEAD)",
        ],
        &["branch", "-a", "--format=%(refname) %(symref)"],
        &["branch", "--points-at", "HEAD~1"],
        &["branch", "--no-contains", "HEAD"],
        &["branch", "--list", "-i", "z*"],
        &["branch", "--column", "--no-column"],
    ] {
        same(&dir, args);
    }

    git(&dir, &["branch", "-u", "origin/main", "alpha"]);
    git(&dir, &["config", "branch.alpha.description", "about it"]);
    ok(&dir, &["branch", "-c", "alpha", "beta"]);
    assert_eq!(
        git(&dir, &["config", "--get-regexp", "^branch[.]beta[.]"]),
        git(&dir, &["config", "--get-regexp", "^branch[.]alpha[.]"]).replace("alpha", "beta")
    );
    let log = git(&dir, &["reflog", "show", "--format=%gs", "beta"]);
    assert_eq!(
        log,
        format!(
            "Branch: copied refs/heads/alpha to refs/heads/beta\n{}",
            git(&dir, &["reflog", "show", "--format=%gs", "alpha"])
        )
    );
    ok(&dir, &["branch", "-m", "beta", "gamma"]);
    assert_eq!(
        git(&dir, &["config", "branch.gamma.merge"]).trim(),
        "refs/heads/main"
    );
    assert_eq!(
        git(&dir, &["reflog", "show", "--format=%gs", "gamma"])
            .lines()
            .count(),
        log.lines().count() + 1
    );

    ok(&dir, &["branch", "--track", "t1", "main"]);
    assert_eq!(git(&dir, &["config", "branch.t1.remote"]).trim(), ".");
    ok(&dir, &["branch", "--track=inherit", "t2", "alpha"]);
    assert_eq!(git(&dir, &["config", "branch.t2.remote"]).trim(), "origin");
    ok(&dir, &["branch", "--no-track", "t3", "origin/main"]);
    assert!(!git(&dir, &["config", "--list"]).contains("branch.t3."));
    ok(&dir, &["branch", "-r", "-d", "origin/main"]);
    assert!(!git(&dir, &["branch", "-r"]).contains("origin/main\n"));
    let out = fails(&dir, &["branch", "--edit-description"]);
    assert!(out.contains("terminal"), "{out}");
}

#[test]
fn remote_takes_git_forms() {
    let dir = repo("remote-forms");
    with_origin(&dir);
    let origin = dir.with_extension("origin.git");
    let same = |rgit_args: &[&str], git_args: &[&str]| {
        let mut all = vec!["--human"];
        all.extend(rgit_args);
        assert_eq!(
            ok(&dir, &all).trim(),
            git(&dir, git_args).trim(),
            "{rgit_args:?}"
        );
    };

    same(&["remote", "-v"], &["remote", "-v"]);
    ok(
        &dir,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "https://example.com/push.git",
        ],
    );
    same(&["remote", "-v"], &["remote", "-v"]);
    same(
        &["remote", "get-url", "origin"],
        &["remote", "get-url", "origin"],
    );
    same(
        &["remote", "get-url", "--push", "origin"],
        &["remote", "get-url", "--push", "origin"],
    );

    git(&origin, &["branch", "gone", "main"]);
    git(&dir, &["fetch", "-q", "origin"]);
    git(&origin, &["branch", "-D", "gone"]);
    let out = ok(&dir, &["--human", "remote", "prune", "origin"]);
    assert_eq!(out.trim(), "pruned origin/gone");
    assert!(!git(&dir, &["branch", "-r"]).contains("gone"));

    git(
        &dir,
        &[
            "remote",
            "set-url",
            "--add",
            "origin",
            "https://example.com/two.git",
        ],
    );
    same(
        &["remote", "get-url", "--all", "origin"],
        &["remote", "get-url", "--all", "origin"],
    );
    same(
        &["remote", "get-url", "origin"],
        &["remote", "get-url", "origin"],
    );
    same(&["remote", "-v"], &["remote", "-v"]);

    ok(&dir, &["remote", "rm", "origin"]);
    assert_eq!(git(&dir, &["remote"]), "");
}

#[test]
fn remote_shows_updates_and_edits_like_git() {
    let dir = repo("remote-show");
    with_origin(&dir);
    let origin = dir.with_extension("origin.git");
    git(&origin, &["branch", "gone", "main"]);
    git(&origin, &["branch", "feature-long-name", "main"]);
    git(&dir, &["fetch", "-q", "origin"]);
    git(&origin, &["branch", "-D", "gone"]);
    git(&origin, &["branch", "newone", "main"]);
    git(
        &dir,
        &["branch", "-q", "--set-upstream-to=origin/main", "main"],
    );
    git(
        &dir,
        &[
            "branch",
            "-q",
            "--track",
            "feat",
            "origin/feature-long-name",
        ],
    );
    git(&dir, &["config", "branch.feat.rebase", "true"]);
    git(&dir, &["branch", "other"]);
    same(&dir, &["remote", "show", "origin"]);
    same(&dir, &["remote", "show", "-n", "origin"]);
    commit(&dir, "a", "ahead");
    same(&dir, &["remote", "show", "origin"]);

    let out = ok(&dir, &["--human", "remote", "prune", "--dry-run", "origin"]);
    assert_eq!(out.trim(), "would prune origin/gone");
    assert!(git(&dir, &["branch", "-r"]).contains("origin/gone"));

    // A second URL: fetch reads the first, push writes to each, as in git.
    let second = dir.with_extension("second.git");
    let _ = std::fs::remove_dir_all(&second);
    git(&dir, &["init", "-q", "--bare", second.to_str().unwrap()]);
    ok(
        &dir,
        &[
            "remote",
            "set-url",
            "--add",
            "origin",
            second.to_str().unwrap(),
        ],
    );
    same(&dir, &["remote", "-v"]);
    same(&dir, &["remote", "show", "origin"]);
    ok(&dir, &["push", "origin", "main"]);
    assert_eq!(rev(&origin, "main"), rev(&dir, "main"));
    assert_eq!(rev(&second, "main"), rev(&dir, "main"));
    ok(&dir, &["fetch", "origin"]);
    assert!(git(&dir, &["branch", "-r"]).contains("origin/newone"));
    let out = fails(&dir, &["remote", "set-url", "--delete", "origin", "."]);
    assert!(out.contains("Will not delete all non-push URLs"), "{out}");
    ok(&dir, &["remote", "set-url", "--delete", "origin", "second"]);
    same(&dir, &["remote", "get-url", "--all", "origin"]);
    let out = fails(&dir, &["remote", "set-url", "origin", "x", "nomatch"]);
    assert!(out.contains("No such URL"), "{out}");

    ok(&dir, &["remote", "set-head", "origin", "feature-long-name"]);
    assert_eq!(
        git(&dir, &["symbolic-ref", "refs/remotes/origin/HEAD"]).trim(),
        "refs/remotes/origin/feature-long-name"
    );
    ok(&dir, &["remote", "set-head", "origin", "-a"]);
    assert_eq!(
        git(&dir, &["symbolic-ref", "refs/remotes/origin/HEAD"]).trim(),
        "refs/remotes/origin/main"
    );
    ok(&dir, &["remote", "set-head", "origin", "-d"]);
    assert!(!git(&dir, &["branch", "-r"]).contains("HEAD"));
    fails(&dir, &["remote", "set-head", "origin", "nope"]);

    ok(&dir, &["remote", "set-branches", "origin", "main"]);
    ok(
        &dir,
        &["remote", "set-branches", "--add", "origin", "newone"],
    );
    assert_eq!(
        git(&dir, &["config", "--get-all", "remote.origin.fetch"]),
        "+refs/heads/main:refs/remotes/origin/main\n+refs/heads/newone:refs/remotes/origin/newone\n"
    );

    let url = origin.to_str().unwrap();
    ok(
        &dir,
        &["remote", "add", "-f", "-t", "main", "-m", "main", "up", url],
    );
    assert_eq!(
        git(&dir, &["config", "--get-all", "remote.up.fetch"]).trim(),
        "+refs/heads/main:refs/remotes/up/main"
    );
    assert_eq!(rev(&dir, "up/HEAD"), rev(&origin, "main"));
    assert!(!git(&dir, &["branch", "-r"]).contains("up/newone"));
    ok(
        &dir,
        &[
            "remote",
            "add",
            "--mirror=fetch",
            "--no-tags",
            "mirror",
            url,
        ],
    );
    assert_eq!(
        git(&dir, &["config", "remote.mirror.fetch"]).trim(),
        "+refs/*:refs/*"
    );
    assert_eq!(
        git(&dir, &["config", "remote.mirror.tagOpt"]).trim(),
        "--no-tags"
    );
    ok(&dir, &["remote", "add", "--mirror=push", "pushm", url]);
    assert_eq!(git(&dir, &["config", "remote.pushm.mirror"]).trim(), "true");
    ok(&dir, &["remote", "rm", "mirror"]);
    assert!(git(&dir, &["branch"]).contains("other"));

    git(&dir, &["config", "remotes.pair", "up origin"]);
    git(&origin, &["branch", "later", "main"]);
    ok(&dir, &["remote", "update", "pair"]);
    ok(
        &dir,
        &["remote", "set-branches", "--add", "origin", "later"],
    );
    ok(&dir, &["remote", "update", "-p", "origin"]);
    assert!(git(&dir, &["branch", "-r"]).contains("origin/later"));
    fails(&dir, &["remote", "update", "nosuch"]);

    ok(&dir, &["remote", "rename", "up", "upstream"]);
    git(
        &dir,
        &["branch", "-q", "--set-upstream-to=upstream/main", "other"],
    );
    ok(&dir, &["remote", "rename", "upstream", "up2"]);
    assert_eq!(git(&dir, &["config", "branch.other.remote"]).trim(), "up2");
    assert!(git(&dir, &["branch", "-r"]).contains("up2/main"));
    git(&dir, &["config", "branch.other.pushRemote", "up2"]);
    git(&dir, &["config", "remote.pushDefault", "up2"]);
    ok(&dir, &["remote", "rename", "up2", "up3"]);
    assert_eq!(
        git(&dir, &["config", "branch.other.pushRemote"]).trim(),
        "up3"
    );
    assert_eq!(git(&dir, &["config", "remote.pushDefault"]).trim(), "up3");
    assert_eq!(
        git(&dir, &["config", "remote.up3.fetch"]).trim(),
        "+refs/heads/main:refs/remotes/up3/main"
    );
}

#[test]
fn worktree_add_modes_list_formats_and_repair_like_git() {
    let dir = repo("worktree-modes");
    commit(&dir, "a", "second");
    with_origin(&dir);
    let base = std::env::temp_dir().join(format!("rgit-wtm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let wt = |n: &str| base.join(n).to_string_lossy().into_owned();

    ok(
        &dir,
        &[
            "worktree",
            "add",
            "--lock",
            "--reason",
            "busy now",
            &wt("a-long"),
        ],
    );
    ok(&dir, &["worktree", "add", "--lock", &wt("b"), "HEAD~1"]);
    assert_eq!(rev(&base.join("b"), "HEAD"), rev(&dir, "main~1"));
    ok(
        &dir,
        &["worktree", "add", "--orphan", "-b", "orph", &wt("c")],
    );
    assert_eq!(git(&base.join("c"), &["status", "--short"]), "");
    assert_eq!(
        git(&base.join("c"), &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/orph"
    );
    ok(&dir, &["worktree", "add", "--no-checkout", &wt("d")]);
    assert!(!base.join("d/a").exists() && base.join("d/.git").is_file());
    ok(&dir, &["worktree", "add", &wt("e")]);
    std::fs::remove_dir_all(base.join("e")).unwrap();
    for args in [
        &["worktree", "list"][..],
        &["worktree", "list", "-v"],
        &["worktree", "list", "--porcelain"],
        &["worktree", "list", "--porcelain", "-z"],
    ] {
        same(&dir, args);
    }
    let out = ok(&dir, &["--human", "worktree", "prune", "-n"]);
    assert_eq!(out.trim(), "would prune e");
    ok(&dir, &["worktree", "prune"]);
    assert!(!git(&dir, &["worktree", "list"]).contains("prunable"));

    let out = fails(&dir, &["worktree", "add", &wt("f"), "main"]);
    assert!(out.contains("already used by worktree"), "{out}");
    ok(&dir, &["worktree", "add", "-f", &wt("f"), "main"]);
    assert_eq!(
        git(&base.join("f"), &["branch", "--show-current"]),
        "main\n"
    );
    git(&dir, &["branch", "moved", "main"]);
    ok(
        &dir,
        &["worktree", "add", "-B", "moved", &wt("g"), "HEAD~1"],
    );
    assert_eq!(rev(&dir, "moved"), rev(&dir, "main~1"));
    fails(&dir, &["worktree", "add", "-b", "moved", &wt("h")]);
    ok(
        &dir,
        &["worktree", "add", "--track", "-b", "t1", &wt("t1"), "main"],
    );
    assert_eq!(
        git(&dir, &["config", "branch.t1.merge"]).trim(),
        "refs/heads/main"
    );
    ok(
        &dir,
        &[
            "worktree",
            "add",
            "--no-track",
            "-b",
            "t2",
            &wt("t2"),
            "origin/main",
        ],
    );
    assert!(!git(&dir, &["config", "--list"]).contains("branch.t2."));
    std::fs::create_dir_all(base.join("full")).unwrap();
    std::fs::write(base.join("full/x"), "x").unwrap();
    fails(&dir, &["worktree", "add", &wt("full")]);

    // Moved by hand: the admin folder still points at the old place.
    std::fs::rename(base.join("g"), base.join("g2")).unwrap();
    assert!(git(&dir, &["worktree", "list"]).contains("prunable"));
    let out = ok(&dir, &["--human", "worktree", "repair", &wt("g2")]);
    assert!(out.contains("gitdir incorrect"), "{out}");
    assert!(!git(&dir, &["worktree", "list"]).contains("prunable"));
    assert_eq!(
        git(&base.join("g2"), &["branch", "--show-current"]),
        "moved\n"
    );
    std::fs::write(base.join("f/.git"), "gitdir: /nowhere\n").unwrap();
    let out = ok(&dir, &["--human", "worktree", "repair"]);
    assert!(out.contains(".git file broken"), "{out}");
    assert_eq!(
        git(&base.join("f"), &["branch", "--show-current"]),
        "main\n"
    );
}

#[test]
fn worktree_takes_git_argument_order_and_forms() {
    let dir = repo("worktree");
    commit(&dir, "a", "second");
    git(&dir, &["branch", "existing", "HEAD~1"]);
    let base = std::env::temp_dir().join(format!("rgit-wts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let wt = |n: &str| base.join(n).to_string_lossy().into_owned();
    let head_of = |n: &str| git(&base.join(n), &["rev-parse", "--abbrev-ref", "HEAD"]);

    ok(&dir, &["worktree", "add", &wt("a"), "existing"]);
    assert_eq!(head_of("a"), "existing\n");
    ok(&dir, &["worktree", "add", &wt("b")]);
    assert_eq!(head_of("b"), "b\n");
    assert_eq!(rev(&dir, "b"), rev(&dir, "HEAD"));
    ok(&dir, &["worktree", "add", "-b", "newb", &wt("c"), "HEAD~1"]);
    assert_eq!(head_of("c"), "newb\n");
    assert_eq!(rev(&dir, "newb"), rev(&dir, "main~1"));
    ok(&dir, &["worktree", "add", "--detach", &wt("d")]);
    assert_eq!(head_of("d"), "HEAD\n");
    ok(&dir, &["worktree", "add", &wt("e"), &rev(&dir, "HEAD~1")]);
    assert_eq!(head_of("e"), "HEAD\n");
    assert_eq!(rev(&base.join("e"), "HEAD"), rev(&dir, "main~1"));
    fails(&dir, &["worktree", "add", &wt("f"), "existing"]);
    assert_eq!(git(&dir, &["worktree", "list"]).lines().count(), 6);
    let out = ok(&dir, &["worktree", "add", &wt("a"), "existing", "--toon"]);
    assert!(out.contains("no-op"), "{out}");

    ok(&dir, &["worktree", "lock", &wt("a"), "--reason", "busy"]);
    assert!(git(&dir, &["worktree", "list", "--porcelain"]).contains("locked busy"));
    ok(&dir, &["worktree", "unlock", "a"]);
    assert!(!git(&dir, &["worktree", "list", "--porcelain"]).contains("locked"));

    ok(&dir, &["worktree", "move", &wt("a"), &wt("g")]);
    assert!(!base.join("a").exists() && base.join("g/a").is_file());
    std::fs::write(base.join("g/new"), "n\n").unwrap();
    fails(&dir, &["worktree", "remove", &wt("g")]);
    ok(&dir, &["worktree", "remove", &wt("g"), "--force"]);
    assert!(!base.join("g").exists());
    ok(&dir, &["worktree", "remove", "b"]);
    assert!(!base.join("b").exists());
    assert_eq!(git(&dir, &["worktree", "list"]).lines().count(), 4);

    with_origin(&dir);
    git(
        &dir.with_extension("origin.git"),
        &["branch", "remote-only", "main"],
    );
    git(&dir, &["fetch", "-q", "origin"]);
    ok(&dir, &["worktree", "add", &wt("h"), "remote-only"]);
    assert_eq!(head_of("h"), "remote-only\n");
    assert_eq!(
        git(&dir, &["config", "branch.remote-only.merge"]).trim(),
        "refs/heads/remote-only"
    );
}
