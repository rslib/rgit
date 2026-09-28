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
fn pull_rebase_stops_on_a_conflict_for_continue() {
    let (_, up, clone) = fetch_setup("pull-rebase-stop");
    commit(&clone, "a", "local\n");
    commit(&up, "a", "remote\n");
    git(&up, &["push", "-q", "origin", "main"]);
    let out = fails(&clone, &["pull", "--rebase"]);
    assert!(out.contains("CONFLICT"), "{out}");
    assert!(clone.join(".git/rebase-merge").exists());
    std::fs::write(clone.join("a"), "both\n").unwrap();
    git(&clone, &["add", "a"]);
    ok(&clone, &["rebase", "--continue"]);
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD~1"]),
        git(&up, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&clone, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
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

fn human(dir: &Path, args: &[&str]) -> (String, String, bool) {
    rgit(dir, &[&["--human"], args].concat())
}

/// A second clone of `origin` pushes a commit, so `work`'s next push is refused.
fn race(work: &Path, origin: &Path) {
    let other = work.with_file_name("other");
    git(
        work.parent().unwrap(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    identity(&other);
    commit(&other, "o", "o\n");
    git(&other, &["push", "-q", "origin", "main"]);
}

#[test]
fn push_rejection_prints_gits_report_and_pushes_the_rest() {
    let (work, origin, _) = push_setup("rejected");
    ok(&work, &["push", "origin", "main"]);
    race(&work, &origin);
    commit(&work, "w", "w\n");
    let url = origin.to_str().unwrap();

    let (out, err, success) = human(&work, &["push", "origin", "main"]);
    assert!(!success, "{out}");
    assert!(
        err.starts_with(&format!(
            "To {url}\n ! [rejected]        main -> main (fetch first)\n\
             error: failed to push some refs to '{url}'\n\
             hint: Updates were rejected because the remote contains work that you do not\n"
        )),
        "{err}"
    );
    assert!(!err.contains("non-fastforwardable"), "{err}");

    ok(&work, &["fetch", "origin"]);
    git(&work, &["branch", "extra"]);
    let (_, err, success) = human(&work, &["push", "origin", "main", "extra"]);
    assert!(!success);
    assert!(
        err.contains(" ! [rejected]        main -> main (non-fast-forward)"),
        "{err}"
    );
    assert!(
        err.contains(
            "hint: Updates were rejected because the tip of your current branch is behind"
        ),
        "{err}"
    );
    // Like git, the refs that can be pushed are.
    assert!(err.contains(" * [new branch]      extra -> extra"), "{err}");
    assert!(heads(&origin).contains("refs/heads/extra"));

    // --atomic pushes nothing when one ref is refused.
    git(&work, &["branch", "extra2"]);
    let msg = fails(&work, &["push", "--atomic", "origin", "main", "extra2"]);
    assert!(msg.contains("atomic push failed"), "{msg}");
    assert!(!heads(&origin).contains("extra2"));

    // A tag that moved is refused as git does.
    git(&work, &["tag", "v1"]);
    ok(&work, &["push", "origin", "v1"]);
    git(&work, &["tag", "-f", "v1", "HEAD~1"]);
    let (_, err, _) = human(&work, &["push", "origin", "v1"]);
    assert!(
        err.contains(" ! [rejected]        v1 -> v1 (already exists)")
            && err.contains(
                "hint: Updates were rejected because the tag already exists in the remote."
            ),
        "{err}"
    );
}

#[test]
fn push_deletes_tags_by_short_name_and_follows_tags() {
    let (work, origin, mirror) = push_setup("tags");
    git(&work, &["tag", "v1"]);
    git(&work, &["tag", "-a", "-m", "rel", "rel"]);
    same_push(&work, &origin, &mirror, &["@", "main", "v1", "rel"]);
    same_push(&work, &origin, &mirror, &["@", ":v1"]);
    assert!(!heads(&origin).contains("refs/tags/v1"));
    same_push(&work, &origin, &mirror, &["@", ":rel"]);

    commit(&work, "b", "b\n");
    git(&work, &["tag", "-a", "-m", "two", "two"]);
    git(&work, &["tag", "light"]);
    same_push(&work, &origin, &mirror, &["--follow-tags", "@", "main"]);
    assert!(heads(&origin).contains("refs/tags/two"));
    assert!(!heads(&origin).contains("refs/tags/light"));
}

#[test]
fn push_prune_mirror_and_porcelain_match_git() {
    let (work, origin, mirror) = push_setup("mirror");
    git(&work, &["branch", "gone"]);
    git(&work, &["tag", "t1"]);
    same_push(&work, &origin, &mirror, &["--all", "@"]);
    git(&work, &["branch", "-D", "gone"]);
    same_push(&work, &origin, &mirror, &["--prune", "--all", "@"]);
    assert!(!heads(&origin).contains("gone"));
    git(&work, &["branch", "fresh"]);
    git(&work, &["tag", "-d", "t1"]);
    let local = heads(&work);
    ok(&work, &["push", "--mirror", "origin"]);
    assert_eq!(heads(&origin), local);

    git(&work, &["branch", "porc"]);
    let (out, _, success) = human(&work, &["push", "--porcelain", "origin", "porc"]);
    assert!(success);
    assert_eq!(
        out,
        format!(
            "To {}\n*\trefs/heads/porc:refs/heads/porc\t[new branch]\nDone\n",
            origin.display()
        )
    );
    let (out, _, _) = human(&work, &["push", "origin", "porc"]);
    assert_eq!(out, "Everything up-to-date\n");
    let (out, _, _) = human(
        &work,
        &["push", "-q", "--recurse-submodules=check", "origin", "main"],
    );
    assert_eq!(out, "");
}

#[test]
fn push_runs_the_pre_push_hook_like_git() {
    use std::os::unix::fs::PermissionsExt;
    let (work, origin, _) = push_setup("hook");
    let hook = work.join(".git/hooks/pre-push");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho \"$1 $2\" > hook-args\ncat > hook-in\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    let msg = fails(&work, &["push", "origin", "main"]);
    assert!(msg.contains("failed to push some refs"), "{msg}");
    assert_eq!(heads(&origin), "");
    let head = git(&work, &["rev-parse", "HEAD"]);
    assert_eq!(
        std::fs::read_to_string(work.join("hook-in")).unwrap(),
        format!(
            "refs/heads/main {} refs/heads/main {}\n",
            head.trim(),
            "0".repeat(40)
        )
    );
    assert_eq!(
        std::fs::read_to_string(work.join("hook-args"))
            .unwrap()
            .trim(),
        format!("origin {}", origin.display())
    );
    ok(&work, &["push", "--no-verify", "origin", "main"]);
    assert!(heads(&origin).contains("refs/heads/main"));
}

#[test]
fn push_honours_push_default() {
    let (work, origin, _) = push_setup("default");
    ok(&work, &["push", "origin", "main"]);
    git(&work, &["checkout", "-qb", "topic"]);
    ok(&work, &["fetch", "origin"]);
    git(&work, &["branch", "-u", "origin/main"]);
    commit(&work, "t", "t\n");
    let msg = fails(&work, &["push"]);
    assert!(msg.contains("does not match"), "{msg}");
    git(&work, &["config", "push.default", "upstream"]);
    ok(&work, &["push"]);
    assert_eq!(
        git(&origin, &["rev-parse", "main"]),
        git(&work, &["rev-parse", "topic"])
    );
    git(&work, &["config", "push.default", "current"]);
    ok(&work, &["push"]);
    assert!(heads(&origin).contains("refs/heads/topic"));
    git(&work, &["config", "push.default", "nothing"]);
    fails(&work, &["push"]);
}

#[test]
fn fetch_writes_fetch_head_and_takes_gits_flags() {
    let (remote, up, clone) = fetch_setup("fetch-flags");
    git(&up, &["checkout", "-qb", "topic"]);
    commit(&up, "t", "t\n");
    git(&up, &["checkout", "-q", "main"]);
    commit(&up, "c", "c\n");
    git(&up, &["tag", "-a", "-m", "rel", "rel"]);
    git(&up, &["push", "-q", "origin", "main", "topic", "rel"]);
    let tags = || git(&clone, &["tag"]);

    // A dry run lists the tag a fetch would follow, and writes nothing.
    let (out, _, _) = human(&clone, &["fetch", "--dry-run"]);
    assert!(out.contains(" * [new tag]         rel -> rel"), "{out}");
    assert!(
        out.contains(" * [new branch]      topic -> origin/topic"),
        "{out}"
    );
    assert_eq!(tags(), "");
    ok(&clone, &["fetch", "--no-tags"]);
    assert_eq!(tags(), "");

    // FETCH_HEAD is git's, so merging it works.
    ok(&clone, &["fetch", "origin", "topic"]);
    let fetch_head = std::fs::read_to_string(clone.join(".git/FETCH_HEAD")).unwrap();
    assert_eq!(
        fetch_head,
        format!(
            "{}\t\tbranch 'topic' of {}\n",
            git(&up, &["rev-parse", "topic"]).trim(),
            remote.display()
        )
    );
    ok(&clone, &["merge", "FETCH_HEAD"]);
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD"]),
        git(&up, &["rev-parse", "topic"])
    );

    // --refmap: none, or another mapping.
    git(&clone, &["update-ref", "-d", "refs/remotes/origin/topic"]);
    ok(&clone, &["fetch", "--refmap=", "origin", "topic"]);
    assert!(!git(&clone, &["for-each-ref"]).contains("origin/topic"));
    ok(
        &clone,
        &[
            "fetch",
            "--refmap=+refs/heads/*:refs/remotes/alt/*",
            "origin",
            "topic",
        ],
    );
    assert!(git(&clone, &["for-each-ref"]).contains("refs/remotes/alt/topic"));

    // --set-upstream records the fetched branch.
    ok(&clone, &["fetch", "--set-upstream", "origin", "topic"]);
    assert_eq!(
        git(&clone, &["config", "branch.main.merge"]).trim(),
        "refs/heads/topic"
    );

    // --prune-tags drops a tag gone from the remote.
    ok(&clone, &["fetch", "--tags", "-f", "-q"]);
    assert_eq!(tags(), "rel\n");
    git(&up, &["push", "-q", "origin", ":refs/tags/rel"]);
    ok(&clone, &["fetch", "--prune", "--prune-tags"]);
    assert_eq!(tags(), "");

    // --multiple fetches each named remote.
    git(
        &clone,
        &["remote", "add", "second", remote.to_str().unwrap()],
    );
    ok(&clone, &["fetch", "--multiple", "origin", "second"]);
    assert!(git(&clone, &["for-each-ref"]).contains("refs/remotes/second/main"));
}

#[test]
fn fetch_deepens_and_unshallows() {
    let (remote, up, _) = fetch_setup("unshallow");
    commit(&up, "c", "c\n");
    git(&up, &["push", "-q", "origin", "main"]);
    let root = remote.parent().unwrap();
    let url = format!("file://{}", remote.display());
    git(root, &["clone", "-q", "--depth", "1", &url, "sh"]);
    let work = root.join("sh");
    let count = || {
        git(&work, &["rev-list", "--count", "HEAD"])
            .trim()
            .to_owned()
    };
    assert_eq!(count(), "1");
    ok(&work, &["fetch", "--deepen", "1"]);
    assert_eq!(count(), "2");
    ok(&work, &["fetch", "--unshallow"]);
    assert_eq!(count(), "3");
    assert!(!work.join(".git/shallow").exists());
}

#[test]
fn pull_autostashes_with_the_flag_or_config() {
    let (_, up, clone) = fetch_setup("pull-autostash");
    diverge(&up, &clone);
    std::fs::write(clone.join("a"), "dirty\n").unwrap();
    let out = ok(&clone, &["pull", "--rebase", "--autostash"]);
    assert!(out.contains("Applied autostash."), "{out}");
    assert_eq!(std::fs::read_to_string(clone.join("a")).unwrap(), "dirty\n");
    assert!(clone.join("remote").exists());
    commit(&up, "remote2", "r2\n");
    git(&up, &["push", "-q", "origin", "main"]);
    git(&clone, &["config", "rebase.autoStash", "true"]);
    ok(&clone, &["pull", "--rebase"]);
    assert_eq!(std::fs::read_to_string(clone.join("a")).unwrap(), "dirty\n");
    assert!(clone.join("remote2").exists());
}

#[test]
fn pull_squash_no_commit_and_strategy_option() {
    let (_, up, clone) = fetch_setup("pull-flags");
    diverge(&up, &clone);
    ok(&clone, &["pull", "--squash"]);
    assert!(!clone.join(".git/MERGE_HEAD").exists());
    assert_eq!(
        git(&clone, &["diff", "--cached", "--name-only"]),
        "remote\n"
    );
    git(&clone, &["commit", "-qm", "squashed"]);

    commit(&up, "remote2", "r2\n");
    git(&up, &["push", "-q", "origin", "main"]);
    ok(&clone, &["pull", "--no-commit", "--no-rebase"]);
    assert!(clone.join(".git/MERGE_HEAD").exists());
    git(&clone, &["commit", "-qm", "merged"]);

    // -X theirs settles a conflicting hunk with the upstream's side.
    commit(&clone, "a", "mine\n");
    commit(&up, "a", "theirs\n");
    git(&up, &["push", "-q", "origin", "main"]);
    ok(&clone, &["pull", "--no-rebase", "-X", "theirs"]);
    assert_eq!(
        std::fs::read_to_string(clone.join("a")).unwrap(),
        "theirs\n"
    );
}

#[test]
fn clone_takes_single_branch_no_checkout_mirror_and_git_only_flags() {
    let (remote, up, _) = fetch_setup("clone-flags");
    git(&up, &["checkout", "-qb", "topic"]);
    commit(&up, "t", "t\n");
    git(&up, &["tag", "v1"]);
    git(&up, &["push", "-q", "origin", "topic", "v1"]);
    let root = remote.parent().unwrap().to_path_buf();
    let src = remote.to_str().unwrap();
    let refs = |dir: &str| git(&root.join(dir), &["for-each-ref", "--format=%(refname)"]);

    ok(&root, &["clone", "--single-branch", src, "single"]);
    assert_eq!(
        refs("single"),
        "refs/heads/main\nrefs/remotes/origin/HEAD\nrefs/remotes/origin/main\n"
    );
    ok(
        &root,
        &[
            "clone",
            "--single-branch",
            "-b",
            "topic",
            "--no-tags",
            src,
            "single-topic",
        ],
    );
    assert_eq!(
        refs("single-topic"),
        "refs/heads/topic\nrefs/remotes/origin/topic\n"
    );

    ok(
        &root,
        &["clone", "-n", "-c", "core.custom=yes", src, "nock"],
    );
    assert!(!root.join("nock/a").exists());
    assert_eq!(
        git(&root.join("nock"), &["config", "core.custom"]).trim(),
        "yes"
    );

    ok(&root, &["clone", "--mirror", src, "m.git"]);
    git(&root, &["clone", "-q", "--mirror", src, "gm.git"]);
    assert_eq!(refs("m.git"), refs("gm.git"));
    assert_eq!(
        git(&root.join("m.git"), &["config", "remote.origin.mirror"]).trim(),
        "true"
    );

    ok(&root, &["clone", "--shared", src, "shared"]);
    assert!(root.join("shared/.git/objects/info/alternates").exists());
    ok(&root, &["clone", "--reference", src, src, "referenced"]);
    assert!(
        root.join("referenced/.git/objects/info/alternates")
            .exists()
    );

    git(&remote, &["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{src}");
    ok(&root, &["clone", "--filter=blob:none", &url, "partial"]);
    assert_eq!(
        git(&root.join("partial"), &["config", "remote.origin.promisor"]).trim(),
        "true"
    );
}

#[test]
fn ls_remote_matches_git() {
    let (remote, up, clone) = fetch_setup("ls-remote");
    git(&up, &["tag", "-a", "-m", "rel", "v1.0"]);
    git(&up, &["tag", "light"]);
    git(&up, &["push", "-q", "origin", "v1.0", "light"]);
    let src = remote.to_str().unwrap();
    for args in [
        vec![],
        vec!["--heads"],
        vec!["--tags"],
        vec!["--refs"],
        vec!["--symref"],
        vec!["--tags", "--refs"],
        vec!["-q", "--exit-code"],
    ] {
        let mut want = vec!["ls-remote"];
        want.extend(&args);
        want.push(src);
        let (out, _, success) = human(&clone, &want);
        assert!(success);
        assert_eq!(out, git(&clone, &want), "{args:?}");
    }
    for patterns in [["main", "v1*"], ["HEAD", "heads/ma?n"]] {
        let args = [&["ls-remote", src][..], &patterns[..]].concat();
        let (out, _, _) = human(&clone, &args);
        assert_eq!(out, git(&clone, &args), "{patterns:?}");
    }
    // With no repository named it lists the branch's remote.
    let (out, err, _) = human(&clone, &["ls-remote", "--heads"]);
    assert_eq!(out, git(&clone, &["ls-remote", "--heads"]));
    assert_eq!(err, format!("From {src}\n"));
    // It needs no repository to list a URL.
    let (out, _, _) = human(remote.parent().unwrap(), &["ls-remote", "--tags", src]);
    assert!(out.contains("refs/tags/light"), "{out}");
    let (_, _, success) = human(&clone, &["ls-remote", "--exit-code", src, "nothing"]);
    assert!(!success);
    let (out, _, _) = human(&clone, &["ls-remote", "--get-url"]);
    assert_eq!(out.trim(), src);
    let out = ok(&clone, &["ls-remote", "--heads", src]);
    assert!(out.contains("refs[1]"), "{out}");
}

#[test]
fn init_takes_template_shared_and_separate_git_dir() {
    let root = tmp("init");
    let tpl = root.join("tpl");
    std::fs::create_dir_all(tpl.join("hooks")).unwrap();
    std::fs::write(tpl.join("hooks/marker"), "x").unwrap();
    let (out, _, success) = human(&root, &["init", "--template", tpl.to_str().unwrap(), "t"]);
    assert!(success);
    let real = std::fs::canonicalize(root.join("t/.git")).unwrap();
    assert_eq!(
        out,
        format!("Initialized empty Git repository in {}/\n", real.display())
    );
    assert!(root.join("t/.git/hooks/marker").exists());

    ok(&root, &["init", "--bare", "--shared=group", "s.git"]);
    assert_eq!(
        git(&root.join("s.git"), &["config", "core.sharedRepository"]).trim(),
        "1"
    );

    let sep = root.join("sep.git");
    ok(
        &root,
        &["init", "--separate-git-dir", sep.to_str().unwrap(), "w"],
    );
    let link = std::fs::read_to_string(root.join("w/.git")).unwrap();
    assert!(link.starts_with("gitdir: "), "{link}");
    assert_eq!(
        git(&root.join("w"), &["rev-parse", "--is-inside-work-tree"]).trim(),
        "true"
    );

    let (out, _, _) = human(&root, &["init", "-q", "--object-format=sha1", "quiet"]);
    assert_eq!(out, "");
    fails(&root, &["init", "--object-format=sha256", "sha256"]);
}

/// A superproject `up`, pushed to `r.git`, with submodule `sub` cloned from `lib`.
fn submodule_setup(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (remote, up, _) = fetch_setup(tag);
    let root = remote.parent().unwrap().to_path_buf();
    let lib = root.join("lib");
    git(&root, &["init", "-q", "-b", "main", lib.to_str().unwrap()]);
    identity(&lib);
    commit(&lib, "l", "l\n");
    ok(&up, &["submodule", "add", lib.to_str().unwrap(), "sub"]);
    git(&up, &["commit", "-qm", "sub"]);
    git(&up, &["push", "-q", "origin", "main"]);
    (root, up, lib)
}

#[test]
fn submodule_add_status_update_and_deinit() {
    let (root, up, lib) = submodule_setup("sub");
    assert_eq!(std::fs::read_to_string(up.join("sub/l")).unwrap(), "l\n");
    assert_eq!(
        git(&up, &["config", "-f", ".gitmodules", "submodule.sub.url"]).trim(),
        lib.to_str().unwrap()
    );
    let (out, _, _) = human(&up, &["submodule"]);
    assert_eq!(out, git(&up, &["submodule", "status"]));

    // A plain clone has it uninitialized; init and update check it out.
    let remote = root.join("r.git");
    git(&root, &["clone", "-q", remote.to_str().unwrap(), "fresh"]);
    let fresh = root.join("fresh");
    let (out, _, _) = human(&fresh, &["submodule", "status"]);
    assert_eq!(out, git(&fresh, &["submodule", "status"]));
    assert!(out.starts_with('-'), "{out}");
    let (out, _, _) = human(&fresh, &["submodule", "init"]);
    assert_eq!(
        out,
        format!(
            "Submodule 'sub' ({}) registered for path 'sub'\n",
            lib.display()
        )
    );
    let (out, _, success) = human(&fresh, &["submodule", "update", "--recursive"]);
    assert!(success, "{out}");
    assert!(out.contains("Submodule path 'sub': checked out '"), "{out}");
    assert_eq!(std::fs::read_to_string(fresh.join("sub/l")).unwrap(), "l\n");
    let (out, _, _) = human(&fresh, &["submodule", "status", "--cached"]);
    assert_eq!(out, git(&fresh, &["submodule", "status", "--cached"]));

    // --remote follows the submodule's branch past the recorded commit.
    commit(&lib, "l2", "l2\n");
    ok(&fresh, &["submodule", "update", "--remote"]);
    assert!(fresh.join("sub/l2").exists());
    let (out, _, _) = human(&fresh, &["submodule"]);
    assert!(out.starts_with('+'), "{out}");

    let (out, _, success) = human(&fresh, &["submodule", "foreach", "echo $name $sm_path"]);
    assert!(success);
    assert_eq!(out, "Entering 'sub'\nsub sub\n");
    let (out, _, _) = human(&fresh, &["submodule", "summary"]);
    assert!(out.contains("* sub "), "{out}");

    ok(&fresh, &["submodule", "set-branch", "-b", "main", "sub"]);
    assert_eq!(
        git(
            &fresh,
            &["config", "-f", ".gitmodules", "submodule.sub.branch"]
        )
        .trim(),
        "main"
    );
    ok(&fresh, &["submodule", "set-branch", "-d", "sub"]);
    ok(&fresh, &["submodule", "set-url", "sub", "../elsewhere"]);
    assert!(
        git(&fresh, &["config", "submodule.sub.url"])
            .trim()
            .ends_with("elsewhere")
    );
    ok(&fresh, &["submodule", "sync"]);

    fails(&fresh, &["submodule", "deinit"]);
    std::fs::write(fresh.join("sub/l"), "changed\n").unwrap();
    let msg = fails(&fresh, &["submodule", "deinit", "sub"]);
    assert!(msg.contains("local modifications"), "{msg}");
    let (out, _, success) = human(&fresh, &["submodule", "deinit", "-f", "--all"]);
    assert!(success, "{out}");
    assert!(out.contains("Cleared directory 'sub'"), "{out}");
    assert!(!fresh.join("sub/l").exists());
    let (out, _, _) = human(&fresh, &["submodule"]);
    assert!(out.starts_with('-'), "{out}");
}

#[test]
fn submodule_absorbgitdirs_and_named_add_go_through_git() {
    let (root, up, lib) = submodule_setup("sub-git");
    // Named add and absorbgitdirs are git's own (libgit2 has neither).
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args([
            "submodule",
            "add",
            "--name",
            "other",
            lib.to_str().unwrap(),
            "two",
        ])
        .current_dir(&up)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &root)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_0", "always")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        git(
            &up,
            &["config", "-f", ".gitmodules", "submodule.other.path"]
        )
        .trim(),
        "two"
    );
    ok(&up, &["submodule", "absorbgitdirs"]);
    assert!(up.join("two/.git").is_file());
    let out = ok(&up, &["submodule", "status"]);
    assert!(out.contains("submodules[2]"), "{out}");
}
