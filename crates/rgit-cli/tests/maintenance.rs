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
fn apply_handles_binary_reject_three_way_and_paths_like_git() {
    let dir = repo("apply-more");
    std::fs::write(
        dir.join("a.txt"),
        (1..=20).map(|n| format!("{n}\n")).collect::<String>(),
    )
    .unwrap();
    std::fs::write(dir.join("b.bin"), b"bin\0ary").unwrap();
    std::fs::create_dir_all(dir.join("src/deep")).unwrap();
    std::fs::write(dir.join("src/deep/old.txt"), "mv\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "base"]);
    let text = std::fs::read_to_string(dir.join("a.txt")).unwrap();
    std::fs::write(
        dir.join("a.txt"),
        text.replace("\n2\n", "\ntwo\n")
            .replace("\n18\n", "\neighteen\n"),
    )
    .unwrap();
    std::fs::write(dir.join("b.bin"), b"bin\0ary2").unwrap();
    git(&dir, &["mv", "src/deep/old.txt", "src/deep/new.txt"]);
    git(&dir, &["rm", "-q", "src/lib.rs"]);
    git(&dir, &["add", "-A"]);
    let patch = git(&dir, &["diff", "--cached", "--binary"]);
    let p = dir.with_extension("patch");
    std::fs::write(&p, &patch).unwrap();
    git(&dir, &["reset", "-q", "--hard"]);
    let p = p.to_str().unwrap();
    for args in [
        &["apply", "--stat", p][..],
        &["apply", "--numstat", p],
        &["apply", "--summary", "--stat", p],
        &["apply", "--stat", "--directory=x", "--exclude=*.bin", p],
    ] {
        assert_eq!(ok(&dir, args), git(&dir, args), "{args:?}");
    }

    // Binary patches apply and reverse.
    ok(&dir, &["apply", "--index", p]);
    assert_eq!(git(&dir, &["diff", "--cached", "--binary"]), patch);
    ok(&dir, &["apply", "--index", "-R", p]);
    assert_eq!(git(&dir, &["status", "--short"]), "");

    // --reject applies what fits and leaves the rest in a.txt.rej.
    std::fs::write(dir.join("a.txt"), text.replace("\n18\n", "\nEIGHTEEN\n")).unwrap();
    fails(&dir, &["apply", "--reject", p]);
    let now = std::fs::read_to_string(dir.join("a.txt")).unwrap();
    assert!(now.contains("\ntwo\n") && now.contains("EIGHTEEN"));
    assert!(
        std::fs::read_to_string(dir.join("a.txt.rej"))
            .unwrap()
            .contains("+eighteen")
    );
    assert!(dir.join("src/deep/new.txt").exists());
    git(&dir, &["reset", "-q", "--hard"]);
    git(&dir, &["clean", "-qfd"]);

    // --3way leaves git's conflict in the index and the file.
    std::fs::write(dir.join("a.txt"), text.replace("\n18\n", "\nEIGHTEEN\n")).unwrap();
    git(&dir, &["commit", "-qam", "theirs"]);
    fails(&dir, &["apply", "--3way", "--include=a.txt", p]);
    let ours = git(&dir, &["diff"]);
    git(&dir, &["reset", "-q", "--hard"]);
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&dir)
        .args(["apply", "--3way", "--include=a.txt", p]);
    isolate(&mut cmd, &dir);
    assert!(!cmd.output().unwrap().status.success());
    assert_eq!(ours, git(&dir, &["diff"]));
    git(&dir, &["reset", "-q", "--hard"]);
    for side in ["--ours", "--theirs", "--union"] {
        ok(&dir, &["apply", "--3way", side, "--include=a.txt", p]);
        let ours = std::fs::read_to_string(dir.join("a.txt")).unwrap();
        git(&dir, &["reset", "-q", "--hard"]);
        git(&dir, &["apply", "--3way", side, "--include=a.txt", p]);
        assert_eq!(
            ours,
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "{side}"
        );
        git(&dir, &["reset", "-q", "--hard"]);
    }

    // --recount fixes hand-edited hunk headers.
    let edited = git(&dir, &["diff", "HEAD~1", "HEAD", "--", "a.txt"])
        .replace("@@ -15,6 +15,6 @@", "@@ -15,9 +15,2 @@");
    let edited_path = dir.with_extension("edited.diff");
    std::fs::write(&edited_path, &edited).unwrap();
    git(&dir, &["checkout", "-q", "HEAD~1", "--", "a.txt"]);
    ok(&dir, &["apply", "--recount", edited_path.to_str().unwrap()]);
    assert!(
        std::fs::read_to_string(dir.join("a.txt"))
            .unwrap()
            .contains("EIGHTEEN")
    );
    git(&dir, &["reset", "-q", "--hard"]);

    // -p and running outside a repository, like GNU patch.
    let out = dir.with_extension("outside");
    std::fs::create_dir_all(out.join("deep")).unwrap();
    std::fs::write(out.join("deep/a.txt"), &text).unwrap();
    let only_a = git(&dir, &["diff", "HEAD~1", "HEAD", "--", "a.txt"]);
    let (_, success) = rgit_in(
        &out,
        &["apply", "--check", "--directory=deep"],
        only_a.as_bytes(),
    );
    assert!(success);
    let (_, success) = rgit_in(&out.join("deep"), &["apply", "-p1"], only_a.as_bytes());
    assert!(success);
    assert!(
        std::fs::read_to_string(out.join("deep/a.txt"))
            .unwrap()
            .contains("EIGHTEEN")
    );
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
fn notes_copy_prune_merge_edit_and_message_sources() {
    let dir = repo("notes-more");
    commit(&dir, "a.txt", "two\n", "second");
    std::fs::write(dir.join("msg.txt"), "from a file\n").unwrap();
    ok(&dir, &["notes", "add", "-F", "msg.txt", "HEAD~1"]);
    assert_eq!(git(&dir, &["notes", "show", "HEAD~1"]), "from a file\n");
    ok(&dir, &["notes", "copy", "HEAD~1", "HEAD"]);
    assert_eq!(git(&dir, &["notes", "show", "HEAD"]), "from a file\n");
    fails(&dir, &["notes", "copy", "HEAD~1", "HEAD"]);
    let blob = git(&dir, &["notes", "list", "HEAD"]);
    ok(&dir, &["notes", "add", "-f", "-C", blob.trim(), "HEAD~1"]);
    ok(
        &dir,
        &["notes", "add", "-f", "--allow-empty", "-m", "", "HEAD"],
    );
    assert_eq!(git(&dir, &["notes", "show", "HEAD"]), "");
    ok(
        &dir,
        &["notes", "remove", "--ignore-missing", "HEAD", "HEAD~1"],
    );
    assert_eq!(git(&dir, &["notes", "list"]), "");

    // Separators between paragraphs, as git joins them.
    for args in [
        &["notes", "add", "-f", "-m", "a", "-m", "b", "--separator=--"][..],
        &["notes", "append", "--no-separator", "-m", "c"],
        &["notes", "append", "-m", "d"],
    ] {
        ok(&dir, args);
        let ours = git(&dir, &["notes", "show"]);
        git(&dir, &["notes", "remove", "--ignore-missing"]);
        if args[1] != "add" {
            git(
                &dir,
                &["notes", "add", "-m", "a", "-m", "b", "--separator=--"],
            );
            if args.contains(&"d") {
                git(&dir, &["notes", "append", "--no-separator", "-m", "c"]);
            }
        }
        git(&dir, args);
        assert_eq!(ours, git(&dir, &["notes", "show"]), "{args:?}");
    }
    git(&dir, &["notes", "remove"]);

    // The editor writes the note for `edit`.
    let editor = dir.with_extension("editor.sh");
    std::fs::write(&editor, "#!/bin/sh\necho edited > \"$1\"\n").unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&editor)
        .status()
        .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    cmd.args(["--human", "notes", "edit"])
        .current_dir(&dir)
        .env("GIT_EDITOR", &editor);
    isolate(&mut cmd, &dir);
    assert!(cmd.output().unwrap().status.success());
    assert_eq!(git(&dir, &["notes", "show"]), "edited\n");
    git(&dir, &["notes", "remove"]);

    // prune drops notes of objects that are gone.
    let (blob, _) = rgit_in(
        &dir,
        &["hash-object", "-w", "--stdin"],
        b"not in the tree\n",
    );
    git(&dir, &["notes", "add", "-m", "doomed", blob.trim()]);
    std::fs::remove_file(
        dir.join(".git/objects")
            .join(&blob[..2])
            .join(blob[2..].trim()),
    )
    .unwrap();
    assert_eq!(ok(&dir, &["notes", "prune", "-n"]), blob);
    ok(&dir, &["notes", "prune"]);
    assert!(!git(&dir, &["notes", "list"]).contains(blob.trim()));

    // merge: fast-forward, then a conflict resolved with a strategy.
    assert_eq!(ok(&dir, &["notes", "get-ref"]), "refs/notes/commits\n");
    ok(
        &dir,
        &["notes", "--ref", "other", "add", "-m", "theirs", "HEAD~1"],
    );
    assert_eq!(
        ok(&dir, &["notes", "--ref", "other", "get-ref"]),
        "refs/notes/other\n"
    );
    git(&dir, &["notes", "add", "-f", "-m", "ours", "HEAD~1"]);
    fails(&dir, &["notes", "merge", "other"]);
    ok(&dir, &["notes", "merge", "-s", "union", "other"]);
    assert_eq!(git(&dir, &["notes", "show", "HEAD~1"]), "ours\n\ntheirs\n");
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
fn update_ref_stdin_transactions_are_all_or_nothing() {
    let dir = repo("update-ref-stdin");
    let first = git(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
    commit(&dir, "a.txt", "changed\n", "second");
    let second = git(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
    let script = format!(
        "create refs/heads/a {first}\nupdate refs/heads/b {second}\nverify refs/heads/main {second}\n"
    );
    let (out, success) = rgit_in(&dir, &["update-ref", "--stdin"], script.as_bytes());
    assert!(success, "{out}");
    assert_eq!(
        git(&dir, &["rev-parse", "a", "b"]),
        format!("{first}\n{second}\n")
    );

    // One bad expectation and nothing changes.
    let script = format!("delete refs/heads/a\nupdate refs/heads/b {first} {first}\n");
    let (_, success) = rgit_in(&dir, &["update-ref", "--stdin"], script.as_bytes());
    assert!(!success);
    assert_eq!(
        git(&dir, &["rev-parse", "a", "b"]),
        format!("{first}\n{second}\n")
    );

    // The explicit protocol answers each step, NUL-separated with -z.
    let script = format!(
        "start\0update refs/heads/b\0{first}\0{second}\0delete refs/heads/a\0\0prepare\0commit\0"
    );
    let (out, success) = rgit_in(&dir, &["update-ref", "--stdin", "-z"], script.as_bytes());
    assert!(success, "{out}");
    assert_eq!(out, "start: ok\nprepare: ok\ncommit: ok\n");
    assert_eq!(git(&dir, &["rev-parse", "b"]).trim(), first);
    assert_eq!(git(&dir, &["branch", "--list", "a"]), "");

    ok(
        &dir,
        &["update-ref", "--create-reflog", "refs/custom/x", &first],
    );
    assert_eq!(
        git(&dir, &["reflog", "show", "--format=%H", "refs/custom/x"]).trim(),
        first
    );
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

    // git's clean filters: eol, ident and a filter driver.
    std::fs::write(dir.join("crlf.txt"), "a\r\nb\r\n").unwrap();
    std::fs::write(dir.join("id.txt"), "x $Id$ y\n").unwrap();
    std::fs::write(dir.join("up.txt"), "hello\n").unwrap();
    std::fs::write(
        dir.join(".gitattributes"),
        "*.txt text eol=lf\nid.txt ident\nup.txt filter=up\n",
    )
    .unwrap();
    git(&dir, &["config", "filter.up.clean", "tr a-z A-Z"]);
    for args in [
        &["hash-object", "crlf.txt", "id.txt", "up.txt"][..],
        &["hash-object", "--no-filters", "crlf.txt", "up.txt"],
    ] {
        assert_eq!(ok(&dir, args), git(&dir, args), "{args:?}");
    }
    let (out, _) = rgit_in(
        &dir,
        &["hash-object", "--stdin-paths"],
        b"crlf.txt\nup.txt\n",
    );
    assert_eq!(out, git(&dir, &["hash-object", "crlf.txt", "up.txt"]));
    let (out, _) = rgit_in(
        &dir,
        &["hash-object", "--stdin", "--path", "up.txt"],
        b"hello\n",
    );
    assert_eq!(out, git(&dir, &["hash-object", "up.txt"]));
    fails(&dir, &["hash-object", "-t", "commit", "loose.txt"]);
    ok(
        &dir,
        &["hash-object", "-t", "commit", "--literally", "loose.txt"],
    );

    // Without -w no repository is needed.
    let out = dir.with_extension("outside");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("f"), "hash me\n").unwrap();
    assert_eq!(ok(&out, &["hash-object", "f"]), want);
    fails(&out, &["hash-object", "-w", "f"]);
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

#[test]
fn am_keeps_subjects_dates_and_shows_the_current_patch() {
    let dir = repo("am-more");
    commit(&dir, "a.txt", "one\n2\nthree\n", "[tag] keep me");
    let mbox = dir.with_extension("mbox");
    std::fs::write(&mbox, git(&dir, &["format-patch", "-1", "--stdout", "-k"])).unwrap();
    let mbox = mbox.to_str().unwrap();
    let author_date = git(&dir, &["log", "-1", "--format=%ad"]);
    git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
    ok(&dir, &["am", "-k", "--committer-date-is-author-date", mbox]);
    assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "[tag] keep me\n");
    assert_eq!(git(&dir, &["log", "-1", "--format=%cd"]), author_date);

    git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&dir, "a.txt", "conflict\n", "conflict");
    fails(&dir, &["am", mbox]);
    assert!(ok(&dir, &["am", "--show-current-patch=diff"]).contains("+2"));
    ok(&dir, &["am", "--quit"]);
    assert!(!dir.join(".git/rebase-apply").exists());
    assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "conflict\n");
}

