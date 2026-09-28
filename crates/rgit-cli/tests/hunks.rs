//! `--hunk` takes several hunks at once: stage, unstage and discard each one,
//! even when an earlier hunk changes the line count.

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rgit(dir: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rgit {args:?}: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn hunk_starts(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| l.strip_prefix("@@ "))
        .map(|l| l.split(' ').nth(1).unwrap().to_owned())
        .collect()
}

#[test]
fn stage_unstage_and_discard_several_hunks() {
    let dir = std::env::temp_dir().join(format!("rgit-hunks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    let base: Vec<String> = (1..=30).map(|n| format!("line {n}")).collect();
    std::fs::write(dir.join("f.txt"), base.join("\n") + "\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "init"]);

    // Each change adds a line, so every hunk moves the ones after it.
    let mut edited = base.clone();
    for n in [28, 15, 2] {
        edited.insert(n, format!("new {n}"));
    }
    std::fs::write(dir.join("f.txt"), edited.join("\n") + "\n").unwrap();
    assert_eq!(
        hunk_starts(&git(&dir, &["diff"])),
        ["+1,6", "+14,7", "+28,6"]
    );

    rgit(&dir, &["stage", "f.txt", "--hunk", "1,28"]);
    assert_eq!(
        hunk_starts(&git(&dir, &["diff", "--cached"])),
        ["+1,6", "+27,6"]
    );
    assert_eq!(hunk_starts(&git(&dir, &["diff"])), ["+14,7"]);

    rgit(&dir, &["unstage", "f.txt", "--hunk", "1,27"]);
    assert_eq!(git(&dir, &["diff", "--cached"]), "");

    rgit(&dir, &["discard", "f.txt", "--hunk", "1,14"]);
    assert_eq!(hunk_starts(&git(&dir, &["diff"])), ["+26,6"]);
}

#[test]
fn a_line_that_starts_no_hunk_is_an_error() {
    let dir = std::env::temp_dir().join(format!("rgit-hunks-miss-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("f.txt"), "a\nb\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-qm", "init"]);
    std::fs::write(dir.join("f.txt"), "A\nb\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["stage", "f.txt", "--hunk", "99"])
        .current_dir(&dir)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    assert!(
        text.contains("no hunk at line 99 of f.txt; hunks start at: 1"),
        "{text}"
    );
    assert_eq!(git(&dir, &["diff", "--cached"]), "");
}
