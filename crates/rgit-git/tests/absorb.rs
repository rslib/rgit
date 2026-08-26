//! Absorb folds a modified file's changes into the newest local commit that
//! touched it, leaving the working tree clean.

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend};

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

#[test]
fn absorb_folds_change_into_the_commit_that_touched_the_file() {
    let dir = std::env::temp_dir().join(format!("rgit-absorb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(&dir, &["add", "base.txt"]);
    git(&dir, &["commit", "-qm", "base"]);

    // Two local commits, each touching a different file.
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "a1\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-qm", "add a"]);
    std::fs::write(dir.join("b.txt"), "b1\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    git(&dir, &["commit", "-qm", "add b"]);

    // Modify a.txt in the working tree and absorb.
    std::fs::write(dir.join("a.txt"), "a1\na2\n").unwrap();
    let backend = Git2Backend::discover(&dir).unwrap();
    let msg = backend.absorb().unwrap();
    assert!(msg.contains("absorbed"), "{msg}");

    // Working tree is clean, and the "add a" commit now carries a2.
    assert!(
        backend.status().unwrap().unstaged.is_empty(),
        "worktree clean"
    );
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["show", "HEAD~1:a.txt"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a1\na2\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn absorb_routes_separate_hunks_of_one_file_to_their_own_commits() {
    let dir = std::env::temp_dir().join(format!("rgit-absorb-hunk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(
        dir.join("x.txt"),
        "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n",
    )
    .unwrap();
    git(&dir, &["add", "x.txt"]);
    git(&dir, &["commit", "-qm", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);

    // Two commits, each owning a different line.
    std::fs::write(
        dir.join("x.txt"),
        "l1\nl2-c1\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n",
    )
    .unwrap();
    git(&dir, &["commit", "-aqm", "C1 touches l2"]);
    std::fs::write(
        dir.join("x.txt"),
        "l1\nl2-c1\nl3\nl4\nl5\nl6\nl7\nl8-c2\nl9\nl10\n",
    )
    .unwrap();
    git(&dir, &["commit", "-aqm", "C2 touches l8"]);

    // Edit both lines in the working tree; absorb must split them.
    std::fs::write(
        dir.join("x.txt"),
        "l1\nl2-mod\nl3\nl4\nl5\nl6\nl7\nl8-mod\nl9\nl10\n",
    )
    .unwrap();
    let backend = Git2Backend::discover(&dir).unwrap();
    let msg = backend.absorb().unwrap();
    assert!(msg.contains("2 hunk(s) into 2 commit(s)"), "{msg}");
    assert!(backend.status().unwrap().unstaged.is_empty());

    // l2 folded into C1 (which does not touch l8); l8 folded into C2.
    let c1 = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["show", "HEAD~1:x.txt"])
        .output()
        .unwrap();
    let c1 = String::from_utf8_lossy(&c1.stdout);
    assert!(c1.contains("l2-mod") && c1.contains("\nl8\n"), "C1: {c1}");

    let _ = std::fs::remove_dir_all(&dir);
}