/// Mask what differs between two runs: the cover letter's date and the
/// timestamp in Message-IDs.
fn mask(mail: &str) -> String {
    let lines: Vec<&str> = mail.lines().collect();
    let cover = |i: usize| {
        lines[i..]
            .iter()
            .take_while(|l| !l.is_empty())
            .any(|l| l.starts_with("Subject: ") && l.contains(" 0/"))
    };
    lines
        .iter()
        .enumerate()
        .map(|(n, l)| {
            if l.starts_with("Date: ") && cover(n) {
                "Date:".to_owned()
            } else if let Some(i) = l.find(".git.").filter(|_| l.contains('<')) {
                let start = l[..i].rfind('.').unwrap_or(i);
                format!("{}{}", &l[..start], &l[i..])
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn format_patch_writes_what_git_writes() {
    let dir = repo("format-patch-more");
    git(&dir, &["config", "user.name", "J. Döe"]);
    std::fs::write(dir.join("b.bin"), b"bin\0ary").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "binary"]);
    std::fs::write(dir.join("b.bin"), b"bin\0ary2").unwrap();
    git(&dir, &["mv", "src/lib.rs", "src/main.rs"]);
    commit(
        &dir,
        "a.txt",
        "one\n2\nthree",
        "Ünïcödé: a subject long enough to be folded over more than one header line",
    );
    commit(&dir, "new.txt", "new\n", "add new.txt\n\nWith a body.");
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "empty"]);
    for args in [
        &["format-patch", "--stdout", "-3"][..],
        &[
            "format-patch",
            "--stdout",
            "-3",
            "-p",
            "-k",
            "--no-signature",
        ],
        &[
            "format-patch",
            "--stdout",
            "-1",
            "-n",
            "--start-number=4",
            "--zero-commit",
        ],
        &[
            "format-patch",
            "--stdout",
            "-2",
            "--signature=sig",
            "--base=HEAD~3",
        ],
        &[
            "format-patch",
            "--stdout",
            "--root",
            "HEAD~3",
            "--subject-prefix=X",
        ],
        &[
            "format-patch",
            "--stdout",
            "-3",
            "--cover-letter",
            "--thread",
            "--in-reply-to=<a@b>",
            "--to=x@y",
            "--to=z@y",
            "--cc=c@d",
            "-v2",
            "--rfc",
        ],
        &["format-patch", "--stdout", "-3", "--thread=deep"],
    ] {
        assert_eq!(mask(&ok(&dir, args)), mask(&git(&dir, args)), "{args:?}");
    }
    let listed = ok(
        &dir,
        &["format-patch", "-2", "-o", "r", "--cover-letter", "-v3"],
    );
    git(
        &dir,
        &["format-patch", "-2", "-o", "g", "--cover-letter", "-v3"],
    );
    for name in listed.lines() {
        let theirs = dir.join("g").join(Path::new(name).file_name().unwrap());
        assert_eq!(
            mask(&std::fs::read_to_string(dir.join(name)).unwrap()),
            mask(&std::fs::read_to_string(theirs).unwrap())
        );
    }
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
fn archive_honours_export_attributes_and_extra_files() {
    let dir = repo("archive-attrs");
    std::fs::create_dir_all(dir.join("secret")).unwrap();
    std::fs::write(dir.join("secret/key"), "k\n").unwrap();
    std::fs::write(dir.join("notes.tmp"), "t\n").unwrap();
    std::fs::write(dir.join("VERSION"), "$Format:%H %an %s$\n").unwrap();
    std::fs::write(
        dir.join(".gitattributes"),
        "secret export-ignore\n*.tmp export-ignore\nVERSION export-subst\n",
    )
    .unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "attrs"]);
    std::fs::write(dir.join("extra.txt"), "x\n").unwrap();
    let args = ["archive", "--add-file=extra.txt", "-o"];
    ok(&dir, &[&args[..], &["r.tar", "HEAD"]].concat());
    git(&dir, &[&args[..], &["g.tar", "HEAD"]].concat());
    assert_eq!(
        names(&list(&dir, "tar", &["-tf", "r.tar"])),
        names(&list(&dir, "tar", &["-tf", "g.tar"]))
    );
    let version = |tar: &str| list(&dir, "tar", &["-xOf", tar, "VERSION"]);
    assert_eq!(version("r.tar"), version("g.tar"));
    assert!(!version("r.tar").contains("$Format"));

    assert_eq!(ok(&dir, &["archive", "-l"]), git(&dir, &["archive", "-l"]));
    ok(&dir, &["archive", "-9", "--format=zip", "-o", "best.zip"]);
    list(&dir, "unzip", &["-tq", "best.zip"]);
    ok(&dir, &["archive", "--mtime=2020-02-03", "-o", "old.tar"]);
    assert!(list(&dir, "tar", &["-tvf", "old.tar"]).contains("2020"));

    // --remote reads another local repository, even from outside one.
    let out = dir.with_extension("outside");
    std::fs::create_dir_all(&out).unwrap();
    ok(
        &out,
        &[
            "archive",
            "--remote",
            dir.to_str().unwrap(),
            "-o",
            "remote.tar",
        ],
    );
    assert!(names(&list(&out, "tar", &["-tf", "remote.tar"])).contains(&"VERSION".to_owned()));
}

