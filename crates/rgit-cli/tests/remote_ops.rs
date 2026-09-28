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
        .args(
            ["--human", "--text", "--json", "--toon", "--axi"]
                .iter()
                .all(|m| !args.contains(m))
                .then_some("--toon"),
        )
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
    assert!(out.contains("topic      -> origin/topic"), "{out}");
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
    assert!(
        out.contains(" * [new tag]         rel        -> rel"),
        "{out}"
    );
    assert!(
        out.contains(" * [new branch]      topic      -> origin/topic"),
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
            remote.with_extension("").display()
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
fn pull_rebase_keeps_the_autostash_across_a_stop_and_takes_x() {
    let (_, up, clone) = fetch_setup("pull-rebase-autostash-x");
    commit(&clone, "a", "mine\n");
    commit(&up, "a", "theirs\n");
    git(&up, &["push", "-q", "origin", "main"]);
    std::fs::write(clone.join("b"), "dirty\n").unwrap();
    fails(&clone, &["pull", "--rebase", "--autostash"]);
    let saved = std::fs::read_to_string(clone.join(".git/rebase-merge/autostash")).unwrap();
    assert_eq!(git(&clone, &["cat-file", "-t", saved.trim()]), "commit\n");
    assert_eq!(std::fs::read_to_string(clone.join("b")).unwrap(), "b\n");
    std::fs::write(clone.join("a"), "both\n").unwrap();
    git(&clone, &["add", "a"]);
    ok(&clone, &["rebase", "--continue"]);
    assert_eq!(std::fs::read_to_string(clone.join("b")).unwrap(), "dirty\n");
    git(&clone, &["checkout", "-q", "--", "b"]);

    commit(&clone, "a", "mine2\n");
    commit(&up, "a", "theirs2\n");
    git(&up, &["push", "-q", "origin", "main"]);
    ok(&clone, &["pull", "--rebase", "-X", "ours"]);
    // In a rebase "ours" is the upstream being rebased onto, as in git.
    assert_eq!(
        std::fs::read_to_string(clone.join("a")).unwrap(),
        "theirs2\n"
    );
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

/// git with local-path submodule clones allowed.
fn git_sm(dir: &Path, args: &[&str]) -> String {
    git(dir, &[&["-c", "protocol.file.allow=always"], args].concat())
}

#[test]
fn submodule_named_add_and_absorbgitdirs_match_git() {
    let (root, ours, lib) = submodule_setup("sub-named");
    let theirs = root.join("theirs");
    let remote = root.join("r.git");
    git(&root, &["clone", "-q", remote.to_str().unwrap(), "theirs"]);
    git_sm(&theirs, &["submodule", "update", "-q", "--init"]);
    let url = lib.to_str().unwrap();
    ok(&ours, &["submodule", "add", "--name", "other", url, "two"]);
    git_sm(
        &theirs,
        &["submodule", "add", "--name", "other", url, "two"],
    );
    for d in [&ours, &theirs] {
        git_sm(d, &["clone", "-q", url, "emb"]);
        git_sm(d, &["submodule", "add", "-q", url, "emb"]);
    }
    ok(&ours, &["submodule", "absorbgitdirs"]);
    git(&theirs, &["submodule", "absorbgitdirs"]);
    let state = |d: &Path| {
        let read = |p: &str| std::fs::read_to_string(d.join(p)).unwrap();
        (
            read(".gitmodules"),
            read("two/.git"),
            read("emb/.git"),
            git(
                d,
                &["config", "--get-regexp", "^submodule\\.(other|emb)\\."],
            ),
            git(d, &["ls-files", "-s"]),
            git(d, &["status", "--short", "--untracked-files=no"]),
            git(d, &["submodule", "status"]),
            ["other", "emb"].map(|m| {
                let file = format!(".git/modules/{m}/config");
                git(d, &["config", "-f", &file, "core.worktree"])
            }),
        )
    };
    assert_eq!(state(&ours), state(&theirs));
}

#[test]
fn submodule_summary_foreach_and_parallel_update_match_git() {
    let (root, up, lib) = submodule_setup("sub-summary");
    let sub = up.join("sub");
    identity(&sub);
    commit(&sub, "s1", "1\n");
    commit(&sub, "s2", "2\n");
    let same = |args: &[&str]| {
        let (out, err, success) = human(&up, args);
        assert!(success, "{args:?}: {err}");
        assert_eq!(out.trim_end(), git(&up, args).trim_end(), "{args:?}");
    };
    same(&["submodule", "summary"]);
    same(&["submodule", "summary", "-n", "1"]);
    git(&up, &["add", "sub"]);
    same(&["submodule", "summary", "--cached"]);
    same(&["submodule", "summary", "--files"]);
    git(&sub, &["reset", "-q", "--hard", "HEAD~2"]);
    commit(&sub, "s3", "3\n");
    same(&["submodule", "summary", "--files"]);
    same(&["submodule", "summary", "HEAD", "--", "sub"]);
    git(&up, &["reset", "-q", "sub"]);

    // A nested submodule sees its own superproject in $toplevel and $sm_path.
    let inner = root.join("inner");
    git(
        &root,
        &["init", "-q", "-b", "main", inner.to_str().unwrap()],
    );
    identity(&inner);
    commit(&inner, "i", "i\n");
    git_sm(
        &lib,
        &["submodule", "add", "-q", inner.to_str().unwrap(), "deep"],
    );
    git(&lib, &["commit", "-qm", "deep"]);
    git(&sub, &["fetch", "-q", "origin"]);
    git(&sub, &["checkout", "-q", "origin/main"]);
    git_sm(&sub, &["submodule", "update", "-q", "--init"]);
    let cmd = "echo $name $sm_path $displaypath $sha1 $toplevel $path";
    same(&["submodule", "foreach", "--recursive", cmd]);
    same(&["submodule", "foreach", "echo", "$name"]);

    git_sm(
        &up,
        &["submodule", "add", "-q", inner.to_str().unwrap(), "more"],
    );
    git(&up, &["commit", "-qm", "more"]);
    git(&up, &["push", "-q", "origin", "main"]);
    let remote = root.join("r.git");
    git(&root, &["clone", "-q", remote.to_str().unwrap(), "par"]);
    git(&root, &["clone", "-q", remote.to_str().unwrap(), "gpar"]);
    let par = root.join("par");
    let (out, err, success) = human(&par, &["submodule", "update", "--init", "--jobs", "2"]);
    assert!(success, "{out}{err}");
    assert!(out.contains("Submodule path 'more': checked out"), "{out}");
    let gpar = root.join("gpar");
    git_sm(
        &gpar,
        &["submodule", "update", "-q", "--init", "--jobs", "2"],
    );
    assert_eq!(
        git(&par, &["submodule", "status"]),
        git(&gpar, &["submodule", "status"])
    );
}

/// An upstream on `main` with dated commits (an older one tagged), and a
/// `topic` branch with a tag of its own, pushed to a bare remote. Returns the
/// test folder and the remote's file:// URL.
fn dated_setup(tag: &str) -> (PathBuf, String) {
    let (remote, up, _) = fetch_setup(tag);
    std::fs::create_dir_all(up.join("d")).unwrap();
    for year in ["2021", "2022", "2023"] {
        std::fs::write(up.join("d").join(year), year).unwrap();
        git(&up, &["add", "."]);
        let date = format!("{year}-01-01T00:00:00Z");
        let out = env(Command::new("git").arg("-C").arg(&up), &up)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_DATE", &date)
            .args(["commit", "-qm", year])
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    git(&up, &["tag", "-a", "-m", "old", "old", "main~3"]);
    git(&up, &["checkout", "-qb", "topic"]);
    commit(&up, "t", "t\n");
    git(&up, &["tag", "tt"]);
    git(&up, &["checkout", "-q", "main"]);
    git(&up, &["push", "-q", "origin", "main", "topic", "old", "tt"]);
    let url = format!("file://{}", remote.display());
    (remote.parent().unwrap().to_path_buf(), url)
}

/// The refs, history and shallow boundary of two repositories match.
fn same_history(a: &Path, b: &Path) {
    for args in [
        &["for-each-ref"][..],
        &["log", "--all", "--format=%H %s"],
        &["diff", "HEAD", "--stat"],
    ] {
        assert_eq!(git(a, args), git(b, args), "{args:?}");
    }
    let shallow = |d: &Path| std::fs::read_to_string(d.join(".git/shallow")).ok();
    assert_eq!(shallow(a), shallow(b));
}

#[test]
fn shallow_clones_and_fetches_over_file_match_git() {
    let (root, url) = dated_setup("shallow-file");
    for (name, flags) in [
        ("depth", &["--depth", "2"][..]),
        ("since", &["--shallow-since=2021-06-01"]),
        ("wide", &["--depth", "1", "--no-single-branch"]),
    ] {
        let theirs = format!("g{name}");
        let mut args = vec!["clone"];
        args.extend(flags);
        ok(&root, &[&args[..], &[url.as_str(), name]].concat());
        args.push("-q");
        git(
            &root,
            &[&args[..], &[url.as_str(), theirs.as_str()]].concat(),
        );
        same_history(&root.join(name), &root.join(theirs));
    }
    let (mine, theirs) = (root.join("depth"), root.join("gdepth"));
    for step in [
        &["fetch", "--deepen", "1"][..],
        &["fetch", "--shallow-since=2020-01-01"],
        &["fetch", "--depth", "1", "origin", "topic"],
        &["fetch", "--unshallow"],
    ] {
        ok(&mine, step);
        git(&theirs, &[step, &["-q"]].concat());
        same_history(&mine, &theirs);
    }
    assert!(!mine.join(".git/shallow").exists());
}

#[test]
fn clone_borrows_objects_separates_and_starts_sparse_like_git() {
    let (root, url) = dated_setup("clone-native");
    let clone = |flags: &[&str], name: &str| {
        let theirs = format!("g{name}");
        let mut args = vec!["clone"];
        args.extend(flags);
        ok(&root, &[&args[..], &[url.as_str(), name]].concat());
        args.push("-q");
        git(
            &root,
            &[&args[..], &[url.as_str(), theirs.as_str()]].concat(),
        );
        (root.join(name), root.join(theirs))
    };

    // One branch takes only the tags on its history.
    let (a, b) = clone(&["--single-branch"], "single");
    same_history(&a, &b);
    assert!(!git(&a, &["tag"]).contains("tt"));
    let (a, b) = clone(&["-b", "old", "-c", "advice.detachedHead=false"], "tagged");
    assert_eq!(
        git(&a, &["rev-parse", "HEAD"]),
        git(&b, &["rev-parse", "HEAD"])
    );

    // Borrowed objects: --dissociate copies them in and drops the link.
    let up = root.join("up");
    let (a, b) = clone(
        &["--reference", up.to_str().unwrap(), "--dissociate"],
        "dissociated",
    );
    same_history(&a, &b);
    assert!(!a.join(".git/objects/info/alternates").exists());
    git(&a, &["fsck", "--no-progress"]);
    let (_, err, success) = rgit(
        &root,
        &["clone", "--reference-if-able", "nowhere", &url, "able"],
    );
    assert!(success && err.contains("Could not add alternate"), "{err}");

    // The repository lives apart, linked by a .git file.
    let apart = root.join("apart.git");
    let flag = format!("--separate-git-dir={}", apart.display());
    ok(&root, &["clone", &flag, &url, "separate"]);
    assert!(root.join("separate/.git").is_file());
    assert!(apart.join("HEAD").exists());
    assert_eq!(git(&root.join("separate"), &["status", "--porcelain"]), "");

    // A template's hooks are copied in.
    let tpl = root.join("tpl");
    std::fs::create_dir_all(tpl.join("hooks")).unwrap();
    std::fs::write(tpl.join("hooks/post-checkout"), "#!/bin/sh\n").unwrap();
    let flag = format!("--template={}", tpl.display());
    ok(&root, &["clone", &flag, &url, "templated"]);
    assert!(root.join("templated/.git/hooks/post-checkout").exists());

    // A sparse checkout has the top-level files only, as git's has.
    let (a, b) = clone(&["--sparse"], "sparse");
    same_history(&a, &b);
    assert!(!a.join("d").exists());
    for args in [&["ls-files", "-t"][..], &["sparse-checkout", "list"]] {
        assert_eq!(git(&a, args), git(&b, args), "{args:?}");
    }

    // A failed clone leaves nothing behind.
    fails(&root, &["clone", "-b", "nope", &url, "gone"]);
    assert!(!root.join("gone").exists());
}

/// What `sh -c <script>` prints on stdout and stderr together, in `dir`.
fn shell(dir: &Path, script: &str) -> String {
    let out = env(Command::new("sh").arg("-c").arg(script), dir)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{script}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The same fetch by rgit in `clone` and by git in `twin` reports the same.
fn same_fetch(clone: &Path, twin: &Path, args: &[&str]) {
    let (out, err, success) = human(clone, &[&["fetch"], args].concat());
    assert!(success, "fetch {args:?}: {err}");
    let theirs = shell(twin, &format!("git fetch {} 2>&1", args.join(" ")));
    assert_eq!(out, theirs, "fetch {args:?}");
}

#[test]
fn fetch_reports_like_git() {
    let (remote, up, clone) = fetch_setup("fetch-report");
    let twin = clone.with_file_name("g");
    let root = remote.parent().unwrap();
    git(
        root,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            twin.to_str().unwrap(),
        ],
    );
    git(
        &up,
        &["checkout", "-qb", "a-very-long-branch-name-for-the-column"],
    );
    commit(&up, "t", "t\n");
    git(&up, &["checkout", "-qb", "topic"]);
    git(&up, &["checkout", "-q", "main"]);
    commit(&up, "c", "c\n");
    git(&up, &["tag", "-a", "-m", "rel", "rel"]);
    git(&up, &["branch", "doomed"]);
    git(&up, &["push", "-q", "--all", "origin"]);
    git(&up, &["push", "-q", "origin", "rel"]);
    same_fetch(&clone, &twin, &["--dry-run"]);
    same_fetch(&clone, &twin, &[]);

    git(&up, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&up, "forced", "f\n");
    git(&up, &["push", "-q", "-f", "origin", "main", ":doomed"]);
    same_fetch(&clone, &twin, &["--prune"]);

    git(&up, &["checkout", "-q", "topic"]);
    commit(&up, "t2", "t2\n");
    git(&up, &["push", "-q", "origin", "topic"]);
    same_fetch(&clone, &twin, &["origin", "topic"]);
    same_fetch(&clone, &twin, &["origin", "topic:refs/heads/copy"]);

    for dir in [&clone, &twin] {
        git(dir, &["config", "fetch.output", "compact"]);
    }
    git(&up, &["checkout", "-q", "main"]);
    commit(&up, "d", "d\n");
    git(&up, &["push", "-q", "origin", "main"]);
    same_fetch(&clone, &twin, &[]);
}

#[test]
fn fetch_multiple_remote_update_and_groups_fetch_each_remote() {
    let (remote, up, clone) = fetch_setup("fetch-many");
    let twin = clone.with_file_name("g");
    let root = remote.parent().unwrap();
    git(
        root,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            twin.to_str().unwrap(),
        ],
    );
    for dir in [&clone, &twin] {
        git(dir, &["remote", "add", "second", remote.to_str().unwrap()]);
        git(dir, &["config", "remotes.both", "origin second"]);
    }
    commit(&up, "c", "c\n");
    git(&up, &["push", "-q", "origin", "main"]);

    let (out, _, success) = human(&clone, &["remote", "update", "both"]);
    assert!(success, "{out}");
    let theirs = shell(&twin, "git remote update both 2>&1");
    assert_eq!(out, theirs);

    commit(&up, "d", "d\n");
    git(&up, &["push", "-q", "origin", "main"]);
    ok(
        &clone,
        &["fetch", "--multiple", "-j", "2", "origin", "second"],
    );
    git(
        &twin,
        &["fetch", "-q", "--multiple", "-j", "2", "origin", "second"],
    );
    let sorted = |dir: &Path| {
        let mut lines: Vec<String> = std::fs::read_to_string(dir.join(".git/FETCH_HEAD"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort();
        lines
    };
    assert_eq!(sorted(&clone), sorted(&twin));
    assert_eq!(
        git(&clone, &["for-each-ref", "refs/remotes"]),
        git(&twin, &["for-each-ref", "refs/remotes"])
    );

    // A group fetches as `--multiple` does; skipFetchAll leaves one out of --all.
    commit(&up, "e", "e\n");
    git(&up, &["push", "-q", "origin", "main"]);
    ok(&clone, &["fetch", "both"]);
    assert_eq!(
        git(&clone, &["rev-parse", "second/main"]),
        git(&up, &["rev-parse", "main"])
    );
    commit(&up, "f", "f\n");
    git(&up, &["push", "-q", "origin", "main"]);
    git(&clone, &["config", "remote.second.skipFetchAll", "true"]);
    let out = ok(&clone, &["fetch", "--all"]);
    assert!(!out.contains("second"), "{out}");
    assert_ne!(
        git(&clone, &["rev-parse", "second/main"]),
        git(&up, &["rev-parse", "main"])
    );
}

#[test]
fn remotes_with_several_urls_fetch_the_first_and_push_to_each() {
    let (work, origin, second) = push_setup("multi-url");
    let url = |p: &Path| p.to_str().unwrap().to_owned();
    git(
        &work,
        &["remote", "set-url", "--add", "origin", &url(&second)],
    );
    git(&work, &["branch", "gone"]);
    let out = ok(&work, &["push", "origin", "main", "gone"]);
    assert_eq!(
        out.matches("[new branch]      gone -> gone").count(),
        2,
        "{out}"
    );
    assert_eq!(heads(&origin), heads(&second));
    // Like git, the remote-tracking refs follow what was pushed.
    assert_eq!(
        git(&work, &["rev-parse", "origin/gone"]),
        git(&work, &["rev-parse", "gone"])
    );
    ok(&work, &["push", "origin", ":gone"]);
    assert!(!heads(&origin).contains("gone") && !heads(&second).contains("gone"));
    assert!(!git(&work, &["branch", "-r"]).contains("origin/gone"));

    // Fetch reads the first URL only.
    git(&origin, &["branch", "first-only", "main"]);
    git(&second, &["branch", "second-only", "main"]);
    ok(&work, &["fetch", "origin"]);
    let remotes = git(&work, &["branch", "-r"]);
    assert!(remotes.contains("origin/first-only"), "{remotes}");
    assert!(!remotes.contains("second-only"), "{remotes}");

    git(&origin, &["branch", "-D", "first-only"]);
    let out = ok(&work, &["--human", "remote", "prune", "origin"]);
    assert_eq!(out.trim(), "pruned origin/first-only");

    // A mirror push moves the remote-tracking refs, as git's does.
    git(
        &work,
        &["remote", "set-url", "--delete", "origin", &url(&second)],
    );
    commit(&work, "m", "m\n");
    git(&work, &["branch", "x"]);
    ok(&work, &["push", "--mirror", "origin"]);
    assert_eq!(
        git(&work, &["rev-parse", "origin/main", "origin/x"]),
        git(&work, &["rev-parse", "main", "x"])
    );
}

#[test]
fn push_recurse_submodules_checks_and_pushes_them() {
    let (root, up, lib) = submodule_setup("push-sub");
    let lib_bare = root.join("lib.git");
    git(
        &root,
        &[
            "clone",
            "-q",
            "--bare",
            lib.to_str().unwrap(),
            lib_bare.to_str().unwrap(),
        ],
    );
    let sub = up.join("sub");
    identity(&sub);
    git(
        &sub,
        &["remote", "set-url", "origin", lib_bare.to_str().unwrap()],
    );
    git(&sub, &["fetch", "-q", "origin"]);
    commit(&sub, "s", "s\n");
    git(&up, &["commit", "-qam", "bump"]);
    let before = heads(&root.join("r.git"));

    let (_, msg, _) = human(
        &up,
        &["push", "--recurse-submodules=check", "origin", "main"],
    );
    assert!(
        msg.contains(
            "The following submodule paths contain changes that can\n\
             not be found on any remote:\n  sub\n"
        ),
        "{msg}"
    );
    git(&up, &["config", "push.recurseSubmodules", "check"]);
    fails(&up, &["push", "origin", "main"]);
    assert_eq!(heads(&root.join("r.git")), before);

    let (out, err, success) = human(
        &up,
        &["push", "--recurse-submodules=on-demand", "origin", "main"],
    );
    assert!(success, "{out}{err}");
    assert!(out.contains("Pushing submodule 'sub'"), "{out}");
    assert_eq!(
        git(&lib_bare, &["rev-parse", "main"]),
        git(&sub, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&root.join("r.git"), &["rev-parse", "main"]),
        git(&up, &["rev-parse", "HEAD"])
    );
    ok(&up, &["push", "origin", "main"]);
}

/// A `git daemon` serving the repositories under a folder on a free local
/// port, stopped when dropped.
struct Daemon(std::process::Child, u16);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Daemon {
    fn start(base: &Path) -> Daemon {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = env(&mut Command::new("git"), base)
            .args([
                "daemon",
                "--reuseaddr",
                "--export-all",
                "--listen=127.0.0.1",
            ])
            .arg(format!("--port={port}"))
            .arg(format!("--base-path={}", base.display()))
            .arg(base)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..200 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Daemon(child, port)
    }

    fn url(&self, repo: &str) -> String {
        format!("git://127.0.0.1:{}/{repo}", self.1)
    }
}

/// What git keeps of a clone: its config, refs and reflog messages, the
/// objects it has and lacks, and the state of its working tree.
fn clone_state(dir: &Path) -> String {
    let gitdir = match dir.join(".git").is_dir() {
        true => dir.join(".git"),
        false => dir.to_path_buf(),
    };
    let read = |p: &str| std::fs::read_to_string(gitdir.join(p)).unwrap_or_default();
    let mut logs: Vec<String> = walkdir(&gitdir.join("logs"))
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            let msgs: Vec<&str> = text.lines().filter_map(|l| l.split('\t').nth(1)).collect();
            format!("{}: {msgs:?}", p.strip_prefix(&gitdir).unwrap().display())
        })
        .collect();
    logs.sort();
    let mut loose: Vec<String> = walkdir(&gitdir.join("refs"))
        .into_iter()
        .map(|p| p.strip_prefix(&gitdir).unwrap().display().to_string())
        .collect();
    loose.sort();
    let promisors = walkdir(&gitdir.join("objects/pack"))
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "promisor"))
        .count();
    let mut objects: Vec<String> = git(dir, &["rev-list", "--all", "--objects", "--missing=print"])
        .lines()
        .map(str::to_owned)
        .collect();
    objects.sort();
    format!(
        "{}\n{}\nHEAD {}\n{loose:?}\n{logs:?}\n{objects:?}\npromisor packs: {}\n{}",
        read("config"),
        read("packed-refs"),
        read("HEAD"),
        promisors > 0,
        match gitdir == dir {
            true => String::new(),
            false => git(dir, &["status", "--porcelain"]),
        }
    )
}

fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walkdir(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// A remote with branches, a lightweight and an annotated tag, a file in a
/// folder and one too big for `blob:limit=1k`, allowing filters.
fn partial_setup(tag: &str) -> (PathBuf, PathBuf) {
    let (remote, up, _) = fetch_setup(tag);
    std::fs::create_dir_all(up.join("d")).unwrap();
    std::fs::write(up.join("d/big"), "x".repeat(4096)).unwrap();
    git(&up, &["add", "."]);
    git(&up, &["commit", "-qm", "big"]);
    git(&up, &["tag", "light", "main~1"]);
    git(&up, &["tag", "-a", "-m", "v", "v1", "main~2"]);
    git(&up, &["checkout", "-qb", "topic"]);
    commit(&up, "t", "t\n");
    git(&up, &["checkout", "-q", "main"]);
    git(
        &up,
        &["push", "-q", "origin", "main", "topic", "light", "v1"],
    );
    git(&remote, &["config", "uploadpack.allowFilter", "true"]);
    (remote.parent().unwrap().to_path_buf(), up)
}

#[test]
fn clones_write_refs_reflogs_and_config_like_git() {
    let (root, _) = partial_setup("clone-refs");
    let daemon = Daemon::start(&root);
    git(&root, &["init", "-q", "--bare", "-b", "trunk", "empty.git"]);
    let file = format!("file://{}", root.join("r.git").display());
    let plain = root.join("r.git").display().to_string();
    let net = daemon.url("r.git");
    let cases: &[(&str, Vec<&str>)] = &[
        ("plain", vec![&plain]),
        ("file", vec![&file]),
        ("bare", vec!["--bare", &file]),
        ("mirror", vec!["--mirror", &file]),
        ("single", vec!["--single-branch", "-b", "topic", &file]),
        (
            "detached",
            vec!["-b", "v1", "-c", "advice.detachedHead=false", &file],
        ),
        (
            "tagless",
            vec!["--no-tags", "-c", "a.b=c", "-o", "up", &file],
        ),
        ("net", vec![&net]),
    ];
    for (name, args) in cases {
        let theirs = format!("g-{name}");
        ok(&root, &[&["clone"], &args[..], &[name]].concat());
        git(&root, &[&["clone", "-q"], &args[..], &[&theirs]].concat());
        let strip = |s: String| s.replace(root.to_str().unwrap(), "");
        assert_eq!(
            strip(clone_state(&root.join(name))),
            strip(clone_state(&root.join(&theirs))),
            "{name}"
        );
    }
    // An empty remote's HEAD names the branch to start, over the network too.
    for url in [
        format!("file://{}", root.join("empty.git").display()),
        daemon.url("empty.git"),
    ] {
        let _ = std::fs::remove_dir_all(root.join("empty"));
        let _ = std::fs::remove_dir_all(root.join("g-empty"));
        ok(&root, &["clone", &url, "empty"]);
        git(&root, &["clone", "-q", &url, "g-empty"]);
        assert_eq!(
            clone_state(&root.join("empty")),
            clone_state(&root.join("g-empty"))
        );
        assert!(clone_state(&root.join("empty")).contains("refs/heads/trunk"));
    }
}

#[test]
fn partial_clones_fetch_what_they_lack_like_git() {
    let (root, up) = partial_setup("partial");
    let daemon = Daemon::start(&root);
    let file = format!("file://{}", root.join("r.git").display());
    let net = daemon.url("r.git");
    let strip = |s: String| s.replace(root.to_str().unwrap(), "").replace(&net, "URL");
    for (name, url, flags) in [
        ("none", &file, &["--filter=blob:none"][..]),
        ("limit", &file, &["--filter=blob:limit=1k"]),
        ("tree", &file, &["--filter=tree:0"]),
        ("sparse", &file, &["--filter=blob:none", "--sparse"]),
        ("unchecked", &file, &["--filter=blob:none", "-n"]),
        ("shallow", &file, &["--filter=blob:none", "--depth=1"]),
        ("net-none", &net, &["--filter=blob:none"]),
        ("net-tree", &net, &["--filter=tree:0"]),
    ] {
        let theirs = format!("g-{name}");
        ok(&root, &[&["clone"], flags, &[url.as_str(), name]].concat());
        git(
            &root,
            &[&["clone", "-q"], flags, &[url.as_str(), &theirs]].concat(),
        );
        assert_eq!(
            strip(clone_state(&root.join(name))),
            strip(clone_state(&root.join(&theirs))),
            "{name}"
        );
    }
    // What the filter left out is fetched when read.
    for name in ["none", "tree", "net-none"] {
        let dir = root.join(name);
        assert_eq!(ok(&dir, &["--human", "show", "HEAD~2:a"]), "a\n", "{name}");
        assert_eq!(git(&dir, &["cat-file", "-p", "HEAD~2:a"]), "a\n");
    }

    // A later fetch goes through the remote's filter.
    commit(&up, "c", "c\n");
    git(&up, &["push", "-q", "origin", "main"]);
    let objects = |dir: &Path| {
        let mut all: Vec<String> = git(dir, &["rev-list", "--all", "--objects", "--missing=print"])
            .lines()
            .map(str::to_owned)
            .collect();
        all.sort();
        all
    };
    for name in ["none", "net-none"] {
        ok(&root.join(name), &["fetch"]);
        git(&root.join(format!("g-{name}")), &["fetch", "-q"]);
        assert_eq!(
            objects(&root.join(name)),
            objects(&root.join(format!("g-{name}"))),
            "{name}"
        );
    }

    // fetch --filter makes a full clone's remote a promisor.
    ok(&root, &["clone", "-q", &file, "full"]);
    ok(
        &root.join("full"),
        &["fetch", "--filter=blob:limit=2k", "origin"],
    );
    let config = git(
        &root.join("full"),
        &["config", "--get-regexp", "promisor|filter|format"],
    );
    assert_eq!(
        config,
        "core.repositoryformatversion 1\nremote.origin.promisor true\n\
         remote.origin.partialclonefilter blob:limit=2048\n"
    );

    // A server that does not filter sends everything, and git's warnings.
    git(
        &root.join("r.git"),
        &["config", "uploadpack.allowFilter", "false"],
    );
    let (_, err, success) = rgit(&root, &["clone", "--filter=blob:none", &file, "unfiltered"]);
    assert!(
        success && err.contains("filtering not recognized by server, ignoring"),
        "{err}"
    );
    let plain = root.join("r.git").display().to_string();
    let (_, err, success) = rgit(&root, &["clone", "--filter=blob:none", &plain, "local"]);
    assert!(
        success && err.contains("--filter is ignored in local clones"),
        "{err}"
    );
    let msg = fails(&root, &["clone", "--filter=bogus", &file, "bogus"]);
    assert!(msg.contains("invalid filter-spec 'bogus'"), "{msg}");
    assert!(!root.join("bogus").exists());
}

#[test]
fn shallow_since_over_the_network_matches_git() {
    let (root, _) = dated_setup("shallow-net");
    let daemon = Daemon::start(&root);
    let url = daemon.url("r.git");
    ok(
        &root,
        &["clone", "--shallow-since=2021-06-01", &url, "since"],
    );
    git(
        &root,
        &["clone", "-q", "--shallow-since=2021-06-01", &url, "gsince"],
    );
    let (mine, theirs) = (root.join("since"), root.join("gsince"));
    same_history(&mine, &theirs);
    for step in [
        &["fetch", "--shallow-since=2020-06-01"][..],
        &["fetch", "--deepen", "1"],
    ] {
        ok(&mine, step);
        git(&theirs, &[step, &["-q"]].concat());
        same_history(&mine, &theirs);
    }
}
