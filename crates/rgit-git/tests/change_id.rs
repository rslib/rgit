//! Every commit rgit makes carries a stable `Change-Id` trailer that survives
//! amend and rebase even as the commit oid changes.

use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-changeid-{}-{name}", std::process::id()))
}

/// A repo with Change-Id stamping turned on (`rgit.changeId`).
fn init_repo(name: &str) -> std::path::PathBuf {
    let dir = init_repo_default(name);
    git(&dir, &["config", "rgit.changeId", "true"]);
    dir
}

/// A repo with rgit's defaults: no Change-Id stamping.
fn init_repo_default(name: &str) -> std::path::PathBuf {
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

fn head_oid(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

// The Change-Id trailer value of a commit, read straight from its message.
fn change_id(dir: &Path, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "-1", "--format=%B", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("Change-Id:").map(|v| v.trim().to_owned()))
        .filter(|v| !v.is_empty())
}

#[test]
fn commit_stamps_a_change_id() {
    let dir = init_repo("commit");
    std::fs::write(dir.join("a.txt"), "1\n").unwrap();
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stage_all().unwrap();
    backend.commit("add a").unwrap();

    let id = change_id(&dir, "HEAD").expect("commit should carry a change id");
    assert!(id.starts_with('I') && id.len() == 41, "{id}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn amend_keeps_the_change_id_across_the_oid_change() {
    let dir = init_repo("amend");
    std::fs::write(dir.join("a.txt"), "1\n").unwrap();
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stage_all().unwrap();
    backend.commit("add a").unwrap();
    let before_oid = head_oid(&dir, "HEAD");
    let id = change_id(&dir, "HEAD").unwrap();

    std::fs::write(dir.join("a.txt"), "2\n").unwrap();
    backend.stage_all().unwrap();
    backend.amend("add a, revised").unwrap();

    assert_ne!(head_oid(&dir, "HEAD"), before_oid, "amend rewrites the oid");
    assert_eq!(
        change_id(&dir, "HEAD").as_deref(),
        Some(id.as_str()),
        "the change id must survive the amend"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// A fresh backend after a CLI checkout, so its in-memory HEAD/index are current.
fn commit_via_backend(dir: &Path, message: &str) {
    let backend = Git2Backend::discover(dir).unwrap();
    backend.stage_all().unwrap();
    backend.commit(message).unwrap();
}

#[test]
fn rebase_preserves_the_change_id() {
    let dir = init_repo("rebase");
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    commit_via_backend(&dir, "C0");

    git(&dir, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(dir.join("t.txt"), "t\n").unwrap();
    commit_via_backend(&dir, "T1");
    let id = change_id(&dir, "HEAD").unwrap();

    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("m.txt"), "m\n").unwrap();
    commit_via_backend(&dir, "M1");

    git(&dir, &["checkout", "-q", "topic"]);
    let before = head_oid(&dir, "HEAD");
    let backend = Git2Backend::discover(&dir).unwrap();
    backend.rebase_onto("main", &|_| {}).unwrap();

    assert_ne!(
        head_oid(&dir, "HEAD"),
        before,
        "rebase replays T1 as a new oid"
    );
    assert_eq!(
        change_id(&dir, "HEAD").as_deref(),
        Some(id.as_str()),
        "the change id rides along with the message through rebase"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn message(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "-1", "--format=%B", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

// The message with its trailing `Change-Id: I...` line replaced by a fixed one.
fn masked(msg: &str) -> String {
    let (head, _) = msg
        .rsplit_once("Change-Id: I")
        .expect("a change id trailer");
    format!("{head}Change-Id: I")
}

#[test]
fn change_id_trailer_is_set_off_from_the_body() {
    let dir = init_repo("trailer-sep");
    let backend = Git2Backend::discover(&dir).unwrap();
    let cases = [
        (
            "fix: a colon subject",
            "fix: a colon subject\n\nChange-Id: I",
        ),
        ("subject\n\n", "subject\n\nChange-Id: I"),
        ("feat: x\n\nbody\n", "feat: x\n\nbody\n\nChange-Id: I"),
        (
            "feat: x\n\nSigned-off-by: A <a@b>\n",
            "feat: x\n\nSigned-off-by: A <a@b>\nChange-Id: I",
        ),
    ];
    for (i, (msg, want)) in cases.iter().enumerate() {
        std::fs::write(dir.join("f.txt"), format!("{i}\n")).unwrap();
        backend.stage_all().unwrap();
        backend.commit(msg).unwrap();
        assert_eq!(masked(&message(&dir, "HEAD")), *want, "commit {msg:?}");
    }

    backend.amend("chore: amended").unwrap();
    assert_eq!(
        masked(&message(&dir, "HEAD")),
        "chore: amended\n\nChange-Id: I"
    );
    backend.reword("HEAD", "docs: reworded\n").unwrap();
    assert_eq!(
        masked(&message(&dir, "HEAD")),
        "docs: reworded\n\nChange-Id: I"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_commit_has_no_change_id() {
    let dir = init_repo_default("default-off");
    std::fs::write(dir.join("f"), "x\n").unwrap();
    git(&dir, &["add", "f"]);
    commit_via_backend(&dir, "fix: subject only");
    assert_eq!(change_id(&dir, "HEAD"), None);
    assert_eq!(message(&dir, "HEAD").trim_end(), "fix: subject only");
    let _ = std::fs::remove_dir_all(&dir);
}