#[test]
fn cherry_and_aliases_match_git() {
    let dir = repo("cherry");
    git(&dir, &["checkout", "-qb", "topic"]);
    commit(&dir, "a.txt", "one\ntwo\n3\n", "three to digit");
    commit(&dir, "b.txt", "b\n", "add b");
    commit(&dir, "c.txt", "c\n", "add c");
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["cherry-pick", "topic~1"]);
    git(&dir, &["checkout", "-q", "topic"]);
    for args in [
        &["cherry", "main"][..],
        &["cherry", "-v", "main", "topic"],
        &["cherry", "-v", "main", "topic", "topic~2"],
    ] {
        assert_eq!(ok(&dir, args), git(&dir, args), "{args:?}");
    }
    git(&dir, &["branch", "-q", "--set-upstream-to=main"]);
    assert_eq!(ok(&dir, &["cherry"]), git(&dir, &["cherry"]));
    assert_eq!(
        ok(&dir, &["annotate", "a.txt"]),
        ok(&dir, &["blame", "a.txt"])
    );
    assert_eq!(
        ok(&dir, &["whatchanged", "-n", "2"]),
        ok(&dir, &["log", "-n", "2"])
    );
}

#[test]
fn bundle_round_trips_through_git() {
    let dir = repo("bundle");
    git(&dir, &["tag", "-a", "v1", "-m", "tag"]);
    commit(&dir, "a.txt", "two\n", "second");
    let all = dir.with_extension("all.bundle");
    let inc = dir.with_extension("inc.bundle");
    let (all, inc) = (all.to_str().unwrap(), inc.to_str().unwrap());
    ok(&dir, &["bundle", "create", all, "--all"]);
    ok(&dir, &["bundle", "create", inc, "v1..main"]);
    // git reads what rgit writes, and the reverse.
    assert_eq!(
        ok(&dir, &["bundle", "list-heads", all]),
        git(&dir, &["bundle", "list-heads", all])
    );
    assert_eq!(
        ok(&dir, &["bundle", "verify", inc]).replace(&format!("{inc} is okay\n"), ""),
        git(&dir, &["bundle", "verify", inc])
    );
    let clone = dir.with_extension("clone");
    let _ = std::fs::remove_dir_all(&clone);
    git(&dir, &["clone", "-q", all, clone.to_str().unwrap()]);
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD"]),
        git(&dir, &["rev-parse", "HEAD"])
    );

    // A repository without the prerequisite cannot use the increment.
    let fresh = dir.with_extension("fresh");
    let _ = std::fs::remove_dir_all(&fresh);
    std::fs::create_dir_all(&fresh).unwrap();
    git(&fresh, &["init", "-q"]);
    fails(&fresh, &["bundle", "verify", inc]);
    let heads = ok(&fresh, &["bundle", "unbundle", all]);
    assert_eq!(heads, git(&dir, &["bundle", "list-heads", all]));
    git(
        &fresh,
        &["cat-file", "-e", git(&dir, &["rev-parse", "HEAD"]).trim()],
    );
    ok(&fresh, &["bundle", "unbundle", inc]);

    // list-heads needs no repository.
    let out = dir.with_extension("outside");
    std::fs::create_dir_all(&out).unwrap();
    assert!(ok(&out, &["bundle", "list-heads", all]).contains("refs/heads/main"));
    fails(&dir, &["bundle", "create", inc, "HEAD..HEAD"]);
}

