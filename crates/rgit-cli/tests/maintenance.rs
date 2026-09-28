//! config, apply, notes, update-ref, hash-object, format-patch, am, archive,
//! gc and fsck run natively and agree with git.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Keep the user's real global config out of every git and rgit run.
fn isolate(cmd: &mut Command, dir: &Path) {
    let home = dir.with_extension("home");
    std::fs::create_dir_all(&home).unwrap();
    cmd.env("HOME", &home)
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    isolate(&mut cmd, dir);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rgit_in(dir: &Path, args: &[&str], stdin: &[u8]) -> (String, bool) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    cmd.arg("--human")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate(&mut cmd, dir);
    let mut child = cmd.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        out.status.success(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, success) = rgit_in(dir, args, b"");
    assert!(success, "rgit {args:?}: {out}");
    out
}

fn fails(dir: &Path, args: &[&str]) -> String {
    let (out, success) = rgit_in(dir, args, b"");
    assert!(!success, "rgit {args:?} should fail: {out}");
    out
}

fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-maint-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.with_extension("home"));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "fn main() {}\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", msg]);
}

#[test]
fn config_gets_sets_and_unsets_like_git() {
    let dir = repo("config");
    ok(&dir, &["config", "rgit.test", "yes"]);
    assert_eq!(git(&dir, &["config", "--local", "rgit.test"]), "yes\n");
    assert_eq!(ok(&dir, &["config", "rgit.test"]), "yes\n");
    assert_eq!(
        ok(&dir, &["config", "--get", "--bool", "rgit.test"]),
        "true\n"
    );
    ok(&dir, &["config", "--add", "rgit.multi", "a"]);
    ok(&dir, &["config", "--add", "rgit.multi", "b"]);
    assert_eq!(git(&dir, &["config", "--get-all", "rgit.multi"]), "a\nb\n");
    assert_eq!(ok(&dir, &["config", "--get-all", "rgit.multi"]), "a\nb\n");
    ok(&dir, &["config", "--unset-all", "rgit.multi"]);
    ok(&dir, &["config", "--unset", "rgit.test"]);
    fails(&dir, &["config", "rgit.test"]);
    ok(&dir, &["config", "--global", "rgit.home", "1k"]);
    assert_eq!(git(&dir, &["config", "--global", "rgit.home"]), "1k\n");
    assert_eq!(ok(&dir, &["config", "--int", "rgit.home"]), "1024\n");
    assert!(ok(&dir, &["config", "-l"]).contains("rgit.home=1k"));
    assert!(!ok(&dir, &["config", "--local", "-l"]).contains("rgit.home"));
    assert!(ok(&dir, &["config", "--global", "--list"]).contains("rgit.home=1k"));
}

#[test]
fn apply_matches_git_apply() {
    let dir = repo("apply");
    std::fs::write(dir.join("a.txt"), "one\n2\nthree\n").unwrap();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    git(&dir, &["add", "-N", "new.txt"]);
    let patch = git(&dir, &["diff"]);
    std::fs::write(dir.join("p.diff"), &patch).unwrap();
    git(&dir, &["reset", "-q"]);
    std::fs::remove_file(dir.join("new.txt")).unwrap();
    git(&dir, &["checkout", "--", "a.txt"]);

    assert!(ok(&dir, &["apply", "--stat", "p.diff"]).contains("2 files changed"));
    ok(&dir, &["apply", "--check", "p.diff"]);
    assert_eq!(git(&dir, &["status", "--short"]), "?? p.diff\n");
    ok(&dir, &["apply", "p.diff"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\n2\nthree\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("new.txt")).unwrap(),
        "new\n"
    );
    fails(&dir, &["apply", "--check", "p.diff"]);
    ok(&dir, &["apply", "-R", "p.diff"]);
    assert_eq!(git(&dir, &["status", "--short"]), "?? p.diff\n");

    let (_, success) = rgit_in(&dir, &["apply", "--cached"], patch.as_bytes());
    assert!(success);
    assert_eq!(git(&dir, &["diff", "--cached"]), patch);
    ok(&dir, &["apply", "--cached", "-R", "p.diff"]);
    assert_eq!(git(&dir, &["diff", "--cached"]), "");
}

#[test]
fn notes_add_show_append_and_remove() {
    let dir = repo("notes");
    assert_eq!(ok(&dir, &["notes"]), "no notes\n");
    ok(&dir, &["notes", "add", "-m", "first"]);
    assert_eq!(git(&dir, &["notes", "show"]), "first\n");
    fails(&dir, &["notes", "add", "-m", "again"]);
    ok(&dir, &["notes", "append", "-m", "second"]);
    assert_eq!(git(&dir, &["notes", "show", "HEAD"]), "first\n\nsecond\n");
    assert_eq!(ok(&dir, &["notes", "show"]), "first\n\nsecond\n");
    assert_eq!(ok(&dir, &["notes", "list"]), git(&dir, &["notes", "list"]));
    ok(&dir, &["notes", "add", "-f", "-m", "over"]);
    assert_eq!(git(&dir, &["notes", "show"]), "over\n");
    ok(&dir, &["notes", "remove"]);
    assert_eq!(git(&dir, &["notes", "list"]), "");
}

