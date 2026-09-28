//! fetch, pull, push and clone take git's arguments and match git's results,
//! against local bare repositories as remotes.

use std::path::{Path, PathBuf};
use std::process::Command;

fn env<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", home)
        .env("RGIT_OPLOG", "0")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = env(Command::new("git").arg("-C").arg(dir), dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rgit(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = env(&mut Command::new(env!("CARGO_BIN_EXE_rgit")), dir)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, err, success) = rgit(dir, args);
    assert!(success, "rgit {args:?}: {out}{err}");
    out
}

fn fails(dir: &Path, args: &[&str]) -> String {
    let (out, err, success) = rgit(dir, args);
    assert!(!success, "rgit {args:?} should fail: {out}");
    out + &err
}

fn heads(bare: &Path) -> String {
    git(bare, &["for-each-ref", "--format=%(refname) %(objectname)"])
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-remote-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", file]);
}

fn identity(dir: &Path) {
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
}

/// A work repo on `main` with one commit, plus two empty bare remotes:
/// `origin` (pushed by rgit) and `mirror` (pushed by git, the reference).
fn push_setup(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = tmp(tag);
    let (origin, mirror, work) = (root.join("o.git"), root.join("m.git"), root.join("w"));
    for bare in [&origin, &mirror] {
        git(
            &root,
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
    }
    git(&root, &["init", "-q", "-b", "main", work.to_str().unwrap()]);
    identity(&work);
    commit(&work, "a", "a\n");
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(
        &work,
        &["remote", "add", "mirror", mirror.to_str().unwrap()],
    );
    (work, origin, mirror)
}

/// Run the same push with rgit (to origin) and git (to mirror); both remotes
/// must end up with the same refs.
fn same_push(work: &Path, origin: &Path, mirror: &Path, args: &[&str]) {
    let with = |remote: &str| -> Vec<&str> {
        let mut v: Vec<&str> = args.to_vec();
        let at = v.iter().position(|a| *a == "@").unwrap();
        v[at] = if remote == "origin" {
            "origin"
        } else {
            "mirror"
        };
        v
    };
    ok(work, &[&["push"], with("origin").as_slice()].concat());
    git(work, &[&["push", "-q"], with("mirror").as_slice()].concat());
    assert_eq!(heads(origin), heads(mirror), "push {args:?}");
}

#[test]
fn push_takes_remote_and_refspecs_like_git() {
    let (work, origin, mirror) = push_setup("push");
    git(&work, &["branch", "a"]);
    git(&work, &["branch", "b"]);
    git(&work, &["checkout", "-qb", "feature"]);
    commit(&work, "f", "f\n");

    same_push(&work, &origin, &mirror, &["@", "feature"]);
    same_push(&work, &origin, &mirror, &["@", "a", "b"]);
    same_push(&work, &origin, &mirror, &["@", "feature:other"]);
    same_push(&work, &origin, &mirror, &["@", ":other"]);
    same_push(&work, &origin, &mirror, &["@", "HEAD:refs/heads/head-copy"]);
    same_push(&work, &origin, &mirror, &["--all", "@"]);
    same_push(&work, &origin, &mirror, &["--delete", "@", "a", "b"]);
    git(&work, &["tag", "v1"]);
    same_push(&work, &origin, &mirror, &["--tags", "@"]);

    // The old forms still work: --remote, and --delete <branch> on the upstream.
    git(&work, &["checkout", "-q", "main"]);
    ok(&work, &["push", "-u", "--remote", "origin"]);
    assert_eq!(
        git(&work, &["rev-parse", "--abbrev-ref", "main@{upstream}"]).trim(),
        "origin/main"
    );
    ok(&work, &["push", "--delete", "head-copy"]);
    assert!(!heads(&origin).contains("head-copy"));
}

#[test]
fn push_set_upstream_follows_the_refspec() {
    let (work, _, _) = push_setup("upstream");
    git(&work, &["checkout", "-qb", "topic"]);
    ok(&work, &["push", "-u", "origin", "topic:remote-topic"]);
    assert_eq!(
        git(&work, &["rev-parse", "--abbrev-ref", "topic@{upstream}"]).trim(),
        "origin/remote-topic"
    );
}

#[test]
fn push_dry_run_changes_nothing() {
    let (work, origin, _) = push_setup("dry");
    ok(&work, &["push", "origin", "main"]);
    let before = heads(&origin);
    commit(&work, "b", "b\n");
    git(&work, &["branch", "extra"]);

    let out = ok(&work, &["push", "-n", "origin", "main", "extra"]);
    assert!(out.contains("main -> main"), "{out}");
    assert!(out.contains("[new branch]"), "{out}");
    let out = ok(&work, &["push", "--dry-run", "--all", "origin"]);
    assert!(out.contains("extra -> extra"), "{out}");
    assert_eq!(heads(&origin), before);

    // A non-fast-forward is reported as rejected, like git.
    git(&work, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&work, "c", "c\n");
    ok(&work, &["push", "origin", "extra"]);
    git(&work, &["push", "-q", "origin", "HEAD~1:main"]);
    ok(&work, &["fetch", "origin"]);
    git(&work, &["push", "-q", "--force", "origin", "extra:main"]);
    let out = fails(&work, &["push", "-n", "origin", "main"]);
    assert!(out.contains("rejected"), "{out}");
}

/// A bare remote with `main` (two commits) and `topic`, and a clone of it.
fn fetch_setup(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = tmp(tag);
    let (remote, up, clone) = (root.join("r.git"), root.join("up"), root.join("c"));
    git(
        &root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    git(&root, &["init", "-q", "-b", "main", up.to_str().unwrap()]);
    identity(&up);
    commit(&up, "a", "a\n");
    commit(&up, "b", "b\n");
    git(&up, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&up, &["push", "-q", "origin", "main"]);
    git(
        &root,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    identity(&clone);
    (remote, up, clone)
}

#[test]
fn fetch_takes_remote_refspecs_tags_and_dry_run() {
    let (_, up, clone) = fetch_setup("fetch");
    git(&up, &["checkout", "-qb", "topic"]);
    commit(&up, "t", "t\n");
    git(&up, &["tag", "-a", "-m", "rel", "rel"]);
    git(&up, &["checkout", "-q", "main"]);
    git(&up, &["tag", "lone", "HEAD~1"]);
    git(&up, &["push", "-q", "origin", "topic", "rel", "lone"]);

    let refs = || git(&clone, &["for-each-ref", "--format=%(refname)"]);
    let before = refs();
    let out = ok(&clone, &["fetch", "--dry-run", "origin"]);
    assert!(out.contains("topic -> origin/topic"), "{out}");
    assert_eq!(refs(), before);

    ok(&clone, &["fetch", "origin", "topic"]);
    assert_eq!(
        git(&clone, &["rev-parse", "origin/topic"]),
        git(&up, &["rev-parse", "topic"])
    );
    // Like git, naming the refs to fetch follows no tags.
    assert!(!refs().contains("refs/tags/"));

    ok(&clone, &["fetch", "--tags"]);
    let tags = git(&clone, &["tag"]);
    assert_eq!(tags, "lone\nrel\n");
}

#[test]
fn fetch_depth_makes_a_shallow_history() {
    let (remote, _, _) = fetch_setup("depth");
    let root = remote.parent().unwrap();
    let work = root.join("shallow");
    git(root, &["init", "-q", "-b", "main", work.to_str().unwrap()]);
    git(
        &work,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    ok(&work, &["fetch", "--depth", "1", "origin"]);
    assert_eq!(
        git(&work, &["rev-list", "--count", "origin/main"]).trim(),
        "1"
    );
    assert!(work.join(".git/shallow").exists());
}

/// The clone and its upstream both commit, so `main` has diverged.
fn diverge(up: &Path, clone: &Path) {
    commit(clone, "local", "l\n");
    commit(up, "remote", "r\n");
    git(up, &["push", "-q", "origin", "main"]);
}

#[test]
fn pull_merges_a_diverged_branch_like_git() {
    let (remote, up, clone) = fetch_setup("pull-merge");
    let twin = clone.with_file_name("twin");
    git(
        &up,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            twin.to_str().unwrap(),
        ],
    );
    identity(&twin);
    commit(&twin, "local", "l\n");
    diverge(&up, &clone);

    let msg = fails(&clone, &["pull", "--ff-only"]);
    assert!(msg.contains("fast-forward"), "{msg}");

    let msg = fails(&clone, &["pull"]);
    assert!(msg.contains("divergent branches"), "{msg}");
    ok(&clone, &["pull", "--no-rebase"]);
    git(&twin, &["pull", "-q", "--no-rebase"]);
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD^{tree}"]),
        git(&twin, &["rev-parse", "HEAD^{tree}"])
    );
    assert_eq!(
        git(&clone, &["rev-list", "--count", "--merges", "HEAD"]).trim(),
        "1"
    );
    let subject = git(&clone, &["log", "-1", "--format=%s"]);
    assert!(subject.starts_with("Merge branch 'main' of "), "{subject}");
}

#[test]
fn pull_rebases_with_the_flag_or_config() {
    let (_, up, clone) = fetch_setup("pull-rebase");
    diverge(&up, &clone);
    ok(&clone, &["pull", "--rebase"]);
    assert_eq!(
        git(&clone, &["rev-list", "--count", "--merges", "HEAD"]).trim(),
        "0"
    );
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD~1"]),
        git(&up, &["rev-parse", "HEAD"])
    );

    commit(&clone, "local2", "l2\n");
    commit(&up, "remote2", "r2\n");
    git(&up, &["push", "-q", "origin", "main"]);
    git(&clone, &["config", "pull.rebase", "true"]);
    ok(&clone, &["pull"]);
    assert_eq!(
        git(&clone, &["rev-list", "--count", "--merges", "HEAD"]).trim(),
        "0"
    );

    commit(&clone, "local3", "l3\n");
    commit(&up, "remote3", "r3\n");
    git(&up, &["push", "-q", "origin", "main"]);
    ok(&clone, &["pull", "--no-rebase"]);
    assert_eq!(
        git(&clone, &["rev-list", "--count", "--merges", "HEAD"]).trim(),
        "1"
    );
}

#[test]
fn pull_takes_a_remote_and_branch() {
    let (_, up, clone) = fetch_setup("pull-branch");
    git(&up, &["checkout", "-qb", "topic"]);
    commit(&up, "t", "t\n");
    git(&up, &["push", "-q", "origin", "topic"]);
    ok(&clone, &["pull", "origin", "topic"]);
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD"]),
        git(&up, &["rev-parse", "topic"])
    );
}