#[test]
fn request_pull_matches_git() {
    let dir = repo("request-pull");
    git(&dir, &["checkout", "-qb", "topic"]);
    let big: String = (1..=100).map(|n| format!("{n}\n")).collect();
    commit(&dir, "big.txt", &big, "add big");
    git(
        &dir,
        &[
            "-c",
            "user.name=Another",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "empty",
        ],
    );
    commit(&dir, "a.txt", "one\ntwo\nthree\nfour\n", "four");
    git(&dir, &["tag", "-a", "v2", "-m", "Release two\n\nNotes."]);
    git(
        &dir,
        &["config", "branch.topic.description", "My topic\nexplained"],
    );
    let bare = dir.with_extension("git");
    let _ = std::fs::remove_dir_all(&bare);
    git(
        &dir,
        &["clone", "-q", "--bare", ".", bare.to_str().unwrap()],
    );
    let url = bare.to_str().unwrap();
    for args in [
        &["request-pull", "main", url, "topic"][..],
        &["request-pull", "-p", "main", url, "v2"],
        &["request-pull", "main", url],
    ] {
        let (ours, ours_ok) = rgit_in(&dir, args, b"");
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&dir).args(args);
        isolate(&mut cmd, &dir);
        let theirs = cmd.output().unwrap();
        let theirs_text = String::from_utf8_lossy(&theirs.stdout).into_owned()
            + &String::from_utf8_lossy(&theirs.stderr);
        assert_eq!(ours_ok, theirs.status.success(), "{args:?}");
        if ours_ok {
            assert_eq!(ours, theirs_text, "{args:?}");
        } else {
            // stdout and stderr interleave differently; compare the lines.
            let mut a: Vec<&str> = ours.lines().collect();
            let mut b: Vec<&str> = theirs_text.lines().collect();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "{args:?}");
        }
    }
    // Not pushed: git's warnings and status 1, the summary still printed.
    commit(&dir, "c.txt", "c\n", "local only");
    let (out, success) = rgit_in(&dir, &["request-pull", "main", url, "topic"], b"");
    assert!(!success);
    assert!(out.contains("warn: No match for commit") && out.contains("The following changes"));
}

