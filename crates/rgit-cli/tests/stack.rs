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
