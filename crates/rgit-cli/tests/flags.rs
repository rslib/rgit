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
    let mut command = Command::new(env!("CARGO_BIN_EXE_rgit"));
    if !args
        .iter()
        .any(|a| matches!(*a, "--json" | "--toon" | "--axi"))
    {
        command.arg("--human");
    }
    let out = command
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

    let (out, _, ok) = rgit(&dir, &["log", "--compact", "-n", "2"]);
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
    let (names, _, _) = rgit(&dir, &["show", "--compact", "--name-only", "HEAD"]);
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
    assert!(rgit(&dir, &["remote", "-v"]).0.contains("b.git"));
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
fn skills_install_project_writes_portable_and_claude() {
    let dir = std::env::temp_dir().join(format!("rgit-skills-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (out, err, ok) = rgit(&dir, &["skills", "install", "--project"]);
    assert!(ok, "skills install failed: {err}");
    assert!(out.contains(".agents/skills/rgit/SKILL.md"));
    assert!(out.contains(".claude/skills/rgit/SKILL.md"));

    let agents = std::fs::read_to_string(dir.join(".agents/skills/rgit/SKILL.md")).unwrap();
    let claude = std::fs::read_to_string(dir.join(".claude/skills/rgit/SKILL.md")).unwrap();
    assert!(agents.contains("name: rgit"));
    assert_eq!(agents, claude);

    let (out, err, ok) = rgit(&dir, &["skills", "install", "--project"]);
    assert!(ok, "skills reinstall failed: {err}");
    assert!(
        !out.contains("installed"),
        "reinstall must be a no-op: {out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn json_wraps_cli_output() {
    let dir = repo("json");
    commit(&dir, "f", "x\n", "init");

    let (out, err, ok) = rgit(&dir, &["--json", "status"]);
    assert!(ok, "json status failed: {err}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["ok"], true);
    assert!(value["files"].to_string().contains("clean"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_status_refuses_auto_init_without_prompt() {
    let dir = std::env::temp_dir().join(format!("rgit-auto-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let (out, err, ok) = rgit(&dir, &["--toon", "status"]);
    assert!(!ok, "porcelain status should fail outside a repo: {err}");
    assert!(out.starts_with("error: no git repository found\n"));
    assert!(out.contains("Run `rgit --toon init`"));
    assert!(!dir.join(".git").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn axi_alias_prints_toon() {
    let dir = repo("axi");
    commit(&dir, "f", "x\n", "init");

    let (out, err, ok) = rgit(&dir, &["--axi", "status"]);
    assert!(ok, "axi status failed: {err}");
    assert!(out.starts_with("branch: "));
    assert!(out.contains("clean"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_non_tty_prints_human_text() {
    let dir = repo("default-human");
    commit(&dir, "f", "x\n", "init");

    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("status")
        .current_dir(&dir)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout,
        "On branch main\nnothing to commit, working tree clean\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toon_flag_prints_toon() {
    let dir = repo("toon");
    commit(&dir, "f", "x\n", "init");

    let (out, err, ok) = rgit(&dir, &["--toon", "status"]);
    assert!(ok, "toon status failed: {err}");
    assert!(out.starts_with("branch: "));
    assert!(out.contains("clean"));

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

#[test]
fn porcelain_unknown_flag_is_usage_error_on_stdout() {
    let dir = repo("unknown-flag");
    commit(&dir, "f", "x\n", "init");

    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["--toon", "diff", "--stat2"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("error: "), "{stdout}");
    assert!(stdout.contains("valid flags for `rgit diff`"), "{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_missing_arg_exits_2() {
    let dir = repo("missing-arg");
    commit(&dir, "f", "x\n", "init");

    let (out, _, ok) = rgit(&dir, &["--toon", "branch", "delete"]);
    assert!(!ok);
    assert!(out.starts_with("error: a branch name required"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_args_non_tty_prints_status() {
    let dir = repo("home-human");
    commit(&dir, "f", "x\n", "init");
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .current_dir(&dir)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "On branch main\nnothing to commit, working tree clean\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toon_no_args_prints_home_view() {
    let dir = repo("home");
    commit(&dir, "f", "x\n", "init");

    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("--toon")
        .current_dir(&dir)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("bin: "), "{stdout}");
    assert!(stdout.contains("description: "));
    assert!(stdout.contains("clean"));
    assert!(stdout.contains("help["));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn short_version_prints_bare_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("-v")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn porcelain_list_has_schema_count_and_fields() {
    let dir = repo("schema");
    commit(&dir, "f", "x\n", "one");
    commit(&dir, "f", "y\n", "two");
    commit(&dir, "f", "z\n", "three");

    let (out, err, ok) = rgit(&dir, &["--toon", "log", "--limit", "2"]);
    assert!(ok, "log failed: {err}");
    assert!(out.contains("count: 2 of 3 total"), "{out}");
    assert!(out.contains("commits[2]{id,summary,author,when}:"), "{out}");
    assert!(out.contains("rgit --toon log --limit 3"), "{out}");

    let (out, _, ok) = rgit(&dir, &["--toon", "log", "--fields", "oid"]);
    assert!(ok);
    assert!(out.contains("{id,summary,author,when,oid}"), "{out}");

    let (out, _, ok) = rgit(&dir, &["--toon", "log", "--fields", "nope"]);
    assert!(!ok);
    assert!(out.starts_with("error: unknown field nope"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_empty_list_is_explicit() {
    let dir = repo("empty-list");
    commit(&dir, "f", "x\n", "init");

    let (out, _, ok) = rgit(&dir, &["--toon", "tag"]);
    assert!(ok);
    assert!(out.starts_with("tags: 0 tags"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_mutations_are_idempotent() {
    let dir = repo("idempotent");
    commit(&dir, "f", "x\n", "init");

    let (out, _, ok) = rgit(&dir, &["--toon", "branch", "create", "topic"]);
    assert!(ok, "{out}");
    let (out, _, ok) = rgit(&dir, &["--toon", "branch", "create", "topic"]);
    assert!(ok && out.contains("already exists (no-op)"), "{out}");
    let (out, _, ok) = rgit(&dir, &["--toon", "branch", "delete", "gone"]);
    assert!(ok && out.contains("does not exist (no-op)"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_truncates_long_body_with_full_hint() {
    let dir = repo("truncate");
    let body = "b".repeat(3000);
    commit(&dir, "f", "x\n", &format!("subject\n\n{body}"));

    let (out, _, ok) = rgit(&dir, &["--toon", "show", "HEAD"]);
    assert!(ok);
    assert!(out.contains("(truncated, 3000 chars total)"), "{out}");
    assert!(out.contains("rgit --full --toon show HEAD"), "{out}");

    let (out, _, ok) = rgit(&dir, &["--toon", "show", "HEAD", "--full"]);
    assert!(ok);
    assert!(!out.contains("truncated"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_renamed_git_spellings_get_targeted_hints() {
    let dir = repo("renamed");
    commit(&dir, "f", "x\n", "init");

    let (out, _, ok) = rgit(&dir, &["--toon", "stash", "save", "x"]);
    assert!(!ok);
    assert!(
        out.contains("`rgit --toon stash push -m <message>`"),
        "{out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_mutations_say_what_they_did() {
    let dir = repo("say-done");
    commit(&dir, "f", "x\n", "init");
    write(&dir, "f", "y\n");

    let (out, _, ok) = rgit(&dir, &["--toon", "stage", "f"]);
    assert!(ok);
    assert!(out.starts_with("result: staged f"), "{out}");
    let branch = git_out(&dir, &["symbolic-ref", "--short", "HEAD"]);
    let (out, _, ok) = rgit(&dir, &["--toon", "checkout", branch.trim()]);
    assert!(ok);
    assert!(out.contains("(no-op)"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_long_plain_output_is_capped() {
    let dir = repo("cap");
    for i in 0..250 {
        write(&dir, &format!("f{i}"), "x\n");
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "many"]);

    let (out, _, ok) = rgit(&dir, &["--toon", "git", "ls-files"]);
    assert!(ok);
    assert!(out.contains("count: 200 of 250 lines"), "{out}");
    assert!(
        out.contains("rgit --full --toon git ls-files` to see all 250 lines"),
        "{out}"
    );
    let (out, _, ok) = rgit(&dir, &["--toon", "--full", "git", "ls-files"]);
    assert!(ok);
    assert!(out.starts_with("lines[250]"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_columns_keep_schema_order() {
    let dir = repo("order");
    commit(&dir, "f", "x\n", "init");

    let (out, _, ok) = rgit(&dir, &["--toon", "log", "--fields", "unpushed,oid"]);
    assert!(ok);
    assert!(
        out.contains("commits[1]{id,summary,author,when,oid,unpushed}:"),
        "{out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn porcelain_lanes_off_is_an_explicit_empty_state() {
    let dir = repo("lanes-off");
    commit(&dir, "f", "x\n", "init");

    let (out, _, ok) = rgit(&dir, &["--toon", "lanes"]);
    assert!(ok, "{out}");
    assert!(out.starts_with("lanes: 0 lanes"), "{out}");
    assert!(out.contains("rgit --toon lanes init"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn version_fast_path_is_near_process_floor() {
    let best = |cmd: &mut dyn FnMut() -> std::process::Output| {
        (0..5)
            .map(|_| {
                let start = std::time::Instant::now();
                assert!(cmd().status.success());
                start.elapsed()
            })
            .min()
            .unwrap()
    };
    let floor = best(&mut || Command::new("true").output().unwrap());
    let version = best(&mut || {
        Command::new(env!("CARGO_BIN_EXE_rgit"))
            .arg("--version")
            .output()
            .unwrap()
    });
    assert!(
        version < floor + std::time::Duration::from_millis(50),
        "--version took {version:?}, process floor {floor:?}"
    );
}

#[test]
fn status_porcelain_matches_git_in_every_mode() {
    let dir = repo("status-porcelain");
    commit(&dir, "f", "x\n", "init");
    write(&dir, "f", "y\n");
    write(&dir, "new", "n\n");

    let want = git_out(&dir, &["status", "--porcelain"]);
    for mode in ["--human", "--toon", "--json"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
            .args([mode, "status", "--porcelain"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "mode {mode}");
    }
    let short = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["status", "-sb"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&short.stdout),
        git_out(&dir, &["status", "-sb"])
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn status_shows_rename_source_and_staged_untrack() {
    let dir = repo("status-rename");
    commit(&dir, "a", "x\n", "init");
    write(&dir, "d", "d\n");
    git(&dir, &["add", "d"]);
    git(&dir, &["commit", "-qm", "add d"]);
    git(&dir, &["mv", "a", "a2"]);
    git(&dir, &["rm", "-q", "--cached", "d"]);

    let (out, err, ok) = rgit(&dir, &["status"]);
    assert!(ok, "{err}");
    assert!(out.contains("a -> a2"), "{out}");

    let (out, _, ok) = rgit(&dir, &["--toon", "status"]);
    assert!(ok);
    assert!(out.contains("\n  d,D,?"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toon_mutations_suggest_the_next_step() {
    let dir = repo("next-steps");
    commit(&dir, "f", "x\n", "init");

    let (out, _, ok) = rgit(&dir, &["--toon", "branch", "create", "topic"]);
    assert!(
        ok && out.contains("created and checked out branch topic"),
        "{out}"
    );
    assert!(
        out.contains("Run `rgit --toon push --set-upstream` to publish topic"),
        "{out}"
    );
    write(&dir, "f", "y\n");
    let (out, _, ok) = rgit(&dir, &["--toon", "stash"]);
    assert!(ok && out.contains("Run `rgit --toon stash pop`"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toon_fields_on_empty_table_and_blame_missing_file() {
    let dir = repo("empty-fields");
    commit(&dir, "f", "x\n", "init");

    let (out, _, ok) = rgit(&dir, &["--toon", "status", "--fields", "from"]);
    assert!(ok, "{out}");
    assert!(out.contains("files: 0 changes"), "{out}");
    let (out, _, ok) = rgit(&dir, &["--toon", "blame", "nope.rs"]);
    assert!(!ok);
    assert!(out.starts_with("error: no file nope.rs"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Run `program args` in a fixed-date, config-free environment; stdout.
fn fixed(program: &str, dir: &Path, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2024-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2024-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo with a merge (conflict resolved by hand) and uncommitted changes.
fn sample(tag: &str) -> PathBuf {
    let dir = repo(tag);
    let g = |args: &[&str]| fixed("git", &dir, args);
    write(&dir, "f", "1\n2\n3\n");
    g(&["add", "f"]);
    g(&["commit", "-qm", "base"]);
    g(&["checkout", "-qb", "side"]);
    write(&dir, "f", "1\n2\nS\n");
    g(&["commit", "-qam", "side"]);
    g(&["checkout", "-q", "main"]);
    write(&dir, "f", "M\n2\n3\n");
    g(&["commit", "-qam", "main"]);
    g(&["merge", "-q", "--no-edit", "side"]);
    write(&dir, "f", "M\n2\nX\n");
    g(&["commit", "-q", "--amend", "-a", "--no-edit"]);
    write(&dir, "f", "M\n2\nX\nnew\n");
    write(&dir, "u", "untracked\n");
    dir
}

#[test]
fn plain_diff_show_log_status_blame_match_git() {
    let dir = sample("git-defaults");
    let rgit = env!("CARGO_BIN_EXE_rgit");
    for args in [
        &["diff"][..],
        &["show"],
        &["show", "HEAD~1"],
        &["log"],
        &["log", "-p"],
        &["status"],
        &["blame", "f"],
    ] {
        assert_eq!(
            fixed(rgit, &dir, args),
            fixed("git", &dir, args),
            "{args:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compact_flag_and_config_restore_rgit_forms() {
    let dir = sample("compact");
    let rgit = env!("CARGO_BIN_EXE_rgit");
    let log = fixed(rgit, &dir, &["log", "--compact"]);
    assert_eq!(log.lines().count(), 4, "{log}");
    assert!(log.lines().all(|l| !l.starts_with("commit ")), "{log}");
    for cmd in ["status", "diff", "log", "show", "blame"] {
        let mut args = vec![cmd];
        if cmd == "blame" {
            args.push("f");
        }
        let compact = fixed(rgit, &dir, &[&["--compact"][..], &args].concat());
        let config = fixed(
            rgit,
            &dir,
            &[&["-c", "rgit.compact=true"][..], &args].concat(),
        );
        assert_eq!(compact, config, "{cmd}");
        assert_ne!(compact, fixed("git", &dir, &args), "{cmd}");
    }
    fixed("git", &dir, &["config", "rgit.compact", "true"]);
    assert_eq!(fixed(rgit, &dir, &["log"]), log);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn piped_output_without_flags_is_human_and_toon_is_opt_in() {
    let dir = sample("piped");
    let rgit = env!("CARGO_BIN_EXE_rgit");
    let human = fixed(rgit, &dir, &["branch"]);
    assert_eq!(human, "* main\n  side\n");
    let toon = fixed(rgit, &dir, &["--toon", "branch"]);
    assert!(toon.starts_with("branches[2]{name,current}:"), "{toon}");
    assert_eq!(fixed(rgit, &dir, &["--axi", "branch"]), toon);
    assert!(fixed(rgit, &dir, &["--json", "branch"]).starts_with('{'));
    assert_eq!(fixed(rgit, &dir, &["--human", "branch"]), human);
    let _ = std::fs::remove_dir_all(&dir);
}