#[test]
fn range_diff_matches_git() {
    let dir = repo("range-diff");
    let ten: String = (1..=10).map(|n| format!("{n}\n")).collect();
    commit(&dir, "a.txt", &ten, "ten lines");
    let series = |branch: &str, five: &str, body: &str, extra: &str| {
        git(&dir, &["checkout", "-qb", branch, "main"]);
        commit(
            &dir,
            "a.txt",
            &ten.replace("\n2\n", "\ntwo\n"),
            "change two",
        );
        commit(
            &dir,
            "a.txt",
            &ten.replace("\n2\n", "\ntwo\n")
                .replace("\n5\n", &format!("\n{five}\n")),
            &format!("change five\n\n{body}"),
        );
        git(&dir, &["mv", "src/lib.rs", "src/moved.rs"]);
        git(&dir, &["commit", "-qm", "move lib"]);
        commit(&dir, extra, "x\n", &format!("add {extra}"));
    };
    series("v1", "five", "Body.", "x.txt");
    series("v2", "FIVE", "Body changed.", "y.txt");
    for args in [
        &["range-diff", "main", "v1", "v2"][..],
        &["range-diff", "-s", "main..v1", "main..v2"],
        &["range-diff", "--right-only", "v1...v2"],
        &["range-diff", "--creation-factor=100", "main", "v1", "v2"],
    ] {
        assert_eq!(
            ok(&dir, args),
            git(&dir, &[&["-c", "color.ui=never"][..], args].concat()),
            "{args:?}"
        );
    }
}

