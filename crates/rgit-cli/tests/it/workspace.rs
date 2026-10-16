//! `rgit workspace`: a copy-on-write clone is created on a new branch, is
//! independent from the source, and can be listed and removed.

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn rgit(cwd: &Path, ws_dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(
            ["--human", "--text", "--json", "--toon", "--axi"]
                .iter()
                .all(|m| !args.contains(m))
                .then_some("--toon"),
        )
        .args(args)
        .current_dir(cwd)
        .env("RGIT_WORKSPACE_DIR", ws_dir)
        .env("RGIT_OPLOG", "0")
        .env("RGIT_OUTPUT", "human")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

#[test]
fn workspace_create_isolate_list_remove() {
    let base = std::env::temp_dir().join(format!("rgit-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let ws_dir = base.join("workspaces");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("code.txt"), "source\n").unwrap();
    git(&repo, &["add", "code.txt"]);
    git(&repo, &["commit", "-qm", "init"]);

    let (out, ok) = rgit(&repo, &ws_dir, &["workspace", "new", "agent1"]);
    assert!(ok, "workspace new should succeed: {out}");
    assert!(out.contains("created workspace agent1"), "{out}");

    let clone = ws_dir.join("agent1");
    assert!(clone.join(".git").exists(), "clone should be a repo");
    assert_eq!(
        std::fs::read_to_string(clone.join("code.txt")).unwrap(),
        "source\n"
    );

    // Copy-on-write independence: editing the clone leaves the source unchanged.
    std::fs::write(clone.join("code.txt"), "changed\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.join("code.txt")).unwrap(),
        "source\n",
        "source must be untouched by clone edits"
    );

    let (list, ok) = rgit(&repo, &ws_dir, &["workspace", "list"]);
    assert!(
        ok && list.contains("agent1"),
        "list should show agent1: {list}"
    );

    let (_, ok) = rgit(&repo, &ws_dir, &["workspace", "remove", "agent1"]);
    assert!(ok && !clone.exists(), "remove should delete the workspace");

    let _ = std::fs::remove_dir_all(&base);
}

fn scratch_repo(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("rgit-ws-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let ws_dir = base.join("workspaces");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("code.txt"), "source\n").unwrap();
    git(&repo, &["add", "code.txt"]);
    git(&repo, &["commit", "-qm", "init"]);
    (base, repo, ws_dir)
}

fn commit_file(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", file]);
}

fn head(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn workspace_pull_fast_forwards_main() {
    let (base, repo, ws_dir) = scratch_repo("pull");
    let (_, ok) = rgit(&repo, &ws_dir, &["workspace", "new", "feat"]);
    assert!(ok);
    let clone = ws_dir.join("feat");
    commit_file(&clone, "new.txt", "from workspace\n");
    let before = head(&repo);

    let (out, ok) = rgit(
        &repo,
        &ws_dir,
        &["workspace", "pull", "feat", "--into", "main"],
    );
    assert!(ok, "{out}");
    assert!(out.contains("workspace: feat"), "{out}");
    assert!(out.contains("branch: main"), "{out}");
    assert!(out.contains(&format!("from: {before}")), "{out}");
    assert!(out.contains(&format!("to: {}", head(&clone))), "{out}");
    assert!(out.contains("commits: 1"), "{out}");
    assert_eq!(head(&repo), head(&clone));
    assert!(repo.join("new.txt").exists());
    let (status, _) = rgit(&repo, &ws_dir, &["status", "--short"]);
    assert!(
        !status.contains("new.txt"),
        "tree should be clean: {status}"
    );

    let (again, ok) = rgit(
        &repo,
        &ws_dir,
        &["workspace", "pull", "feat", "--into", "main"],
    );
    assert!(ok && again.contains("commits: 0"), "{again}");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn workspace_pull_creates_the_same_named_branch() {
    let (base, repo, ws_dir) = scratch_repo("create");
    rgit(&repo, &ws_dir, &["workspace", "new", "feat"]);
    commit_file(&ws_dir.join("feat"), "new.txt", "x\n");

    let (out, ok) = rgit(&repo, &ws_dir, &["workspace", "pull", "feat"]);
    assert!(
        ok && out.contains("branch: feat") && out.contains("commits: 1"),
        "{out}"
    );
    let tip = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "feat"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&tip.stdout).trim(),
        head(&ws_dir.join("feat"))
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn workspace_pull_refuses_a_diverged_branch() {
    let (base, repo, ws_dir) = scratch_repo("diverged");
    rgit(&repo, &ws_dir, &["workspace", "new", "feat"]);
    commit_file(&ws_dir.join("feat"), "ws.txt", "w\n");
    commit_file(&repo, "main1.txt", "1\n");
    commit_file(&repo, "main2.txt", "2\n");
    let before = head(&repo);

    let (out, ok) = rgit(
        &repo,
        &ws_dir,
        &["workspace", "pull", "feat", "--into", "main"],
    );
    assert!(!ok, "{out}");
    assert!(
        out.contains("error: main has 2 commits the workspace does not"),
        "{out}"
    );
    assert!(out.contains("rgit workspace push feat"), "{out}");
    assert_eq!(head(&repo), before);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn workspace_pull_refuses_uncommitted_changes() {
    let (base, repo, ws_dir) = scratch_repo("dirty");
    rgit(&repo, &ws_dir, &["workspace", "new", "feat"]);
    commit_file(&ws_dir.join("feat"), "ws.txt", "w\n");
    std::fs::write(repo.join("code.txt"), "edited\n").unwrap();
    let before = head(&repo);

    let (out, ok) = rgit(
        &repo,
        &ws_dir,
        &["workspace", "pull", "feat", "--into", "main"],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("uncommitted changes on main"), "{out}");
    assert_eq!(head(&repo), before);
    assert_eq!(
        std::fs::read_to_string(repo.join("code.txt")).unwrap(),
        "edited\n"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn workspace_push_brings_the_workspace_up_to_date() {
    let (base, repo, ws_dir) = scratch_repo("push");
    rgit(&repo, &ws_dir, &["workspace", "new", "feat"]);
    let clone = ws_dir.join("feat");
    git(&repo, &["branch", "-f", "feat", "main"]);
    commit_file(&repo, "m.txt", "m\n");
    git(&repo, &["branch", "-f", "feat", "main"]);

    let (list, _) = rgit(&repo, &ws_dir, &["workspace", "list"]);
    assert!(list.contains("behind") && list.contains("ahead"), "{list}");

    let (out, ok) = rgit(&repo, &ws_dir, &["workspace", "push", "feat"]);
    assert!(ok && out.contains("commits: 1"), "{out}");
    assert_eq!(head(&clone), head(&repo));
    assert!(clone.join("m.txt").exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn workspace_list_counts_unlanded_commits() {
    let (base, repo, ws_dir) = scratch_repo("list");
    rgit(&repo, &ws_dir, &["workspace", "new", "feat"]);
    git(&repo, &["branch", "-f", "feat", "main"]);
    commit_file(&ws_dir.join("feat"), "a.txt", "a\n");
    commit_file(&ws_dir.join("feat"), "b.txt", "b\n");
    commit_file(&repo, "m.txt", "m\n");
    git(&repo, &["branch", "-f", "feat", "main"]);

    let (list, ok) = rgit(&repo, &ws_dir, &["workspace", "list", "--json"]);
    assert!(ok, "{list}");
    let json: serde_json::Value = serde_json::from_str(&list).unwrap();
    let row = &json["workspaces"][0];
    assert_eq!(
        (row["ahead"].as_i64(), row["behind"].as_i64()),
        (Some(2), Some(1)),
        "{list}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn fetch_and_pull_accept_a_path() {
    let (base, repo, ws_dir) = scratch_repo("path");
    rgit(&repo, &ws_dir, &["workspace", "new", "feat"]);
    let clone = ws_dir.join("feat");
    commit_file(&clone, "new.txt", "x\n");
    let path = clone.to_str().unwrap();

    let (out, ok) = rgit(&repo, &ws_dir, &["remote", "add", "wsr", path]);
    assert!(ok, "{out}");
    let (named, ok) = rgit(&repo, &ws_dir, &["--human", "fetch", "wsr", "feat"]);
    assert!(ok, "{named}");
    let (by_path, ok) = rgit(&repo, &ws_dir, &["--human", "fetch", path, "feat"]);
    assert!(ok, "{by_path}");
    fn shape(text: &str) -> Vec<&str> {
        text.lines().nth(1).unwrap().split_whitespace().collect()
    }
    assert_eq!(shape(&named), shape(&by_path), "{named}|{by_path}");
    assert!(by_path.starts_with(&format!("From {path}")), "{by_path}");

    let (out, ok) = rgit(&repo, &ws_dir, &["pull", "--ff-only", path, "feat"]);
    assert!(ok, "{out}");
    assert_eq!(head(&repo), head(&clone));
    let (config, _) = rgit(&repo, &ws_dir, &["--human", "remote"]);
    assert_eq!(config.trim(), "wsr", "no stray remotes: {config}");

    let _ = std::fs::remove_dir_all(&base);
}
