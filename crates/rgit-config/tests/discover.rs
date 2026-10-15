use std::collections::HashMap;
use std::path::Path;

use rgit_config::RepoDirs;
use tempfile::TempDir;

fn no_env(_: &str) -> Option<String> {
    None
}

fn mkdir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
}

#[test]
fn plain_repo() {
    let dir = TempDir::new().unwrap();
    mkdir(&dir.path().join(".git"));
    let repo = RepoDirs::discover_with(dir.path(), &no_env).unwrap();
    assert_eq!(repo.worktree_root.as_deref(), Some(dir.path()));
    assert_eq!(repo.common_dir, dir.path().join(".git"));
}

#[test]
fn subfolder_start_finds_the_root() {
    let dir = TempDir::new().unwrap();
    mkdir(&dir.path().join(".git"));
    let sub = dir.path().join("a").join("b");
    mkdir(&sub);
    let repo = RepoDirs::discover_with(&sub, &no_env).unwrap();
    assert_eq!(repo.worktree_root.as_deref(), Some(dir.path()));
}

#[test]
fn linked_worktree_uses_gitfile_and_commondir() {
    let dir = TempDir::new().unwrap();
    let main = dir.path().join("main").join(".git");
    let linked_git = main.join("worktrees").join("w");
    mkdir(&linked_git);
    std::fs::write(linked_git.join("commondir"), "../..\n").unwrap();
    let work = dir.path().join("w");
    mkdir(&work);
    std::fs::write(
        work.join(".git"),
        format!("gitdir: {}\n", linked_git.display()),
    )
    .unwrap();
    let repo = RepoDirs::discover_with(&work, &no_env).unwrap();
    assert_eq!(repo.worktree_root.as_deref(), Some(work.as_path()));
    assert_eq!(
        repo.common_dir.canonicalize().unwrap(),
        main.canonicalize().unwrap()
    );
}

#[test]
fn relative_gitfile_is_relative_to_its_folder() {
    let dir = TempDir::new().unwrap();
    mkdir(&dir.path().join("real"));
    let work = dir.path().join("w");
    mkdir(&work);
    std::fs::write(work.join(".git"), "gitdir: ../real\n").unwrap();
    let repo = RepoDirs::discover_with(&work, &no_env).unwrap();
    assert_eq!(
        repo.common_dir.canonicalize().unwrap(),
        dir.path().join("real").canonicalize().unwrap()
    );
}

#[test]
fn not_a_repo() {
    let dir = TempDir::new().unwrap();
    assert!(RepoDirs::discover_with(dir.path(), &no_env).is_none());
}

#[test]
fn git_dir_env_wins() {
    let dir = TempDir::new().unwrap();
    mkdir(&dir.path().join("bare"));
    let vars: HashMap<&str, String> = HashMap::from([
        ("GIT_DIR", dir.path().join("bare").display().to_string()),
        (
            "GIT_WORK_TREE",
            dir.path().join("tree").display().to_string(),
        ),
    ]);
    let env = |name: &str| vars.get(name).cloned();
    let repo = RepoDirs::discover_with(dir.path(), &env).unwrap();
    assert_eq!(repo.common_dir, dir.path().join("bare"));
    assert_eq!(repo.worktree_root, Some(dir.path().join("tree")));
}