#[test]
fn clone_takes_depth_bare_origin_and_submodules() {
    let (remote, up, _) = fetch_setup("clone");
    let root = remote.parent().unwrap().to_path_buf();

    // A plain path ignores --depth with git's warning; file:// is shallow.
    let (_, err, success) = rgit(
        &root,
        &["clone", "--depth", "1", remote.to_str().unwrap(), "full"],
    );
    assert!(success, "{err}");
    assert!(err.contains("--depth is ignored in local clones"), "{err}");
    assert_eq!(
        git(&root.join("full"), &["rev-list", "--count", "HEAD"]).trim(),
        "2"
    );
    let url = format!("file://{}", remote.display());
    ok(&root, &["clone", "--depth", "1", &url, "shallow"]);
    git(&root, &["clone", "-q", "--depth", "1", &url, "shallow-git"]);
    assert_eq!(
        git(&root.join("shallow"), &["rev-list", "--count", "HEAD"]),
        git(&root.join("shallow-git"), &["rev-list", "--count", "HEAD"])
    );
    assert!(root.join("shallow/.git/shallow").exists());

    ok(&root, &["clone", "--bare", up.to_str().unwrap()]);
    assert_eq!(
        git(&root.join("up.git"), &["rev-parse", "--is-bare-repository"]).trim(),
        "true"
    );
    ok(
        &root,
        &["clone", "-o", "up", remote.to_str().unwrap(), "named"],
    );
    assert_eq!(git(&root.join("named"), &["remote"]).trim(), "up");

    // A submodule is cloned and checked out with --recurse-submodules.
    let sub = root.join("sub");
    git(&root, &["init", "-q", "-b", "main", sub.to_str().unwrap()]);
    identity(&sub);
    commit(&sub, "s", "s\n");
    git(
        &up,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "sub",
        ],
    );
    git(&up, &["commit", "-qm", "sub"]);
    git(&up, &["push", "-q", "origin", "main"]);
    ok(
        &root,
        &[
            "clone",
            "--recurse-submodules",
            remote.to_str().unwrap(),
            "withsub",
        ],
    );
    assert_eq!(
        std::fs::read_to_string(root.join("withsub/sub/s")).unwrap(),
        "s\n"
    );
}
