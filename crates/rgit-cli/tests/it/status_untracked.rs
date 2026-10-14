//! `rgit status` untracked modes: a folder without tracked files is one
//! `<folder>/` entry unless `-uall` asks for every file (CHG-8z0w74).

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `rgit --json <args>` output as a JSON object.
fn json_out(dir: &Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rgit --json {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// `rgit --toon <args>` output.
fn toon(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("--toon")
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rgit --toon {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn venv_repo(tag: &str, files: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-untracked-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".venv")).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("tracked.txt"), "tracked\n").unwrap();
    git(&dir, &["add", "tracked.txt"]);
    git(&dir, &["commit", "-qm", "seed"]);
    for i in 0..files {
        std::fs::write(dir.join(format!(".venv/{i}.py")), "x\n").unwrap();
    }
    dir
}

fn untracked_paths(js: &serde_json::Value) -> Vec<String> {
    let Some(files) = js["files"].as_array() else {
        return Vec::new();
    };
    files
        .iter()
        .filter(|f| f["unstaged"] == "?")
        .map(|f| f["path"].as_str().unwrap_or("?").to_owned())
        .collect()
}

#[test]
fn status_untracked_json_collapses_venv() {
    let dir = venv_repo("json", 250);
    let js = json_out(&dir, &["status"]);
    assert_eq!(untracked_paths(&js), vec![".venv/".to_owned()]);
    let out = toon(&dir, &["status"]);
    assert!(out.contains("files[1]{path,staged,unstaged}:"), "{out}");
    assert!(out.contains(".venv/"), "{out}");
}

#[test]
fn status_untracked_uall_lists_files() {
    let dir = venv_repo("uall", 250);
    let js = json_out(&dir, &["status", "-uall"]);
    let paths = untracked_paths(&js);
    assert_eq!(paths.len(), 250);
    assert!(paths.iter().all(|p| p.starts_with(".venv/")));
}

#[test]
fn status_untracked_uno_hides() {
    let dir = venv_repo("uno", 250);
    let js = json_out(&dir, &["status", "-uno"]);
    assert!(untracked_paths(&js).is_empty());
}

#[test]
fn status_untracked_config_no_hides() {
    let dir = venv_repo("config-no", 250);
    git(&dir, &["config", "status.showUntrackedFiles", "no"]);
    let js = json_out(&dir, &["status"]);
    assert!(untracked_paths(&js).is_empty());
}

#[test]
fn status_untracked_path_inside_collapsed_folder() {
    let dir = venv_repo("path-inside", 3);
    let js = json_out(&dir, &["status", ".venv/1.py"]);
    assert_eq!(untracked_paths(&js), vec![".venv/1.py".to_owned()]);
    let js = json_out(&dir, &["status", ".venv/*.py"]);
    assert_eq!(untracked_paths(&js).len(), 3);
    let js = json_out(&dir, &["status", ".venv/"]);
    assert_eq!(untracked_paths(&js), vec![".venv/".to_owned()]);
}

#[test]
fn status_untracked_add_dry_run_lists_files() {
    let dir = venv_repo("add-dry-run", 2);
    let out = toon(&dir, &["add", "-n", "."]);
    assert!(out.contains("add '.venv/0.py'"), "{out}");
    assert!(out.contains("add '.venv/1.py'"), "{out}");
    assert!(!out.contains("add '.venv/'"), "{out}");
}
