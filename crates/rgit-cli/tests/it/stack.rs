//! `rgit stack`: restack replays a child branch's own commits onto its parent's
//! new tip after the parent is edited.

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
        .args(
            ["--human", "--text", "--json", "--toon", "--axi"]
                .iter()
                .all(|m| !args.contains(m))
                .then_some("--toon"),
        )
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("RGIT_OUTPUT", "human")
        .env(
            "RGIT_CONFIG",
            std::env::temp_dir().join("rgit-stack-no-config.toml"),
        )
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

#[test]
fn restack_after_editing_the_parent() {
    let dir = std::env::temp_dir().join(format!("rgit-stack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("f"), "base\n").unwrap();
    git(&dir, &["add", "f"]);
    git(&dir, &["commit", "-qm", "base"]);

    rgit(&dir, &["stack", "new", "feat-a"]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    git(&dir, &["add", "a"]);
    git(&dir, &["commit", "-qm", "A"]);

    rgit(&dir, &["stack", "new", "feat-b"]);
    std::fs::write(dir.join("b"), "b\n").unwrap();
    git(&dir, &["add", "b"]);
    git(&dir, &["commit", "-qm", "B"]);

    // Edit the parent, then restack.
    git(&dir, &["checkout", "-q", "feat-a"]);
    std::fs::write(dir.join("a"), "a\na2\n").unwrap();
    git(&dir, &["add", "a"]);
    git(&dir, &["commit", "-qm", "A2"]);

    let (out, ok) = rgit(&dir, &["stack", "restack"]);
    assert!(ok && out.contains("feat-b"), "restack: {out}");

    // feat-b now sits on the edited parent: it has A2's change and its own b.
    git(&dir, &["checkout", "-q", "feat-b"]);
    assert!(dir.join("b").exists(), "feat-b keeps its own commit");
    assert_eq!(
        std::fs::read_to_string(dir.join("a")).unwrap(),
        "a\na2\n",
        "feat-b picked up the parent edit"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sync_without_a_stack_restacks_nothing() {
    let dir = std::env::temp_dir().join(format!("rgit-stack-sync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "base"]);

    let (out, ok) = rgit(&dir, &["stack", "sync"]);
    assert!(ok && out.contains("nothing to restack"), "sync: {out}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A repo on `main` with `b` stacked on `a`, `a` stacked on `main`.
fn two_deep(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-stack-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let (out, ok) = rgit(&dir, &["stack", "new", "a"]);
    assert!(ok, "{out}");
    let (out, ok) = rgit(&dir, &["stack", "new", "b"]);
    assert!(ok, "{out}");
    git(&dir, &["checkout", "-q", "main"]);
    dir
}

fn stack_records(dir: &Path) -> toml::Table {
    let text = std::fs::read_to_string(dir.join(".git/rgit/stacks.toml")).unwrap();
    toml::from_str(&text).unwrap()
}

fn parent_of(records: &toml::Table, branch: &str) -> Option<String> {
    records
        .get("branch")?
        .get(branch)?
        .get("parent")?
        .as_str()
        .map(str::to_owned)
}

#[test]
fn branch_rename_moves_record_and_children() {
    let dir = two_deep("rename");
    let (out, ok) = rgit(&dir, &["branch", "-m", "a", "a2"]);
    assert!(ok, "{out}");
    let records = stack_records(&dir);
    assert_eq!(parent_of(&records, "a2").as_deref(), Some("main"));
    assert_eq!(parent_of(&records, "b").as_deref(), Some("a2"));
    assert_eq!(parent_of(&records, "a"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn branch_delete_reparents_children() {
    let dir = two_deep("delete");
    let (out, ok) = rgit(&dir, &["branch", "-D", "a"]);
    assert!(ok, "{out}");
    let records = stack_records(&dir);
    assert_eq!(parent_of(&records, "b").as_deref(), Some("main"));
    assert_eq!(parent_of(&records, "a"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn branch_delete_without_record_leaves_file_unchanged() {
    let dir = two_deep("delete-none");
    git(&dir, &["branch", "x"]);
    let file = dir.join(".git/rgit/stacks.toml");
    let before = std::fs::read(&file).unwrap();
    let (out, ok) = rgit(&dir, &["branch", "-D", "x"]);
    assert!(ok, "{out}");
    assert_eq!(std::fs::read(&file).unwrap(), before);

    let plain = std::env::temp_dir().join(format!("rgit-stack-{}-plain", std::process::id()));
    let _ = std::fs::remove_dir_all(&plain);
    std::fs::create_dir_all(&plain).unwrap();
    git(&plain, &["init", "-q", "-b", "main"]);
    git(&plain, &["config", "user.email", "t@t"]);
    git(&plain, &["config", "user.name", "t"]);
    git(&plain, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&plain, &["branch", "x"]);
    let (out, ok) = rgit(&plain, &["branch", "-D", "x"]);
    assert!(ok, "{out}");
    assert!(!plain.join(".git/rgit/stacks.toml").exists());
    let _ = std::fs::remove_dir_all(&plain);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn repair_list_marks_missing_parent() {
    let dir = two_deep("repair-list");
    git(&dir, &["branch", "-m", "a", "a2"]);
    git(&dir, &["checkout", "-q", "b"]);
    let (out, ok) = rgit(&dir, &["--human", "stack", "list"]);
    assert!(ok, "{out}");
    assert!(out.contains("on a, missing"), "{out}");
    assert!(out.contains("rgit stack repair"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn repair_reparents_to_nearest_existing_ancestor() {
    let dir = two_deep("repair-nearest");
    git(&dir, &["branch", "-m", "a", "a2"]);
    let (out, ok) = rgit(&dir, &["--human", "stack", "repair"]);
    assert!(ok, "{out}");
    assert!(out.contains("b: parent a is gone, now on main"), "{out}");
    assert!(out.contains("a: branch is gone"), "{out}");
    let records = stack_records(&dir);
    assert_eq!(parent_of(&records, "b").as_deref(), Some("main"));
    assert_eq!(parent_of(&records, "a"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn repair_orphans_when_no_ancestor_exists() {
    let dir = two_deep("repair-orphan");
    let file = dir.join(".git/rgit/stacks.toml");
    std::fs::write(&file, "[branch.b]\nparent = \"a\"\n").unwrap();
    git(&dir, &["branch", "-D", "a"]);
    let (out, ok) = rgit(&dir, &["--human", "stack", "repair"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("b: parent a is gone, now has no parent"),
        "{out}"
    );
    let records = stack_records(&dir);
    assert_eq!(parent_of(&records, "b"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn repair_nothing_to_do_leaves_file_unchanged() {
    let dir = two_deep("repair-none");
    let file = dir.join(".git/rgit/stacks.toml");
    let before = std::fs::read(&file).unwrap();
    let (out, ok) = rgit(&dir, &["--human", "stack", "repair"]);
    assert!(ok && out.contains("nothing to repair"), "{out}");
    assert_eq!(std::fs::read(&file).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `two_deep` with a stack file that does not parse.
fn broken(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = two_deep(tag);
    let file = dir.join(".git/rgit/stacks.toml");
    std::fs::write(&file, "[branch").unwrap();
    (dir, file)
}

fn branch_names(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["branch", "--format=%(refname:short)"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).replace('\n', " ")
}

fn stderr_and_stdout(dir: &Path, args: &[&str]) -> (String, Option<i32>) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("RGIT_OUTPUT", "human")
        .env(
            "RGIT_CONFIG",
            std::env::temp_dir().join("rgit-stack-no-config.toml"),
        )
        .output()
        .unwrap();
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        out.status.code(),
    )
}

#[test]
fn broken_stack_file_blocks_delete() {
    let (dir, file) = broken("broken-delete");
    let (text, code) = stderr_and_stdout(&dir, &["branch", "-D", "a"]);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains(&file.display().to_string()), "{text}");
    assert!(branch_names(&dir).split(' ').any(|b| b == "a"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn broken_stack_file_blocks_rename() {
    let (dir, file) = broken("broken-rename");
    let (text, code) = stderr_and_stdout(&dir, &["branch", "-m", "a", "a2"]);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains(&file.display().to_string()), "{text}");
    let names = branch_names(&dir);
    assert!(names.split(' ').any(|b| b == "a"), "{names}");
    assert!(!names.split(' ').any(|b| b == "a2"), "{names}");
    let _ = std::fs::remove_dir_all(&dir);
}
