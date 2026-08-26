//! Copy-on-write workspaces: `workspace::create` clones the repo beside it on
//! a new branch, `list` shows it, `remove` deletes it, and a failed branch
//! create leaves no orphan directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_git::{workspace, Git2Backend, GitBackend};

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rgit-workspace-{}-{name}", std::process::id()))
}

fn init_repo(name: &str) -> PathBuf {
    let dir = scratch(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "test"],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "A"]);
    dir
}

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

// workspaces_dir() in src/workspace.rs: RGIT_WORKSPACE_DIR env override, else
// repo_root.parent()/.rgit-workspaces, namespaced by the repo's own dir name.
// Reproduce it here so tests can assert on the real paths without setting env
// vars (which would race other tests in this binary).
fn workspace_dest(repo_root: &Path, name: &str) -> PathBuf {
    repo_root
        .parent()
        .unwrap_or(repo_root)
        .join(".rgit-workspaces")
        .join(repo_root.file_name().unwrap_or(std::ffi::OsStr::new("repo")))
        .join(name)
}

// Cleanup helper: remove only this test's workspace dir, not the whole
// (shared, temp-dir-wide) .rgit-workspaces base other tests may be using.
fn cleanup(repo_root: &Path, name: &str) {
    let _ = std::fs::remove_dir_all(workspace_dest(repo_root, name));
}

#[test]
fn create_makes_workspace_dir_and_branch() {
    let dir = init_repo("create");
    let name = format!("ws-create-{}", std::process::id());
    cleanup(&dir, &name);

    let backend = Git2Backend::discover(&dir).unwrap();
    let out = workspace::create(&backend, &name).unwrap();
    assert!(out.contains(&name), "{out}");

    let dest = workspace_dest(&dir, &name);
    assert!(dest.join(".git").exists(), "workspace should be a clone");

    let clone = Git2Backend::discover(&dest).unwrap();
    assert!(
        clone.branch_exists(&name),
        "clone should have the new branch"
    );

    cleanup(&dir, &name);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_includes_a_created_workspace() {
    let dir = init_repo("list");
    let name = format!("ws-list-{}", std::process::id());
    cleanup(&dir, &name);

    let backend = Git2Backend::discover(&dir).unwrap();
    workspace::create(&backend, &name).unwrap();

    let listed = workspace::list(&backend).unwrap();
    assert!(listed.contains(&name), "{listed}");

    cleanup(&dir, &name);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remove_deletes_the_workspace_dir() {
    let dir = init_repo("remove");
    let name = format!("ws-remove-{}", std::process::id());
    cleanup(&dir, &name);

    let backend = Git2Backend::discover(&dir).unwrap();
    workspace::create(&backend, &name).unwrap();
    let dest = workspace_dest(&dir, &name);
    assert!(dest.exists());

    let out = workspace::remove(&backend, &name).unwrap();
    assert!(out.contains(&name), "{out}");
    assert!(!dest.exists(), "workspace dir should be gone");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remove_errors_when_workspace_missing() {
    let dir = init_repo("remove-missing");
    let name = format!("ws-missing-{}", std::process::id());
    cleanup(&dir, &name);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(workspace::remove(&backend, &name).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_rejects_invalid_ref_name_and_leaves_no_dir() {
    let dir = init_repo("badname");
    let name = "bad name"; // space is not a valid git ref component
    let dest = workspace_dest(&dir, name);
    let _ = std::fs::remove_dir_all(&dest);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(workspace::create(&backend, name).is_err());
    assert!(!dest.exists(), "invalid name should not create a dir");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_cleans_up_clone_when_branch_already_exists() {
    // "main" is a valid ref name, so it passes the up-front check, but the
    // fresh clone already has a "main" branch (checked out from the source),
    // so create_branch("main") fails there. The source comment says this
    // path removes the clone dir rather than leaving an orphan.
    let dir = init_repo("dupbranch");
    let name = "main";
    let dest = workspace_dest(&dir, name);
    cleanup(&dir, name);

    let backend = Git2Backend::discover(&dir).unwrap();
    let result = workspace::create(&backend, name);
    assert!(result.is_err(), "branch already exists in the clone");
    assert!(!dest.exists(), "failed create should not leave an orphan dir");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_errors_when_workspace_already_exists() {
    let dir = init_repo("dupname");
    let name = format!("ws-dup-{}", std::process::id());
    cleanup(&dir, &name);

    let backend = Git2Backend::discover(&dir).unwrap();
    workspace::create(&backend, &name).unwrap();
    assert!(
        workspace::create(&backend, &name).is_err(),
        "second create with the same name should fail"
    );

    cleanup(&dir, &name);
    let _ = std::fs::remove_dir_all(&dir);
}

// Two repos sharing a parent dir get separate workspace pools, so the same
// workspace name in each does not collide (the pre-fix flat namespace did).
#[test]
fn sibling_repos_do_not_share_a_workspace_pool() {
    let a = init_repo("sibling-a");
    let b = init_repo("sibling-b");
    assert_eq!(a.parent(), b.parent(), "both repos live under temp_dir");
    let name = format!("ws-shared-{}", std::process::id());
    cleanup(&a, &name);
    cleanup(&b, &name);

    let ba = Git2Backend::discover(&a).unwrap();
    let bb = Git2Backend::discover(&b).unwrap();
    // Same workspace name in both repos: both must succeed with distinct dirs.
    workspace::create(&ba, &name).unwrap();
    workspace::create(&bb, &name).unwrap();
    let dest_a = workspace_dest(&a, &name);
    let dest_b = workspace_dest(&b, &name);
    assert_ne!(dest_a, dest_b);
    assert!(dest_a.join(".git").exists());
    assert!(dest_b.join(".git").exists());

    // Each repo lists only its own workspace.
    let list_a = workspace::list(&ba).unwrap();
    assert!(list_a.contains(&name));
    assert!(!list_a.contains(&b.file_name().unwrap().to_string_lossy().to_string()));

    cleanup(&a, &name);
    cleanup(&b, &name);
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}
