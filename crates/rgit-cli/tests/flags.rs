//! End-to-end coverage of the git-compatible CLI flags: spawn the real `rgit`
//! binary and assert observable git state or its printed output. This exercises
//! the clap parsing plus dispatch wiring, complementing the backend unit tests.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run `rgit <args>` in `dir`. Returns (stdout, stderr, success).
fn rgit(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-flags-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    dir
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
}

fn commit(dir: &Path, name: &str, body: &str, msg: &str) {
    write(dir, name, body);
    git(dir, &["add", name]);
    git(dir, &["commit", "-qm", msg]);
}

#[test]
fn log_limit_and_path_filter() {
    let dir = repo("log");
    commit(&dir, "a", "a\n", "add a");
    commit(&dir, "b", "b\n", "add b");
    commit(&dir, "a", "a2\n", "edit a");

    let (out, _, ok) = rgit(&dir, &["log", "-n", "2"]);
    assert!(ok);
    assert_eq!(out.trim().lines().count(), 2, "-n 2 shows two commits");

    // `-- a` keeps only commits that touched file a.
    let (out, _, ok) = rgit(&dir, &["log", "--", "a"]);
    assert!(ok);
    assert!(out.contains("edit a") && out.contains("add a"));
    assert!(!out.contains("add b"), "b-only commit is filtered out");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn branch_delete_safety() {
    let dir = repo("branchdel");
    commit(&dir, "f", "base\n", "base");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "f", "work\n", "feat work");
    git(&dir, &["checkout", "-q", "main"]);

    // -d refuses an unmerged branch; -D forces it.
    let (_, _, ok) = rgit(&dir, &["branch", "delete", "feat"]);
    assert!(!ok, "unmerged delete without force should fail");
    let (_, _, ok) = rgit(&dir, &["branch", "delete", "-D", "feat"]);
    assert!(ok, "force delete succeeds");
    assert!(!git_out(&dir, &["branch"]).contains("feat"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkout_dash_b_creates_and_switches() {
    let dir = repo("cob");
    commit(&dir, "f", "x\n", "init");
    let (_, _, ok) = rgit(&dir, &["checkout", "-b", "feature"]);
    assert!(ok);
    assert_eq!(
        git_out(&dir, &["branch", "--show-current"]).trim(),
        "feature"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commit_all_stages_tracked_only() {
    let dir = repo("commit-a");
    commit(&dir, "a", "a\n", "init");
    write(&dir, "a", "a2\n"); // tracked, modified
    write(&dir, "u", "u\n"); // untracked
    let (_, _, ok) = rgit(&dir, &["commit", "-a", "-m", "all"]);
    assert!(ok);
    // Untracked file must remain untracked (not committed).
    assert!(git_out(&dir, &["status", "--porcelain"]).contains("?? u"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn diff_cached_vs_unstaged() {
    let dir = repo("diff");
    commit(&dir, "a", "a\n", "init");
    write(&dir, "a", "a2\n");
    git(&dir, &["add", "a"]);
    write(&dir, "b", "b\n"); // unstaged new file (untracked shows in neither ref diff)
    write(&dir, "a", "a3\n"); // now a has staged (a2) and unstaged (a3)

    let (cached, _, _) = rgit(&dir, &["diff", "--cached", "--name-only"]);
    assert!(cached.contains('a'), "staged diff lists a");
    let (unstaged, _, _) = rgit(&dir, &["diff", "--name-only"]);
    assert!(unstaged.contains('a'), "bare diff (unstaged) lists a");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stash_untracked_default_off() {
    let dir = repo("stash");
    commit(&dir, "a", "a\n", "init");
    write(&dir, "a", "a2\n");
    write(&dir, "u", "u\n");
    rgit(&dir, &["stash", "push"]);
    assert!(dir.join("u").exists(), "bare stash keeps untracked u");
    rgit(&dir, &["stash", "pop"]);
    write(&dir, "a", "a3\n");
    rgit(&dir, &["stash", "push", "-u"]);
    assert!(!dir.join("u").exists(), "stash -u stashes untracked u");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clean_dry_run_keeps_files() {
    let dir = repo("clean");
    commit(&dir, "a", "a\n", "init");
    write(&dir, "junk", "junk\n");
    let (out, _, ok) = rgit(&dir, &["clean", "-n"]);
    assert!(ok);
    assert!(out.contains("junk"), "dry-run lists junk");
    assert!(dir.join("junk").exists(), "dry-run does not delete");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_ff_only_refuses_divergence() {
    let dir = repo("mergeff");
    commit(&dir, "f", "base\n", "base");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "g", "g\n", "on feat");
    git(&dir, &["checkout", "-q", "main"]);
    commit(&dir, "h", "h\n", "on main"); // diverge
    let (_, _, ok) = rgit(&dir, &["merge", "--ff-only", "feat"]);
    assert!(!ok, "ff-only merge of a diverged branch must fail");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tag_git_style() {
    let dir = repo("tag");
    commit(&dir, "f", "x\n", "init");
    assert!(rgit(&dir, &["tag", "v1.0"]).2, "tag <name> creates");
    assert!(rgit(&dir, &["tag", "v2.0", "-m", "two"]).2, "annotated tag");
    let (list, _, _) = rgit(&dir, &["tag"]);
    assert!(list.contains("v1.0") && list.contains("v2.0"));
    assert!(rgit(&dir, &["tag", "-d", "v2.0"]).2, "tag -d deletes");
    assert!(!rgit(&dir, &["tag"]).0.contains("v2.0"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rm_cached_keeps_file() {
    let dir = repo("rm");
    commit(&dir, "a", "a\n", "init");
    let (_, _, ok) = rgit(&dir, &["rm", "--cached", "a"]);
    assert!(ok);
    assert!(dir.join("a").exists(), "--cached keeps the working file");
    assert!(git_out(&dir, &["status", "--porcelain"]).contains("?? a"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mv_force_required_to_overwrite() {
    let dir = repo("mv");
    commit(&dir, "a", "a\n", "a");
    commit(&dir, "b", "b\n", "b");
    let (_, _, ok) = rgit(&dir, &["mv", "a", "b"]);
    assert!(!ok, "mv onto existing without -f fails");
    let (_, _, ok) = rgit(&dir, &["mv", "a", "b", "-f"]);
    assert!(ok, "mv -f overwrites");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reset_paths_unstages() {
    let dir = repo("reset");
    commit(&dir, "a", "a\n", "init");
    write(&dir, "a", "a2\n");
    write(&dir, "b", "b\n");
    git(&dir, &["add", "a", "b"]);
    let (_, _, ok) = rgit(&dir, &["reset", "--", "a"]);
    assert!(ok);
    let staged = git_out(&dir, &["diff", "--cached", "--name-only"]);
    assert!(
        !staged.contains('a') && staged.contains('b'),
        "only a unstaged"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn show_patch_and_name_only() {
    let dir = repo("show");
    commit(&dir, "a", "line\n", "init");
    let (patch, _, _) = rgit(&dir, &["show", "-p", "HEAD"]);
    assert!(patch.contains("+line"), "show -p prints the patch");
    let (names, _, _) = rgit(&dir, &["show", "--name-only", "HEAD"]);
    assert!(names.trim() == "a", "show --name-only lists files");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn branch_all_lists_remotes() {
    let base = std::env::temp_dir().join(format!("rgit-flags-{}-brall", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let remote = base.join("remote.git");
    let work = base.join("work");
    git(
        &base,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    git(
        &base,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    commit(&work, "f", "x\n", "init");
    git(&work, &["push", "-q", "-u", "origin", "main"]);

    let (out, _, ok) = rgit(&work, &["branch", "-r"]);
    assert!(ok);
    assert!(out.contains("origin/main"), "branch -r lists origin/main");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn describe_dirty_is_opt_in() {
    let dir = repo("describe");
    commit(&dir, "f", "x\n", "init");
    rgit(&dir, &["tag", "v1", "-m", "one"]);
    write(&dir, "f", "dirty\n"); // modify tracked, unstaged
    let (plain, _, _) = rgit(&dir, &["describe"]);
    assert!(!plain.trim().ends_with("-dirty"), "default is not dirty");
    let (dirty, _, _) = rgit(&dir, &["describe", "--dirty"]);
    assert!(dirty.trim().ends_with("-dirty"), "--dirty appends -dirty");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remote_set_url_and_rename() {
    let dir = repo("remote");
    commit(&dir, "f", "x\n", "init");
    rgit(
        &dir,
        &["remote", "add", "origin", "https://example.com/a.git"],
    );
    assert!(
        rgit(
            &dir,
            &["remote", "set-url", "origin", "https://example.com/b.git"]
        )
        .2
    );
    assert!(rgit(&dir, &["remote"]).0.contains("b.git"));
    assert!(rgit(&dir, &["remote", "rename", "origin", "upstream"]).2);
    assert!(rgit(&dir, &["remote"]).0.contains("upstream"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cherry_pick_no_commit_leaves_staged() {
    let dir = repo("cp");
    commit(&dir, "f", "base\n", "base");
    git(&dir, &["checkout", "-qb", "feat"]);
    commit(&dir, "f", "base\nplus\n", "feat");
    let feat = git_out(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&dir, &["checkout", "-q", "main"]);
    let before = git_out(&dir, &["rev-parse", "HEAD"]).trim().to_owned();

    let (_, _, ok) = rgit(&dir, &["cherry-pick", "-n", &feat]);
    assert!(ok);
    let after = git_out(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(before, after, "-n does not create a commit");
    assert!(
        !git_out(&dir, &["diff", "--cached", "--name-only"])
            .trim()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rebase_onto_replays_range() {
    let dir = repo("rebonto");
    commit(&dir, "f", "base\n", "base");
    git(&dir, &["checkout", "-qb", "feature"]);
    commit(&dir, "f1", "1\n", "f1");
    commit(&dir, "f2", "2\n", "f2");
    git(&dir, &["checkout", "-q", "main"]);
    commit(&dir, "g", "g\n", "newbase");
    let newbase = git_out(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&dir, &["checkout", "-q", "feature"]);

    let (_, _, ok) = rgit(&dir, &["rebase", "--onto", "main", "feature~2"]);
    assert!(ok, "rebase --onto succeeds");
    // newbase is now an ancestor of the rebased feature tip.
    let merge_base = git_out(&dir, &["merge-base", "HEAD", &newbase]);
    assert_eq!(
        merge_base.trim(),
        newbase,
        "feature now sits on the new base"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prune_is_object_prune_branch_prune_is_separate() {
    let dir = repo("prune");
    commit(&dir, "f", "x\n", "init");
    // Top-level prune does git's object prune and succeeds on a clean repo.
    let (_, _, ok) = rgit(&dir, &["prune", "-n"]);
    assert!(ok, "prune (objects) runs");
    // Branch cleanup lives under `branch prune`.
    git(&dir, &["branch", "merged"]); // merged into HEAD (points at HEAD)
    let (out, _, ok) = rgit(&dir, &["branch", "prune"]);
    assert!(ok, "branch prune runs: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_uses_default_branch_config() {
    let dir = std::env::temp_dir().join(format!("rgit-default-branch-{}", std::process::id()));
    let cfg = dir.with_extension("gitconfig");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&cfg);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&cfg, "[init]\n\tdefaultBranch = trunk\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("init")
        .current_dir(&dir)
        .env("GIT_CONFIG_GLOBAL", &cfg)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        git_out(&dir, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "trunk"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&cfg);
}
