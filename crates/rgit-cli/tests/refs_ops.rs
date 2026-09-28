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

/// Branch names from `rgit --human branch ...` (dropping the `* ` mark).
fn branch_names(dir: &Path, args: &[&str]) -> Vec<String> {
    let mut all = vec!["--human", "branch"];
    all.extend(args);
    let mut v: Vec<String> = ok(dir, &all).lines().map(|l| l[2..].to_owned()).collect();
    v.sort_unstable();
    v
}

fn git_branches(dir: &Path, args: &[&str]) -> Vec<String> {
    // rgit, like `git branch -a`'s default view, leaves out `origin/HEAD`.
    let mut all = vec![
        "branch",
        "--format=%(if)%(symref)%(then)%(else)%(refname:short)%(end)",
    ];
    all.extend(args);
    let mut v: Vec<String> = git(dir, &all)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
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