#[test]
fn update_ref_creates_moves_checks_and_deletes() {
    let dir = repo("update-ref");
    let first = git(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
    commit(&dir, "a.txt", "changed\n", "second");
    let second = git(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
    ok(&dir, &["update-ref", "refs/heads/topic", "HEAD~1"]);
    assert_eq!(git(&dir, &["rev-parse", "topic"]).trim(), first);
    fails(&dir, &["update-ref", "refs/heads/topic", "HEAD", &second]);
    ok(&dir, &["update-ref", "refs/heads/topic", "HEAD", &first]);
    assert_eq!(git(&dir, &["rev-parse", "topic"]).trim(), second);
    ok(&dir, &["update-ref", "-d", "refs/heads/topic"]);
    assert_eq!(git(&dir, &["branch", "--list", "topic"]), "");
    ok(&dir, &["update-ref", "HEAD", &first]);
    assert_eq!(git(&dir, &["rev-parse", "main"]).trim(), first);
    assert_eq!(git(&dir, &["symbolic-ref", "HEAD"]), "refs/heads/main\n");
}

#[test]
fn hash_object_matches_git() {
    let dir = repo("hash");
    std::fs::write(dir.join("loose.txt"), "hash me\n").unwrap();
    let want = git(&dir, &["hash-object", "loose.txt"]);
    assert_eq!(ok(&dir, &["hash-object", "loose.txt"]), want);
    let (out, success) = rgit_in(&dir, &["hash-object", "-w", "--stdin"], b"hash me\n");
    assert!(success, "{out}");
    assert_eq!(out, want);
    git(&dir, &["cat-file", "-e", want.trim()]);
}

#[test]
fn format_patch_and_am_round_trip() {
    let dir = repo("patches");
    commit(&dir, "a.txt", "one\n2\nthree\n", "Change two: to a digit!");
    commit(&dir, "src/new.rs", "pub fn f() {}\n", "add new.rs");
    let out = ok(&dir, &["format-patch", "-2", "-o", "out"]);
    let theirs = git(&dir, &["format-patch", "-2", "-o", "git-out"]);
    assert_eq!(out, theirs.replace("git-out/", "out/"));
    assert_eq!(ok(&dir, &["format-patch", "HEAD~2"]).lines().count(), 2);
    assert!(
        ok(&dir, &["format-patch", "HEAD~1..HEAD", "--stdout"])
            .contains("Subject: [PATCH] add new.rs")
    );

    let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
    let log = git(&dir, &["log", "--format=%s%n%an <%ae>", "-2"]);
    git(&dir, &["reset", "-q", "--hard", "HEAD~2"]);
    let files: Vec<String> = out.lines().map(str::to_owned).collect();
    let mut args = vec!["am"];
    args.extend(files.iter().map(String::as_str));
    ok(&dir, &args);
    assert_eq!(git(&dir, &["rev-parse", "HEAD^{tree}"]), tree);
    assert_eq!(git(&dir, &["log", "--format=%s%n%an <%ae>", "-2"]), log);

    // A patch that no longer applies stops am; --abort restores the branch.
    let head = git(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["reset", "-q", "--hard", "HEAD~2"]);
    commit(&dir, "a.txt", "conflict\n", "conflict");
    let conflicted = git(&dir, &["rev-parse", "HEAD"]);
    let (_, success) = rgit_in(&dir, &["am", &files[0]], b"");
    assert!(!success);
    ok(&dir, &["am", "--abort"]);
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), conflicted);
    assert_ne!(conflicted, head);
}

fn names(listing: &str) -> Vec<String> {
    let mut names: Vec<String> = listing
        .lines()
        .map(|l| l.trim_end_matches('/').to_owned())
        .filter(|l| !l.is_empty() && l != "pax_global_header")
        .collect();
    names.sort();
    names
}

fn list(dir: &Path, tool: &str, args: &[&str]) -> String {
    let out = Command::new(tool)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{tool} {args:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn archive_lists_the_same_files_as_git() {
    let dir = repo("archive");
    ok(&dir, &["archive", "-o", "r.tar", "--prefix", "p/"]);
    git(&dir, &["archive", "-o", "g.tar", "--prefix", "p/", "HEAD"]);
    assert_eq!(
        names(&list(&dir, "tar", &["-tf", "r.tar"])),
        names(&list(&dir, "tar", &["-tf", "g.tar"]))
    );
    ok(&dir, &["archive", "-o", "r.zip", "HEAD", "src"]);
    git(&dir, &["archive", "-o", "g.zip", "HEAD", "src"]);
    assert_eq!(
        names(&list(&dir, "unzip", &["-Z1", "r.zip"])),
        names(&list(&dir, "unzip", &["-Z1", "g.zip"]))
    );
    list(&dir, "unzip", &["-tq", "r.zip"]);
    ok(&dir, &["archive", "-o", "r.tgz"]);
    assert!(names(&list(&dir, "tar", &["-tzf", "r.tgz"])).contains(&"src/lib.rs".to_owned()));
    let (out, success) = rgit_in(&dir.join("src"), &["archive", "HEAD", "lib.rs"], b"");
    assert!(success);
    std::fs::write(dir.join("s.tar"), out.as_bytes()).unwrap();
    assert_eq!(
        names(&list(&dir, "tar", &["-tf", "s.tar"])),
        ["src", "src/lib.rs"]
    );
}

#[test]
fn gc_and_fsck_run() {
    let dir = repo("gc");
    ok(&dir, &["gc", "--prune=now"]);
    assert_eq!(git(&dir, &["count-objects"]).split(' ').next(), Some("0"));
    ok(&dir, &["fsck", "--full"]);
}
