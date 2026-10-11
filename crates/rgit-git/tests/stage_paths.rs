use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend};

fn init_repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "test"],
    ] {
        git(&dir, &args);
    }
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
            .success(),
        "git {args:?} failed"
    );
}

fn index(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["ls-files", "-s"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Commit three files, then leave one modified, one deleted, one new file, a
/// changed folder and a changed glob target.
fn mixed(name: &str) -> PathBuf {
    let dir = init_repo(name);
    for f in [
        "mod.txt",
        "gone.txt",
        "dir/a.txt",
        "dir/b.txt",
        "g/x.rs",
        "g/y.rs",
    ] {
        write(&dir, f, "one\n");
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    write(&dir, "mod.txt", "two\n");
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    write(&dir, "new.txt", "new\n");
    write(&dir, "dir/a.txt", "two\n");
    write(&dir, "dir/c.txt", "new\n");
    std::fs::remove_file(dir.join("dir/b.txt")).unwrap();
    write(&dir, "g/x.rs", "two\n");
    write(&dir, "g/y.rs", "two\n");
    dir
}

const SET: [&str; 6] = [
    "mod.txt",
    "gone.txt",
    "new.txt",
    "dir",
    "g/*.rs",
    "dir/a.txt",
];

fn owned(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

#[test]
fn stage_paths_matches_git_add() {
    let ours = mixed("stage-paths-ours");
    let theirs = mixed("stage-paths-theirs");

    Git2Backend::discover(&ours)
        .unwrap()
        .stage_paths(&owned(&SET))
        .unwrap();
    let mut args = vec!["add", "--"];
    args.extend(SET);
    git(&theirs, &args);

    assert_eq!(index(&ours), index(&theirs));
    assert!(!index(&ours).contains("gone.txt"));
}

#[test]
fn unstage_paths_matches_git_reset() {
    let ours = mixed("unstage-paths-ours");
    let theirs = mixed("unstage-paths-theirs");
    git(&ours, &["add", "-A"]);
    git(&theirs, &["add", "-A"]);

    Git2Backend::discover(&ours)
        .unwrap()
        .unstage_paths(&owned(&SET))
        .unwrap();
    let mut args = vec!["reset", "-q", "--"];
    args.extend(SET);
    git(&theirs, &args);

    assert_eq!(index(&ours), index(&theirs));
}

#[test]
fn unmatched_path_fails_and_changes_nothing() {
    let dir = mixed("paths-nomatch");
    let before = index(&dir);
    let backend = Git2Backend::discover(&dir).unwrap();

    let err = backend
        .stage_paths(&owned(&["mod.txt", "nope.txt"]))
        .unwrap_err();
    assert!(err.to_string().contains("nope.txt"), "{err}");
    assert_eq!(index(&dir), before);

    git(&dir, &["add", "-A"]);
    let staged = index(&dir);
    let err = backend
        .unstage_paths(&owned(&["mod.txt", "nope.txt"]))
        .unwrap_err();
    assert!(err.to_string().contains("nope.txt"), "{err}");
    assert_eq!(index(&dir), staged);
}

#[test]
fn unborn_branch_stages_and_unstages_many() {
    let dir = init_repo("paths-unborn");
    for f in ["a.txt", "b.txt", "c.txt"] {
        write(&dir, f, "x\n");
    }
    let backend = Git2Backend::discover(&dir).unwrap();

    backend.stage_paths(&owned(&["a.txt", "b.txt"])).unwrap();
    assert_eq!(index(&dir).lines().count(), 2);

    backend.stage_paths(&owned(&["c.txt"])).unwrap();
    backend.unstage_paths(&owned(&["a.txt", "b.txt"])).unwrap();
    assert!(index(&dir).contains("c.txt") && !index(&dir).contains("a.txt"));
}

#[test]
fn sparse_skipped_deletion_stays_in_the_index() {
    let dir = mixed("paths-sparse");
    git(&dir, &["update-index", "--skip-worktree", "gone.txt"]);

    Git2Backend::discover(&dir)
        .unwrap()
        .stage_paths(&owned(&["gone.txt", "mod.txt"]))
        .unwrap();

    let staged = index(&dir);
    assert!(staged.contains("gone.txt"));
    let mod_line = staged.lines().find(|l| l.ends_with("mod.txt")).unwrap();
    assert!(
        !mod_line.contains("5626abf"),
        "mod.txt is staged: {mod_line}"
    );
}
