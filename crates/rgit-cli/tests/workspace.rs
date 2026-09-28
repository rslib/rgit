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