#[test]
fn difftool_and_mergetool_run_the_configured_tools() {
    let dir = repo("tools");
    commit(&dir, "a.txt", "one\n2\nthree\n", "digit");
    std::fs::write(dir.join("a.txt"), "one\n2\n3\n").unwrap();
    for args in [
        &["difftool", "-y", "-x", "cat"][..],
        &["difftool", "-y", "-x", "cat", "HEAD~1"],
        &["difftool", "-y", "-x", "cat", "HEAD~1", "HEAD"],
    ] {
        assert_eq!(ok(&dir, args), git(&dir, args), "{args:?}");
    }
    git(&dir, &["config", "diff.tool", "shout"]);
    git(
        &dir,
        &[
            "config",
            "difftool.shout.cmd",
            "echo \"$LOCAL\" \"$REMOTE\" | wc -w",
        ],
    );
    assert_eq!(ok(&dir, &["difftool", "-y"]).trim(), "2");
    ok(&dir, &["difftool", "--tool-help"]);
    git(&dir, &["checkout", "--", "a.txt"]);

    // A conflict resolved by a tool that takes the other side.
    git(&dir, &["checkout", "-qb", "other", "HEAD~1"]);
    commit(&dir, "a.txt", "one\nTWO\nthree\n", "theirs");
    git(&dir, &["checkout", "-q", "main"]);
    let mut merge = Command::new("git");
    merge.arg("-C").arg(&dir).args(["merge", "-q", "other"]);
    isolate(&mut merge, &dir);
    assert!(!merge.output().unwrap().status.success());
    git(&dir, &["config", "merge.tool", "theirs"]);
    git(
        &dir,
        &[
            "config",
            "mergetool.theirs.cmd",
            "cat \"$REMOTE\" > \"$MERGED\"",
        ],
    );
    ok(&dir, &["mergetool", "-y"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nTWO\nthree\n"
    );
    assert_eq!(git(&dir, &["diff", "--name-only", "--diff-filter=U"]), "");
    assert!(dir.join("a.txt.orig").exists());
    assert_eq!(ok(&dir, &["mergetool", "-y"]), "No files need merging\n");
}

#[test]
fn format_patch_versions_signoff_and_descriptions_match_git() {
    let dir = repo("format-patch-versions");
    let ten: String = (1..=10).map(|n| format!("{n}\n")).collect();
    commit(&dir, "a.txt", &ten, "ten");
    let version = |branch: &str, two: &str, body: &str| {
        git(&dir, &["checkout", "-qb", branch, "main"]);
        std::fs::write(
            dir.join("a.txt"),
            ten.replace("\n2\n", &format!("\n{two}\n")),
        )
        .unwrap();
        git(&dir, &["add", "."]);
        git(
            &dir,
            &[
                "-c",
                "user.name=Other",
                "commit",
                "-qm",
                &format!("change two\n\n{body}"),
            ],
        );
    };
    version("v1", "two", "Body.\n\nAcked-by: X <x@y>");
    version("v2", "TWO", "Body.");
    git(
        &dir,
        &[
            "config",
            "branch.v2.description",
            "Series title\n\nThe blurb.",
        ],
    );
    for args in [
        &["format-patch", "--stdout", "-1", "-s", "v1"][..],
        &["format-patch", "--stdout", "-1", "-s", "--from", "v2"],
        &[
            "format-patch",
            "--stdout",
            "-1",
            "--rfc=WIP",
            "--no-binary",
            "v2",
        ],
        &[
            "format-patch",
            "--stdout",
            "-1",
            "--cover-letter",
            "--range-diff=v1",
            "v2",
        ],
        &[
            "format-patch",
            "--stdout",
            "-1",
            "--cover-letter",
            "-v2",
            "--cover-from-description=subject",
            "--interdiff=v1",
            "--range-diff=v1",
            "v2",
        ],
        &["format-patch", "--stdout", "-1", "--range-diff=v1", "v2"],
    ] {
        assert_eq!(mask(&ok(&dir, args)), mask(&git(&dir, args)), "{args:?}");
    }
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["cherry-pick", "v1"]);
    assert_eq!(
        ok(
            &dir,
            &[
                "format-patch",
                "--stdout",
                "--ignore-if-in-upstream",
                "main..v1"
            ]
        ),
        ""
    );
}

