//! `rgit flow`: the gitflow preset creates an integration branch, starts a
//! prefixed feature off it, and finishes by merging back and deleting.

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

fn branches(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["branch", "--format=%(refname:short)"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).replace('\n', " ")
}

#[test]
fn gitflow_start_and_finish_a_feature() {
    let dir = std::env::temp_dir().join(format!("rgit-flow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("f.txt"), "base\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "init"]);

    let (out, ok) = rgit(&dir, &["flow", "init", "gitflow"]);
    assert!(ok && out.contains("develop"), "init: {out}");
    assert!(branches(&dir).contains("develop"), "develop created");

    let (out, ok) = rgit(&dir, &["flow", "start", "login"]);
    assert!(ok && out.contains("feature/login"), "start: {out}");

    std::fs::write(dir.join("w.txt"), "work\n").unwrap();
    git(&dir, &["add", "w.txt"]);
    git(&dir, &["commit", "-qm", "add w"]);

    let (out, ok) = rgit(&dir, &["flow", "finish"]);
    assert!(ok && out.contains("develop"), "finish: {out}");

    // Back on develop, the feature branch is gone, and its work is present.
    let list = branches(&dir);
    assert!(list.contains("develop") && list.contains("main"));
    assert!(!list.contains("feature/login"), "feature deleted: {list}");
    assert!(dir.join("w.txt").exists());

    let _ = std::fs::remove_dir_all(&dir);
}