fn toon(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    cmd.arg("--toon").args(args).current_dir(dir);
    isolate(&mut cmd, dir);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "rgit {args:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn fsck_repack_pack_refs_and_maintenance() {
    let dir = repo("fsck-more");
    commit(&dir, "a.txt", "two\n", "second");
    git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&dir, &["reflog", "expire", "--expire=now", "--all"]);
    for args in [
        &["fsck", "--name-objects", "--root"][..],
        &["fsck", "--unreachable", "--no-reflogs"],
    ] {
        assert_eq!(ok(&dir, args), git(&dir, args), "{args:?}");
    }
    let out = toon(&dir, &["fsck", "--no-reflogs"]);
    assert!(out.contains("problems[1]{kind,type,id,name}:"), "{out}");
    assert!(out.contains("dangling,commit,"), "{out}");
    ok(&dir, &["fsck", "--lost-found"]);
    assert!(
        dir.join(".git/lost-found/commit")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );

    ok(&dir, &["repack", "-a", "-d"]);
    assert!(git(&dir, &["count-objects", "-v"]).contains("packs: 1\n"));
    ok(&dir, &["pack-refs", "--all"]);
    assert!(
        std::fs::read_to_string(dir.join(".git/packed-refs"))
            .unwrap()
            .contains("refs/heads/main")
    );
    let out = toon(&dir, &["gc", "--prune=now"]);
    assert!(
        out.contains("loose_objects: 0") && out.contains("packs: 1"),
        "{out}"
    );

    ok(&dir, &["maintenance", "register"]);
    let root = dir.canonicalize().unwrap();
    assert_eq!(
        git(
            &dir,
            &["config", "--global", "--get-all", "maintenance.repo"]
        )
        .trim(),
        root.to_str().unwrap()
    );
    assert_eq!(git(&dir, &["config", "maintenance.auto"]), "false\n");
    ok(&dir, &["maintenance", "run", "--task=pack-refs"]);
    ok(&dir, &["maintenance", "unregister"]);
    fails(&dir, &["maintenance", "unregister"]);
    ok(&dir, &["maintenance", "unregister", "--force"]);
}

#[test]
fn gc_and_fsck_run() {
    let dir = repo("gc");
    ok(&dir, &["gc", "--prune=now"]);
    assert_eq!(git(&dir, &["count-objects"]).split(' ').next(), Some("0"));
    ok(&dir, &["fsck", "--full"]);
}

#[test]
fn config_scopes_types_sections_and_includes_match_git() {
    let dir = repo("config-more");
    std::fs::write(
        dir.join(".git/inc.cfg"),
        "[inc]\n\tv = 3\n[Sec \"Sub\"]\n\tKey = v1\n\tkey = v2\n\tflag\n",
    )
    .unwrap();
    git(&dir, &["config", "include.path", "inc.cfg"]);
    git(&dir, &["config", "--global", "a.b", "1"]);
    for args in [
        &["config", "--show-origin", "--show-scope", "-l"][..],
        &["config", "--get-regexp", "sec.*"],
        &["config", "--get-all", "sec.Sub.key", "v[2]"],
        &["config", "--bool", "sec.Sub.flag"],
        &["config", "--local", "-l"],
        &["config", "--local", "--includes", "-l"],
        &["config", "-z", "--name-only", "--get-regexp", "^inc"],
        &[
            "config",
            "--type=color",
            "--default",
            "bold red",
            "no.color",
        ],
        &["config", "--type", "int", "--default", "2k", "no.int"],
    ] {
        assert_eq!(ok(&dir, args), git(&dir, args), "{args:?}");
    }
    // git 2.46's verb forms.
    for args in [
        &["config", "list", "--local"][..],
        &["config", "get", "--all", "sec.Sub.key"],
        &["config", "get", "--regexp", "^inc"],
        &["config", "get", "--regexp", "--all", "--show-names", "^inc"],
        &["config", "get", "--value=v1", "--all", "sec.Sub.key"],
    ] {
        assert_eq!(ok(&dir, args), git(&dir, args), "{args:?}");
    }
    ok(&dir, &["config", "set", "verb.key", "1"]);
    ok(&dir, &["config", "set", "--all", "verb.key", "2"]);
    assert_eq!(git(&dir, &["config", "verb.key"]), "2\n");
    ok(&dir, &["config", "unset", "verb.key"]);
    ok(&dir, &["config", "--add", "m.v", "a"]);
    ok(&dir, &["config", "--add", "m.v", "b"]);
    ok(&dir, &["config", "--replace-all", "m.v", "z", "a"]);
    assert_eq!(git(&dir, &["config", "--get-all", "m.v"]), "z\nb\n");
    ok(&dir, &["config", "--rename-section", "m", "n"]);
    assert_eq!(git(&dir, &["config", "--get-all", "n.v"]), "z\nb\n");
    ok(&dir, &["config", "--remove-section", "n"]);
    assert!(!git(&dir, &["config", "-l"]).contains("n.v"));

    // Outside a repository: --global, --file and plain reads work, as in git.
    let out = dir.with_extension("outside");
    std::fs::create_dir_all(&out).unwrap();
    ok(&out, &["config", "--global", "c.d", "2"]);
    assert_eq!(ok(&out, &["config", "c.d"]), "2\n");
    ok(&out, &["config", "--file", "f.cfg", "p.q", "r"]);
    assert_eq!(
        std::fs::read_to_string(out.join("f.cfg")).unwrap(),
        "[p]\n\tq = r\n"
    );
    fails(&out, &["config", "x.y", "3"]);
}
