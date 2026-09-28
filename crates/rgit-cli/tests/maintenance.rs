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
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GNUPGHOME", home.join("gnupg"));
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
fn apply_whitespace_modes_report_and_fix_like_git() {
    let dir = repo("apply-ws");
    std::fs::write(dir.join("f.txt"), "a  \n\tb\nc\nd \ne\nf\n").unwrap();
    std::fs::write(dir.join("g.txt"), "x\ny\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "ws"]);
    // Context without the file's whitespace errors, bad added lines; and
    // more errors than git reports before squelching.
    let fixed_context = "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n\
        @@ -1,6 +1,8 @@\n a\n \tb\n c\n+new  \n+  \tmixed\n d\n e\n f\n";
    let mut many = String::from(
        "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,6 +1,12 @@\n a  \n \tb\n c\n",
    );
    for i in 1..=6 {
        many += &format!("+x{i} \n");
    }
    many += " d \n e\n f\ndiff --git a/g.txt b/g.txt\n--- a/g.txt\n+++ b/g.txt\n\
        @@ -1,2 +1,3 @@\n x\n+g \n y\n";
    for (n, patch) in [fixed_context, many.as_str()].into_iter().enumerate() {
        std::fs::write(dir.join("p.diff"), patch).unwrap();
        for mode in ["fix", "error", "error-all", "warn", "nowarn"] {
            let arg = format!("--whitespace={mode}");
            let outcome = |tool: &str| {
                let copy = twin(&dir, tool);
                let mut cmd = Command::new(if tool == "git" {
                    "git"
                } else {
                    env!("CARGO_BIN_EXE_rgit")
                });
                if tool != "git" {
                    cmd.arg("--human");
                }
                cmd.args(["apply", &arg, "p.diff"]).current_dir(&copy);
                isolate(&mut cmd, &copy);
                let out = cmd.output().unwrap();
                (
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                    std::fs::read_to_string(copy.join("f.txt")).unwrap(),
                    std::fs::read_to_string(copy.join("g.txt")).unwrap(),
                )
            };
            let want = outcome("git");
            // rgit words a failed hunk its own way; the rest must agree.
            if !want.1.contains("patch failed") {
                assert_eq!(outcome("rgit"), want, "patch {n} {arg}");
            }
        }
    }
}

/// `git apply` and `rgit apply` with `args` on copies of `dir` changed by
/// `prep` agree: success, progress on stderr, the file `f` and the index.
fn same_apply(dir: &Path, prep: &str, args: &[&str]) {
    let outcome = |tool: &str| {
        let copy = twin(dir, tool);
        std::fs::write(copy.join("f"), prep).unwrap();
        let mut cmd = Command::new(if tool == "git" {
            "git"
        } else {
            env!("CARGO_BIN_EXE_rgit")
        });
        if tool != "git" {
            cmd.arg("--human");
        }
        cmd.arg("apply").args(args).current_dir(&copy);
        isolate(&mut cmd, &copy);
        let out = cmd.output().unwrap();
        let success = out.status.success();
        (
            success,
            success.then(|| String::from_utf8_lossy(&out.stderr).into_owned()),
            std::fs::read_to_string(copy.join("f")).unwrap_or_default(),
            git(&copy, &["status", "--short"]),
            git(&copy, &["ls-files", "-s"]),
        )
    };
    assert_eq!(outcome("rgit"), outcome("git"), "{prep:?} {args:?}");
}

#[test]
fn apply_places_hunks_like_git() {
    let dir = repo("apply-fuzz");
    let lines =
        |n: std::ops::RangeInclusive<u32>| -> String { n.map(|i| format!("{i}\n")).collect() };
    let base = lines(1..=20);
    commit(&dir, "f", &base, "numbers");
    let patch = |name: &str, text: &str, extra: &[&str]| {
        std::fs::write(dir.join("f"), text).unwrap();
        let mut args = vec!["diff"];
        args.extend(extra);
        let p = dir.with_extension(format!("{name}.diff"));
        std::fs::write(&p, git(&dir, &args)).unwrap();
        git(&dir, &["checkout", "-q", "f"]);
        p.display().to_string()
    };
    let one = patch("one", &base.replace("\n10\n", "\nten\n"), &[]);
    let two = patch(
        "two",
        &base
            .replace("\n3\n", "\nthree\n")
            .replace("\n15\n", "\nfifteen\n"),
        &[],
    );
    let zero = patch(
        "zero",
        &base
            .replace("\n3\n", "\nthree\n")
            .replace("\n15\n", "\nfifteen\n"),
        &["-U0"],
    );
    let end = patch("end", &base.replace("\n20\n", "\ntwenty\n"), &[]);
    let shifted = format!("x\ny\n{base}");
    let eight = base.replace("\n8\n", "\neight\n");
    let spaced = base
        .replace("\n9\n", "\n9  \n")
        .replace("\n11\n", "\n11\t\n");
    let doubled = format!("{base}{base}");
    let no_eol = base.trim_end().to_owned();
    for (prep, args) in [
        (shifted.as_str(), vec!["-v", &one]),
        (&base[8..], vec!["-v", &one]),
        (&eight, vec!["-v", &one]),
        (&eight, vec!["-v", "-C1", &one]),
        (&eight, vec!["-C2", &one]),
        (&spaced, vec![&one]),
        (&spaced, vec!["-v", "--ignore-whitespace", &one]),
        (&spaced, vec!["--ignore-space-change", &one]),
        (&base, vec![&zero]),
        (&base, vec!["-v", "--unidiff-zero", &zero]),
        (&shifted, vec!["-v", "--unidiff-zero", &zero]),
        (&base[4..], vec!["-v", "-R", &two]),
        (&doubled, vec!["-v", &two]),
        (&doubled, vec!["-v", "--allow-overlap", &two]),
        (&no_eol, vec!["-v", &end]),
        (&no_eol, vec!["-v", "--inaccurate-eof", &end]),
        (&no_eol, vec!["--inaccurate-eof", &one]),
        (&eight, vec!["--reject", &two]),
    ] {
        same_apply(&dir, prep, &args);
    }
    let (out, _) = rgit_in(&dir, &["apply", "--numstat", "-z", &two], b"");
    assert_eq!(out, "2\t2\tf\0");
}

#[test]
fn apply_intent_to_add_and_fake_ancestor_match_git() {
    let dir = repo("apply-ita");
    std::fs::write(dir.join("a.txt"), "one\n2\nthree\n").unwrap();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    git(&dir, &["add", "-N", "new.txt"]);
    let patch = dir.with_extension("ita.diff");
    std::fs::write(&patch, git(&dir, &["diff"])).unwrap();
    git(&dir, &["reset", "-q"]);
    std::fs::remove_file(dir.join("new.txt")).unwrap();
    git(&dir, &["checkout", "--", "a.txt"]);
    let patch = patch.display().to_string();
    let other = twin(&dir, "git");
    git(&other, &["apply", "-N", &patch]);
    ok(&dir, &["apply", "-N", &patch]);
    // Only the patched paths: git 2.50 also drops every other entry from
    // the index here, which rgit does not copy.
    for args in [
        &["status", "--short", "a.txt", "new.txt"][..],
        &["ls-files", "-s", "a.txt", "new.txt"],
    ] {
        assert_eq!(git(&dir, args), git(&other, args), "{args:?}");
    }
    let fake = |d: &Path| d.with_extension("fake");
    git(
        &other,
        &[
            "apply",
            &format!("--build-fake-ancestor={}", fake(&other).display()),
            &patch,
        ],
    );
    ok(
        &dir,
        &[
            "apply",
            &format!("--build-fake-ancestor={}", fake(&dir).display()),
            &patch,
        ],
    );
    assert_eq!(
        std::fs::read(fake(&dir)).unwrap(),
        std::fs::read(fake(&other)).unwrap()
    );
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
    assert_eq!(ok(&dir, &["notes"]), "");
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

/// stdout and success of `bin` (git or rgit) in `dir`, stderr dropped.
fn stdout_of(bin: &str, dir: &Path, args: &[&str]) -> (String, bool) {
    let mut cmd = Command::new(bin);
    if bin != "git" {
        cmd.arg("--human");
    }
    cmd.args(args).current_dir(dir);
    isolate(&mut cmd, dir);
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

/// Every note of every notes ref with its text, and the files of a manual
/// notes merge in progress.
fn notes_state(dir: &Path) -> String {
    let mut out = String::new();
    for r in git(dir, &["for-each-ref", "--format=%(refname)", "refs/notes"]).lines() {
        out += &format!("[{r}]\n");
        for line in git(dir, &["notes", "--ref", r, "list"]).lines() {
            let (note, obj) = line.split_once(' ').unwrap();
            out += &format!("{obj}: {:?}\n", git(dir, &["cat-file", "-p", note]));
        }
    }
    if let Ok(files) = std::fs::read_dir(dir.join(".git/NOTES_MERGE_WORKTREE")) {
        let mut files: Vec<_> = files.flatten().map(|f| f.path()).collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            out += &format!("{name}: {:?}\n", std::fs::read_to_string(&f).unwrap());
        }
    }
    for f in ["NOTES_MERGE_PARTIAL", "NOTES_MERGE_REF"] {
        out += &format!("{f}: {}\n", dir.join(".git").join(f).exists());
    }
    out
}

#[test]
fn notes_merge_is_three_way_and_manual_like_git() {
    let ours = repo("notes-merge");
    for n in 1..=4 {
        commit(&ours, "a.txt", &format!("{n}\n"), &format!("c{n}"));
    }
    for args in [
        &["notes", "add", "-m", "base1", "HEAD~1"][..],
        &["notes", "add", "-m", "l1\nl2\nl3", "HEAD~2"],
        &["notes", "add", "-m", "gone", "HEAD~3"],
        &["update-ref", "refs/notes/other", "refs/notes/commits"],
        &["notes", "add", "-f", "-m", "local1", "HEAD~1"],
        &["notes", "add", "-f", "-m", "l1\nL2\nl3", "HEAD~2"],
        &["notes", "add", "-m", "localnew", "HEAD"],
        &[
            "notes", "--ref", "other", "add", "-f", "-m", "remote1", "HEAD~1",
        ],
        &[
            "notes",
            "--ref",
            "other",
            "add",
            "-f",
            "-m",
            "l1\nR2\nl3",
            "HEAD~2",
        ],
        &["notes", "--ref", "other", "remove", "HEAD~3"],
        &["update-ref", "refs/notes/save", "refs/notes/commits"],
    ] {
        git(&ours, args);
    }
    let theirs = ours.with_extension("twin");
    let _ = std::fs::remove_dir_all(&theirs);
    let copied = Command::new("cp")
        .arg("-R")
        .arg(&ours)
        .arg(&theirs)
        .status()
        .unwrap();
    assert!(copied.success());
    let reset = ["update-ref", "refs/notes/commits", "refs/notes/save"];
    for args in [
        &["notes", "merge", "-v", "other"][..],
        &["notes", "merge", "--commit", "-v"],
        &reset,
        &["notes", "merge", "-s", "union", "other"],
        &reset,
        &["notes", "merge", "-s", "cat_sort_uniq", "-v", "other"],
        &reset,
        &["notes", "merge", "-s", "theirs", "other"],
        &reset,
        &["notes", "merge", "-s", "ours", "-q", "other"],
        &reset,
        &["notes", "merge", "-q", "other"],
        &["notes", "merge", "--abort", "-v"],
        &["notes", "merge", "--abort"],
        &["notes", "merge", "--commit"],
        &reset,
        &["notes", "--ref", "other", "merge", "-vv", "commits"],
        &["notes", "merge", "other"],
        &["notes", "--ref", "fresh", "merge", "other"],
    ] {
        if args[0] == "update-ref" {
            git(&ours, args);
            git(&theirs, args);
            continue;
        }
        let want = stdout_of("git", &theirs, args);
        let got = stdout_of(env!("CARGO_BIN_EXE_rgit"), &ours, args);
        assert_eq!(got, want, "{args:?}");
        assert_eq!(notes_state(&ours), notes_state(&theirs), "{args:?}");
    }
}

#[test]
fn notes_keep_or_clean_up_text_like_git() {
    let dir = repo("notes-stripspace");
    std::fs::write(dir.join("n.txt"), "\n\nx  \n\n").unwrap();
    let blob = git(&dir, &["hash-object", "-w", "n.txt"]);
    let blob = blob.trim();
    for args in [
        &["notes", "add", "-f", "-m", "  a  ", "-m", "", "-m", "b   "][..],
        &[
            "notes",
            "add",
            "-f",
            "--no-stripspace",
            "-m",
            "  a  ",
            "-m",
            "b   ",
        ],
        &[
            "notes",
            "add",
            "-f",
            "--no-stripspace",
            "-F",
            "n.txt",
            "-m",
            "y",
        ],
        &[
            "notes",
            "add",
            "-f",
            "-F",
            "n.txt",
            "--separator=SEP",
            "-m",
            "x",
        ],
        &["notes", "add", "-f", "-C", blob],
        &["notes", "add", "-f", "--stripspace", "-C", blob],
        &["notes", "add", "-f", "-m", "a", "--no-separator", "-m", "b"],
    ] {
        ok(&dir, args);
        let ours = git(&dir, &["notes", "show"]);
        git(&dir, args);
        assert_eq!(ours, git(&dir, &["notes", "show"]), "{args:?}");
    }
    for args in [
        &["notes", "append", "--no-stripspace", "-m", "  z  "][..],
        &["notes", "append", "-m", "  q  "],
    ] {
        let before = git(&dir, &["notes", "show"]);
        ok(&dir, args);
        let ours = git(&dir, &["notes", "show"]);
        git(
            &dir,
            &[
                "notes",
                "add",
                "-f",
                "--no-stripspace",
                "-m",
                before.as_str(),
            ],
        );
        git(&dir, args);
        assert_eq!(ours, git(&dir, &["notes", "show"]), "{args:?}");
    }
}

#[test]
fn notes_commits_match_git_history() {
    let ours = repo("notes-history");
    commit(&ours, "a.txt", "two\n", "second");
    let theirs = twin(&ours, "git");
    let mut blobs = Vec::new();
    for i in 0..100 {
        let path = ours.join(format!("b{i}"));
        std::fs::write(&path, format!("{i}\n")).unwrap();
        blobs.push(git(&ours, &["hash-object", "-w", &format!("b{i}")]));
        git(&theirs, &["hash-object", "-w", path.to_str().unwrap()]);
    }
    let mut steps: Vec<Vec<&str>> = vec![
        vec!["notes", "add", "-m", "first"],
        vec!["notes", "append", "-m", "second"],
        vec!["notes", "copy", "HEAD", "HEAD~1"],
        vec!["notes", "add", "-f", "-m", "", "HEAD~1"],
        vec!["notes", "remove", "HEAD", "HEAD~1"],
        vec!["notes", "remove", "--ignore-missing", "HEAD", "HEAD~1"],
        vec!["notes", "append", "-m", "x", "HEAD~1"],
        vec!["notes", "edit", "HEAD~1"],
        vec!["notes", "--ref", "other", "add", "-m", "theirs", "HEAD"],
        vec!["notes", "merge", "other"],
    ];
    // Enough notes that git fans the tree out into directories.
    for b in &blobs {
        steps.push(vec!["notes", "add", "-m", "n", b.trim()]);
    }
    steps.push(vec!["notes", "remove", blobs[0].trim()]);
    for step in &steps {
        let a = dated("git", &theirs, step, b"").1;
        assert_eq!(dated("rgit", &ours, step, b"").1, a, "{step:?}");
    }
    let log = |dir: &Path| {
        git(dir, &["log", "--format=raw", "--raw", "refs/notes/commits"])
            + &git(dir, &["reflog", "--format=%gs", "refs/notes/commits"])
    };
    assert_eq!(log(&ours), log(&theirs));
    assert!(git(&ours, &["ls-tree", "refs/notes/commits"]).starts_with("040000"));
}

#[test]
fn log_and_show_display_notes_and_amend_copies_them_like_git() {
    let dir = repo("notes-display");
    commit(&dir, "a.txt", "2\n", "second");
    git(&dir, &["notes", "add", "-m", "default note", "HEAD~1"]);
    git(
        &dir,
        &["notes", "--ref", "foo", "add", "-m", "foo\n\ntwo", "HEAD"],
    );
    git(
        &dir,
        &["notes", "--ref", "bar", "add", "-m", "bar note", "HEAD"],
    );
    let same = |args: &[&str]| {
        assert_eq!(
            ok(&dir, args),
            git(&dir, args),
            "{args:?} with {}",
            git(&dir, &["config", "--get-regexp", "notes|core.notes"]).trim()
        );
    };
    for args in [
        &["log", "--pretty"][..],
        &["log", "--oneline", "--notes"],
        &["log", "--notes=foo", "-1"],
        &["log", "--notes=foo", "--notes"],
        &["log", "--no-notes", "--pretty=medium"],
        &["log", "--no-notes", "--notes=bar"],
        &["log", "--format=[%N]"],
        &["log", "--format=%s", "--notes=foo"],
        &["show", "-s", "--pretty", "HEAD"],
    ] {
        same(args);
    }
    git(&dir, &["config", "notes.displayRef", "refs/notes/*"]);
    same(&["log", "--pretty"]);
    same(&["log", "--format=[%N]"]);
    same(&["show", "-s", "--pretty=fuller", "HEAD"]);
    git(&dir, &["config", "--unset", "notes.displayRef"]);
    git(&dir, &["config", "core.notesRef", "refs/notes/foo"]);
    same(&["log", "--pretty", "-1"]);
    assert_eq!(ok(&dir, &["notes", "get-ref"]), "refs/notes/foo\n");
    git(&dir, &["config", "--unset", "core.notesRef"]);

    // notes.rewriteRef: amend carries the notes over, concatenated.
    git(&dir, &["config", "notes.rewriteRef", "refs/notes/*"]);
    git(
        &dir,
        &["notes", "--ref", "foo", "add", "-m", "old", "HEAD~1"],
    );
    ok(&dir, &["commit", "--amend", "-m", "amended"]);
    assert_eq!(
        git(&dir, &["notes", "--ref", "foo", "show"]),
        "foo\n\ntwo\n"
    );
    assert_eq!(git(&dir, &["notes", "--ref", "bar", "show"]), "bar note\n");
    git(&dir, &["config", "notes.rewrite.amend", "false"]);
    ok(&dir, &["commit", "--amend", "-m", "again"]);
    let head = git(&dir, &["rev-parse", "HEAD"]);
    assert!(!git(&dir, &["notes", "--ref", "bar", "list"]).contains(head.trim()));
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

/// git's output and exit status for `args` with `stdin`.
fn git_in(dir: &Path, args: &[&str], stdin: &[u8]) -> (String, bool) {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    isolate(&mut cmd, dir);
    let mut child = cmd.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

/// rgit's stdout alone and exit status for `args` with `stdin`.
fn rgit_stdout(dir: &Path, args: &[&str], stdin: &[u8]) -> (String, bool) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    cmd.arg("--human")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    isolate(&mut cmd, dir);
    let mut child = cmd.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

/// A copy of `dir` (same objects and refs) to run git in beside rgit.
fn twin(dir: &Path, ext: &str) -> PathBuf {
    let copy = dir.with_extension(ext);
    let _ = std::fs::remove_dir_all(&copy);
    let status = Command::new("cp").arg("-R").arg(dir).arg(&copy).status();
    assert!(status.unwrap().success());
    copy
}

fn refs(dir: &Path) -> String {
    git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname) %(symref)",
            "refs/heads",
        ],
    )
}

#[test]
fn update_ref_symrefs_no_deref_and_batch_updates_match_git() {
    let dir = repo("update-ref-batch");
    for b in ["side", "y", "w"] {
        git(&dir, &["branch", b]);
    }
    git(&dir, &["symbolic-ref", "refs/heads/sym", "refs/heads/side"]);
    let ones = "1".repeat(40);
    let zeros = "0".repeat(40);
    let scripts = [
        "update refs/heads/side HEAD\ndelete refs/heads/nope\n".to_owned(),
        format!(
            "update refs/heads/z HEAD {ones}\nverify refs/heads/w {zeros}\n\
             update refs/heads/y/q HEAD\ncreate refs/heads/main HEAD\n\
             delete refs/heads/y {ones}\nupdate refs/heads/ok HEAD\n\
             option no-deref\nsymref-verify refs/heads/sym refs/heads/main\n\
             symref-create refs/heads/sy refs/heads/main\n\
             symref-update refs/heads/sy2 refs/heads/main ref refs/heads/foo\n\
             symref-create refs/heads/w/x refs/heads/main\n\
             verify refs/heads/nope2 HEAD\n\
             symref-update refs/heads/sym2 refs/heads/main oid HEAD\n\
             option no-deref\nsymref-verify refs/heads/w refs/heads/main\n"
        ),
        format!("update refs/heads/sym HEAD {ones}\ncreate refs/heads/new HEAD\n"),
        "option no-deref\nupdate refs/heads/sym HEAD~0\noption no-deref\nsymref-update refs/heads/s3 refs/heads/w\n"
            .to_owned(),
        "option no-deref\nsymref-verify refs/heads/s3 refs/heads/w\n".to_owned(),
        "option no-deref\nsymref-delete refs/heads/s3 refs/heads/w\n"
            .to_owned(),
        "start\nsymref-create refs/heads/s5 refs/heads/y\ncreate refs/heads/y HEAD\ncommit\n"
            .to_owned(),
    ];
    for (i, script) in scripts.iter().enumerate() {
        for batch in [true, false] {
            let args: &[&str] = if batch {
                &["update-ref", "--stdin", "--batch-updates"]
            } else {
                &["update-ref", "--stdin"]
            };
            let other = twin(&dir, "twin");
            let want = git_in(&other, args, script.as_bytes());
            let got = rgit_stdout(&dir, args, script.as_bytes());
            assert_eq!(got, want, "script {i} batch {batch}: {script}");
            assert_eq!(refs(&dir), refs(&other), "script {i} batch {batch}");
        }
    }
    // Symref commands that must not follow the ref need `option no-deref`.
    let (_, success) = rgit_in(
        &dir,
        &["update-ref", "--stdin"],
        b"symref-verify refs/heads/sym refs/heads/side\n",
    );
    assert!(!success);
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

/// Run git (`tool` "git") or rgit in `dir` with fixed dates, feeding `stdin`.
fn dated(tool: &str, dir: &Path, args: &[&str], stdin: &[u8]) -> (String, bool) {
    let mut cmd = if tool == "git" {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir);
        c
    } else {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rgit"));
        c.arg("--human").current_dir(dir).env("RGIT_OPLOG", "0");
        c
    };
    cmd.args(args)
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_EDITOR", "true")
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

/// Two repos with the same dated history (a.txt: one two three), and a
/// mailbox of two patches on it.
fn am_twins(tag: &str) -> (PathBuf, PathBuf, Vec<u8>) {
    let mut dirs = Vec::new();
    let mut mbox = Vec::new();
    for side in ["git", "rgit"] {
        let d =
            std::env::temp_dir().join(format!("rgit-maint-{}-{tag}-{side}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(d.with_extension("home"));
        std::fs::create_dir_all(&d).unwrap();
        let run = |args: &[&str]| {
            let (out, success) = dated("git", &d, args, b"");
            assert!(success, "{args:?}: {out}");
            out
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(d.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(d.join("b.txt"), "b\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        std::fs::write(d.join("a.txt"), "one\n2\nthree\n").unwrap();
        run(&[
            "commit",
            "-qam",
            "Caf\u{e9}: two as a digit!",
            "--author=J\u{fc}rgen \u{d6} <j@x.org>",
            "--date=2019-05-06T07:08:09+0200",
        ]);
        std::fs::write(d.join("b.txt"), "b\nb2\n").unwrap();
        run(&[
            "commit",
            "-qam",
            "[tag] add b",
            "-m",
            "Body.\n\nSigned-off-by: S <s@x>",
        ]);
        mbox = run(&["format-patch", "-2", "--stdout", "-k"]).into_bytes();
        run(&["reset", "-q", "--hard", "HEAD~2"]);
        dirs.push(d);
    }
    let b = dirs.pop().unwrap();
    (dirs.pop().unwrap(), b, mbox)
}

fn tip(dir: &Path, n: usize) -> String {
    git(
        dir,
        &["log", &format!("-{n}"), "--format=%H %an <%ae> %ad%n%B"],
    )
}

#[test]
fn am_parses_and_commits_mail_like_git() {
    let (a, b, mbox) = am_twins("am-twins");
    let file = a.with_extension("mbox");
    std::fs::write(&file, &mbox).unwrap();
    let file = file.to_str().unwrap();
    for args in [
        vec!["am", file],
        vec![
            "am",
            "-k",
            "-s",
            "-m",
            "--committer-date-is-author-date",
            file,
        ],
        vec!["am", "--keep-non-patch", "--whitespace=fix", "-p1", file],
    ] {
        assert!(dated("git", &a, &args, b"").1);
        let (out, success) = dated("rgit", &b, &args, b"");
        assert!(success, "{out}");
        assert_eq!(tip(&a, 2), tip(&b, 2), "{args:?}");
        for d in [&a, &b] {
            git(d, &["reset", "-q", "--hard", "HEAD~2"]);
        }
    }
    // From stdin.
    assert!(dated("git", &a, &["am"], &mbox).1);
    assert!(dated("rgit", &b, &["am"], &mbox).1);
    assert_eq!(tip(&a, 2), tip(&b, 2));
    for d in [&a, &b] {
        git(d, &["reset", "-q", "--hard", "HEAD~2"]);
    }

    // Encoded headers, quoted-printable, scissors and in-body headers.
    let mail = [
        "From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001",
        "From: =?UTF-8?q?J=C3=BCrgen?= <j@x.org>",
        "Date: Tue, 2 Jan 2024 10:00:00 +0100",
        "Subject: [PATCH v2 3/7] Re: =?UTF-8?q?caf=C3=A9?=",
        " fix",
        "Message-ID: <abc@x.org>",
        "Content-Type: text/plain; charset=UTF-8",
        "Content-Transfer-Encoding: quoted-printable",
        "",
        "cover chatter to cut",
        "-- >8 --",
        "From: Real Author <real@x.org>",
        "Subject: the real subject",
        "",
        "Body with =C3=A9 accent.",
        "---",
        " b.txt | 1 +",
        "",
        "diff --git a/b.txt b/b.txt",
        "--- a/b.txt",
        "+++ b/b.txt",
        "@@ -1 +1,2 @@",
        " b",
        "+c",
        "",
    ]
    .join("\n");
    for args in [vec!["am", "-c", "-m"], vec!["am", "--no-scissors"]] {
        assert!(dated("git", &a, &args, mail.as_bytes()).1);
        let (out, success) = dated("rgit", &b, &args, mail.as_bytes());
        assert!(success, "{out}");
        assert_eq!(tip(&a, 1), tip(&b, 1), "{args:?}");
        for d in [&a, &b] {
            git(d, &["reset", "-q", "--hard", "HEAD~1"]);
        }
    }
}

#[test]
fn am_sessions_continue_across_git_and_rgit() {
    let (a, b, mbox) = am_twins("am-cross");
    let file = a.with_extension("mbox");
    std::fs::write(&file, &mbox).unwrap();
    let file = file.to_str().unwrap();
    for d in [&a, &b] {
        std::fs::write(d.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        assert!(dated("git", d, &["commit", "-qam", "conflict"], b"").1);
    }
    // Both stop on the first patch with the same state.
    assert!(!dated("git", &a, &["am", file], b"").1);
    assert!(!dated("rgit", &b, &["am", file], b"").1);
    for f in [
        "next",
        "last",
        "info",
        "msg",
        "patch",
        "final-commit",
        "author-script",
        "keep",
        "messageid",
        "utf8",
        "scissors",
        "threeway",
        "quiet",
        "sign",
        "apply-opt",
        "abort-safety",
        "0001",
        "0002",
    ] {
        let read = |d: &Path| std::fs::read(d.join(".git/rebase-apply").join(f)).unwrap();
        assert_eq!(read(&a), read(&b), "{f}");
    }
    // git continues what rgit stopped, and rgit what git stopped.
    for d in [&a, &b] {
        std::fs::write(d.join("a.txt"), "one\n2\nthree\n").unwrap();
        git(d, &["add", "a.txt"]);
    }
    assert!(dated("rgit", &a, &["am", "--continue"], b"").1);
    assert!(dated("git", &b, &["am", "--continue"], b"").1);
    assert_eq!(tip(&a, 3), tip(&b, 3));

    // A three-way stop, skipped by the other tool; then an abort.
    for d in [&a, &b] {
        git(d, &["reset", "-q", "--hard", "HEAD~2"]);
    }
    assert!(!dated("git", &a, &["am", "-3", file], b"").1);
    assert!(!dated("rgit", &b, &["am", "-3", file], b"").1);
    assert_eq!(
        git(&a, &["status", "--short"]),
        git(&b, &["status", "--short"])
    );
    assert!(dated("rgit", &a, &["am", "--skip"], b"").1);
    assert!(dated("git", &b, &["am", "--skip"], b"").1);
    assert_eq!(tip(&a, 2), tip(&b, 2));
    for d in [&a, &b] {
        git(d, &["reset", "-q", "--hard", "HEAD~1"]);
    }
    let before = git(&b, &["rev-parse", "HEAD"]);
    assert!(!dated("rgit", &b, &["am", file], b"").1);
    assert!(dated("git", &b, &["am", "--abort"], b"").1);
    assert_eq!(git(&b, &["rev-parse", "HEAD"]), before);
    assert!(!dated("git", &b, &["am", file], b"").1);
    assert!(dated("rgit", &b, &["am", "--abort"], b"").1);
    assert_eq!(git(&b, &["rev-parse", "HEAD"]), before);
    assert!(!b.join(".git/rebase-apply").exists());
}

#[test]
fn am_runs_hooks_and_handles_empty_patches() {
    let (_, b, mbox) = am_twins("am-hooks");
    let hooks = b.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = |name: &str, body: &str| {
        let p = hooks.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        Command::new("chmod").arg("+x").arg(&p).status().unwrap();
    };
    hook("applypatch-msg", "sed -i.bak 's/^Caf/Hooked caf/' \"$1\"");
    hook("post-applypatch", "echo done >> .git/post-applypatch.log");
    let (out, success) = dated("rgit", &b, &["am"], &mbox);
    assert!(success, "{out}");
    assert!(git(&b, &["log", "--format=%s", "-2"]).contains("Hooked caf"));
    assert_eq!(
        std::fs::read_to_string(b.join(".git/post-applypatch.log")).unwrap(),
        "done\ndone\n"
    );
    hook("pre-applypatch", "exit 1");
    git(&b, &["reset", "-q", "--hard", "HEAD~2"]);
    assert!(!dated("rgit", &b, &["am"], &mbox).1);
    assert!(dated("rgit", &b, &["am", "--abort"], b"").1);
    let (out, success) = dated("rgit", &b, &["am", "--no-verify"], &mbox);
    assert!(success, "{out}");
    std::fs::remove_file(hooks.join("pre-applypatch")).unwrap();

    let empty = "From: A <a@x>\nSubject: [PATCH] nothing\n\nno diff here\n";
    let (out, success) = dated("rgit", &b, &["am", "-n"], empty.as_bytes());
    assert!(!success && out.contains("Patch is empty."), "{out}");
    let head = git(&b, &["rev-parse", "HEAD"]);
    assert!(dated("rgit", &b, &["am", "--allow-empty"], b"").1);
    assert_eq!(git(&b, &["rev-parse", "HEAD~1"]), head);
    assert_eq!(git(&b, &["log", "-1", "--format=%s"]), "nothing\n");
    let (out, success) = dated("rgit", &b, &["am", "--empty=drop"], empty.as_bytes());
    assert!(success && out.contains("Skipping: nothing"), "{out}");
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
    assert_eq!(names(&list(&dir, "tar", &["-tf", "s.tar"])), ["lib.rs"]);
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
        git(&dir, &["whatchanged", "-n", "2"])
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

#[test]
fn format_patch_attach_notes_encoding_and_config_match_git() {
    let dir = repo("format-patch-attach");
    commit(&dir, "a.txt", "2\n", "second é\n\nbody line");
    commit(&dir, "a.txt", "3\n", "third");
    commit(&dir, "a.txt", "4\n", "fourth\n\nSigned-off-by: X <x@y>");
    git(&dir, &["notes", "add", "-m", "a note", "HEAD~1"]);
    git(
        &dir,
        &["notes", "--ref", "other", "add", "-m", "other", "HEAD~1"],
    );
    let same = |args: &[&str]| {
        assert_eq!(mask(&ok(&dir, args)), mask(&git(&dir, args)), "{args:?}");
    };
    for args in [
        &["format-patch", "--stdout", "-3", "--attach"][..],
        &[
            "format-patch",
            "--stdout",
            "-3",
            "--attach=XYZ",
            "--numbered-files",
        ],
        &[
            "format-patch",
            "--stdout",
            "-2",
            "--inline=XYZ",
            "--cover-letter",
        ],
        &["format-patch", "--stdout", "-2", "--attach", "--no-attach"],
        &["format-patch", "--stdout", "-1", "--inline", "--attach=Q"],
        &["format-patch", "--stdout", "-3", "--attach", "-p"],
        &[
            "format-patch",
            "--stdout",
            "-1",
            "--attach",
            "-s",
            "--base=HEAD~2",
        ],
        &["format-patch", "--stdout", "-3", "--notes"],
        &["format-patch", "--stdout", "-3", "--notes=other", "--notes"],
        &["format-patch", "--stdout", "-3", "--notes", "-p"],
        &["format-patch", "--stdout", "-3", "--notes", "--attach"],
        &[
            "format-patch",
            "--stdout",
            "-3",
            "--no-encode-email-headers",
        ],
        &[
            "format-patch",
            "--stdout",
            "-2",
            "--from=T <t@t>",
            "--force-in-body-from",
        ],
        &[
            "format-patch",
            "--stdout",
            "-1",
            "--to=a@x",
            "--add-header=To: b@x",
            "--add-header=Cc: c@x",
            "--add-header=X-Foo: bar",
        ],
    ] {
        same(args);
    }
    ok(&dir, &["format-patch", "-3", "--output=r.mbox"]);
    git(&dir, &["format-patch", "-3", "--output=g.mbox"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("r.mbox")).unwrap(),
        std::fs::read_to_string(dir.join("g.mbox")).unwrap()
    );

    std::fs::write(dir.join("sig.txt"), "sig from file\n").unwrap();
    for (key, value, args) in [
        (
            "format.headers",
            "X-Cfg: one",
            &["format-patch", "--stdout", "-1"][..],
        ),
        (
            "format.to",
            "cfg@x",
            &["format-patch", "--stdout", "-1", "--to=cli@x"],
        ),
        (
            "format.numbered",
            "true",
            &["format-patch", "--stdout", "-1"],
        ),
        (
            "format.numbered",
            "false",
            &["format-patch", "--stdout", "-2"],
        ),
        ("format.thread", "deep", &["format-patch", "--stdout", "-2"]),
        (
            "format.coverLetter",
            "auto",
            &["format-patch", "--stdout", "-2"],
        ),
        (
            "format.coverLetter",
            "true",
            &["format-patch", "--stdout", "-1", "--no-cover-letter"],
        ),
        (
            "format.signOff",
            "true",
            &["format-patch", "--stdout", "-1"],
        ),
        (
            "format.from",
            "Cfg From <cf@x>",
            &["format-patch", "--stdout", "-1"],
        ),
        (
            "format.forceInBodyFrom",
            "true",
            &["format-patch", "--stdout", "-1", "--from"],
        ),
        (
            "format.signatureFile",
            "sig.txt",
            &["format-patch", "--stdout", "-1"],
        ),
        ("format.attach", "BND", &["format-patch", "--stdout", "-1"]),
        ("format.notes", "other", &["format-patch", "--stdout", "-2"]),
        (
            "format.encodeEmailHeaders",
            "false",
            &["format-patch", "--stdout", "-3"],
        ),
        (
            "format.filenameMaxLength",
            "12",
            &["format-patch", "-3", "-o", "out"],
        ),
        ("format.outputDirectory", "outdir", &["format-patch", "-2"]),
    ] {
        git(&dir, &["config", key, value]);
        same(args);
        git(&dir, &["config", "--unset-all", key]);
    }
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

/// git's stdout then its stderr, and whether it succeeded.
fn git_all(dir: &Path, args: &[&str]) -> (String, bool) {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    isolate(&mut cmd, dir);
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        out.status.success(),
    )
}

fn have(tool: &str, arg: &str) -> bool {
    let found = Command::new(tool)
        .arg(arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if !found {
        eprintln!("skipping: {tool} is not installed");
    }
    found
}

/// Both tools report the same signatures on every commit and tag.
fn same_signatures(dir: &Path) {
    let fmt = "--format=%h %G? %GS %GK %GF %GP %GT %s";
    assert_eq!(ok(dir, &["log", fmt]), git(dir, &["log", fmt]));
    for rev in ["HEAD", "HEAD~1", "HEAD~2"] {
        for args in [
            &["verify-commit", rev][..],
            &["verify-commit", "-v", rev],
            &["verify-commit", "--raw", rev],
        ] {
            let (want, good) = git_all(dir, args);
            let (got, rgood) = rgit_in(dir, args, b"");
            assert_eq!((got, rgood), (want.clone(), good), "{args:?}");
        }
    }
    for args in [
        &["verify-tag", "-v", "signed"][..],
        &["verify-tag", "plain"],
        &["verify-tag", "HEAD"],
        &["tag", "-v", "signed"],
    ] {
        let (want, good) = git_all(dir, args);
        let (got, rgood) = rgit_in(dir, args, b"");
        assert_eq!(got.trim_end(), want.trim_end(), "{args:?}");
        assert_eq!(rgood, good, "{args:?}");
    }
    let args = ["log", "-3", "--show-signature"];
    assert_eq!(git_all(dir, &args).0, ok(dir, &args));
    // fmt-merge-msg quotes signed tags with the verifier's report.
    let mut input = String::new();
    for t in ["signed", "plain"] {
        let id = git(dir, &["rev-parse", t]);
        input += &format!("{}\t\ttag '{t}' of .\n", id.trim());
        let args = ["fmt-merge-msg"];
        assert_eq!(
            rgit_stdout(dir, &args, input.as_bytes()),
            git_in(dir, &args, input.as_bytes()),
            "{input}"
        );
    }
}

#[test]
fn signs_and_verifies_with_gpg_like_git() {
    if !have("gpg", "--version") {
        return;
    }
    let dir = repo("gpg");
    let gnupg = dir.with_extension("home").join("gnupg");
    std::fs::create_dir_all(&gnupg).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&gnupg, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let mut keygen = Command::new("gpg");
    keygen
        .args(["--batch", "--pinentry-mode", "loopback", "--passphrase", ""])
        .args(["--quick-gen-key", "t <t@t>", "ed25519", "sign", "never"]);
    isolate(&mut keygen, &dir);
    assert!(keygen.output().unwrap().status.success());
    // gpg's first check prints its trustdb lines once.
    git(
        &dir,
        &["commit", "-q", "--allow-empty", "-S", "-m", "by git"],
    );
    git_all(&dir, &["verify-commit", "HEAD"]);

    ok(&dir, &["commit", "--allow-empty", "-S", "-m", "by rgit"]);
    ok(&dir, &["tag", "-s", "-m", "signed tag", "signed"]);
    ok(&dir, &["tag", "-a", "-m", "plain tag", "plain"]);
    same_signatures(&dir);

    git(&dir, &["config", "commit.gpgSign", "true"]);
    ok(&dir, &["commit", "--allow-empty", "-m", "by config"]);
    ok(
        &dir,
        &["commit", "--allow-empty", "--no-gpg-sign", "-m", "unsigned"],
    );
    git(&dir, &["config", "commit.gpgSign", "false"]);
    git(&dir, &["switch", "-qc", "side", "HEAD~2"]);
    commit(&dir, "side.txt", "side\n", "side");
    git(&dir, &["switch", "-q", "main"]);
    ok(
        &dir,
        &["merge", "-S", "--no-ff", "-m", "merge side", "side"],
    );
    git(&dir, &["switch", "-q", "side"]);
    commit(&dir, "side.txt", "more\n", "more");
    git(&dir, &["switch", "-q", "main"]);
    ok(&dir, &["cherry-pick", "-S", "side"]);
    ok(&dir, &["revert", "-S", "HEAD"]);
    assert_eq!(
        git(&dir, &["log", "-6", "--first-parent", "--format=%G? %s"]),
        "G Revert \"more\"\nG more\nG merge side\nN unsigned\nG by config\nG by rgit\n"
    );
    commit(&dir, "r1.txt", "1\n", "r1");
    commit(&dir, "r2.txt", "2\n", "r2");
    ok(&dir, &["rebase", "-f", "-S", "HEAD~2"]);
    assert_eq!(
        git(&dir, &["log", "-3", "--format=%G? %s"]),
        "G r2\nG r1\nG Revert \"more\"\n"
    );
    let patch = git(&dir, &["format-patch", "-1", "--stdout", "HEAD"]);
    std::fs::write(dir.with_extension("patch"), patch).unwrap();
    git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
    ok(
        &dir,
        &["am", "-S", dir.with_extension("patch").to_str().unwrap()],
    );
    assert_eq!(git(&dir, &["log", "-1", "--format=%G? %s"]), "G r2\n");
    let _ = Command::new("gpgconf")
        .args(["--kill", "gpg-agent"])
        .env("GNUPGHOME", &gnupg)
        .status();
}

#[test]
fn signs_and_verifies_with_ssh_like_git() {
    if !have("ssh-keygen", "-?") {
        return;
    }
    let dir = repo("ssh");
    let key = dir.with_extension("home").join("id");
    let other = dir.with_extension("home").join("other");
    for k in [&key, &other] {
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "t@t", "-f"])
            .arg(k)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    let allowed = dir.with_extension("home").join("allowed");
    git(&dir, &["config", "gpg.format", "ssh"]);
    git(&dir, &["config", "user.signingKey", key.to_str().unwrap()]);
    git(
        &dir,
        &["commit", "-q", "--allow-empty", "-S", "-m", "by git"],
    );
    ok(&dir, &["commit", "--allow-empty", "-S", "-m", "by rgit"]);
    ok(&dir, &["tag", "-s", "-m", "signed tag", "signed"]);
    ok(&dir, &["tag", "-a", "-m", "plain tag", "plain"]);
    // Without allowed signers nothing verifies, in either tool.
    let (want, _) = git_all(&dir, &["verify-commit", "HEAD"]);
    assert_eq!(fails(&dir, &["verify-commit", "HEAD"]), want);

    std::fs::write(&allowed, format!("t@t {public}")).unwrap();
    git(
        &dir,
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            allowed.to_str().unwrap(),
        ],
    );
    same_signatures(&dir);
    // A key no allowed signer lists is checked but not trusted.
    let public = std::fs::read_to_string(other.with_extension("pub")).unwrap();
    std::fs::write(&allowed, format!("x@x {public}")).unwrap();
    let (want, good) = git_all(&dir, &["verify-commit", "HEAD"]);
    assert_eq!(rgit_in(&dir, &["verify-commit", "HEAD"], b""), (want, good));

    // A key's validity is checked at the commit's date (-Overify-time).
    let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    std::fs::write(&allowed, format!("t@t valid-before=\"20210101\" {public}")).unwrap();
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&dir)
        .args(["commit", "-q", "--allow-empty", "-S", "-m", "old"])
        .env("GIT_COMMITTER_DATE", "2020-06-01T00:00:00Z");
    isolate(&mut cmd, &dir);
    assert!(cmd.status().unwrap().success());
    for (rev, valid) in [("HEAD", true), ("HEAD~1", false)] {
        let (want, good) = git_all(&dir, &["verify-commit", rev]);
        assert_eq!(good, valid, "{want}");
        assert_eq!(rgit_in(&dir, &["verify-commit", rev], b""), (want, good));
    }
}

/// A repo with history old enough for every expiry: tags, a merge, extra
/// branches, a dangling blob and an unreachable commit; and two copies of it
/// for git and rgit.
fn twins(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("rgit-twin-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let run = |args: &[&str]| {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&base).args(args);
        isolate(&mut cmd, &base);
        cmd.env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z");
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "t@t"]);
    run(&["config", "user.name", "t"]);
    for i in 1..=4 {
        std::fs::write(base.join(format!("f{i}")), format!("{i}\n")).unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", &format!("c{i}")]);
    }
    run(&["tag", "-a", "-m", "ann", "v1", "HEAD~1"]);
    run(&["tag", "light", "HEAD~2"]);
    run(&["switch", "-qc", "side", "HEAD~2"]);
    std::fs::write(base.join("s"), "s\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "side"]);
    run(&["switch", "-q", "main"]);
    run(&["merge", "-q", "--no-ff", "-m", "merge", "side"]);
    run(&["branch", "b1", "HEAD~1"]);
    std::fs::write(base.join("dangling"), "dangling\n").unwrap();
    run(&["hash-object", "-w", "dangling"]);
    std::fs::remove_file(base.join("dangling")).unwrap();
    run(&["commit", "-q", "--allow-empty", "-m", "gone"]);
    run(&["reset", "-q", "--hard", "HEAD~1"]);
    let copy = |name: &str| {
        let to = base.with_extension(name);
        let _ = std::fs::remove_dir_all(&to);
        assert!(
            Command::new("cp")
                .arg("-R")
                .arg(&base)
                .arg(&to)
                .status()
                .unwrap()
                .success()
        );
        to
    };
    (copy("git"), copy("rgit"))
}

/// Every object, as `<id> <type> <size>` lines.
fn objects(dir: &Path) -> String {
    let mut lines: Vec<String> = git(dir, &["cat-file", "--batch-all-objects", "--batch-check"])
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines.join("\n")
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(".git").join(file)).unwrap_or_default()
}

#[test]
fn pack_refs_and_reflog_expire_write_what_git_writes() {
    for args in [&["pack-refs", "--all"][..], &["pack-refs"]] {
        let (g, r) = twins("pack-refs");
        git(&g, args);
        ok(&r, args);
        assert_eq!(read(&r, "packed-refs"), read(&g, "packed-refs"), "{args:?}");
        assert_eq!(
            r.join(".git/refs/heads/main").exists(),
            g.join(".git/refs/heads/main").exists()
        );
    }
    let logs = |d: &Path| {
        ["logs/HEAD", "logs/refs/heads/main", "logs/refs/heads/side"]
            .map(|f| read(d, f))
            .join("--\n")
    };
    for args in [
        &["reflog", "expire", "--expire=now", "--all"][..],
        &[
            "reflog",
            "expire",
            "--expire=never",
            "--expire-unreachable=now",
            "--all",
        ],
        &["reflog", "delete", "--rewrite", "HEAD@{1}", "main@{0}"],
        &["reflog", "delete", "HEAD@{0}"],
    ] {
        let (g, r) = twins("reflog");
        git(&g, args);
        ok(&r, args);
        assert_eq!(logs(&r), logs(&g), "{args:?}");
    }
    let (g, r) = twins("reflog-verbose");
    let args = [
        "reflog",
        "expire",
        "-n",
        "--verbose",
        "--expire=never",
        "--expire-unreachable=now",
        "HEAD",
    ];
    assert_eq!(ok(&r, &args), git(&g, &args));
    assert_eq!(logs(&r), logs(&g));
    ok(&r, &["reflog", "exists", "refs/heads/main"]);
    fails(&r, &["reflog", "exists", "main"]);
}

#[test]
fn prune_repack_and_gc_keep_what_git_keeps() {
    let (g, r) = twins("prune");
    for d in [&g, &r] {
        git(d, &["reflog", "expire", "--expire=now", "--all"]);
    }
    let sorted = |s: String| {
        let mut l: Vec<String> = s.lines().map(str::to_owned).collect();
        l.sort();
        l
    };
    assert_eq!(
        sorted(ok(&r, &["prune", "-n"])),
        sorted(git(&g, &["prune", "-n"]))
    );
    git(&g, &["prune"]);
    ok(&r, &["prune"]);
    assert_eq!(objects(&r), objects(&g));

    for args in [
        &["repack"][..],
        &["repack", "-a", "-d"],
        &["repack", "-A", "-d"],
        &["gc", "--prune=now"],
        &["gc"],
        &["gc", "--no-cruft"],
    ] {
        let (g, r) = twins("repack");
        git(&g, args);
        ok(&r, args);
        assert_eq!(objects(&r), objects(&g), "{args:?}");
        let counts = |d: &Path| {
            git(d, &["count-objects", "-v"])
                .lines()
                .filter(|l| l.starts_with("count:") || l.starts_with("packs:"))
                .collect::<Vec<_>>()
                .join(",")
        };
        assert_eq!(counts(&r), counts(&g), "{args:?}");
        git(&r, &["fsck", "--no-dangling"]);
        if args[0] == "gc" {
            git(&r, &["commit-graph", "verify"]);
            assert_eq!(
                std::fs::read(r.join(".git/objects/info/commit-graph")).unwrap(),
                std::fs::read(g.join(".git/objects/info/commit-graph")).unwrap()
            );
            assert_eq!(read(&r, "packed-refs"), read(&g, "packed-refs"));
        }
    }
    let (_, r) = twins("gc-auto");
    ok(&r, &["gc", "--auto"]);
    assert!(git(&r, &["count-objects", "-v"]).contains("packs: 0"));
}

#[test]
fn maintenance_runs_tasks_and_writes_schedules() {
    let (g, r) = twins("maint");
    let args = [
        "maintenance",
        "run",
        "--task=commit-graph",
        "--task=loose-objects",
        "--task=pack-refs",
        "--task=reflog-expire",
        "--task=rerere-gc",
        "--task=worktree-prune",
    ];
    git(&g, &args);
    ok(&r, &args);
    assert_eq!(objects(&r), objects(&g));
    assert_eq!(read(&r, "packed-refs"), read(&g, "packed-refs"));
    git(&r, &["commit-graph", "verify"]);
    fails(&r, &["maintenance", "run", "--task=bogus"]);
    fails(&r, &["maintenance", "run", "--task=gc", "--task=gc"]);
    fails(&r, &["maintenance", "run", "--schedule=never"]);
    ok(&r, &["maintenance", "run", "--auto"]);

    // A remote's branches arrive under refs/prefetch, and nothing else moves.
    let (_, clone) = twins("maint-clone");
    git(&clone, &["remote", "add", "origin", r.to_str().unwrap()]);
    git(
        &clone,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    ok(&clone, &["maintenance", "run", "--task=prefetch"]);
    let refs = git(&clone, &["for-each-ref", "--format=%(refname)"]);
    assert!(refs.contains("refs/prefetch/remotes/origin/main"), "{refs}");
    assert!(!refs.contains("refs/remotes/origin/"), "{refs}");

    let sched = r.with_extension("sched");
    let _ = std::fs::remove_dir_all(&sched);
    let start = |scheduler: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
        cmd.args(["--human", "maintenance", "start", "--scheduler", scheduler])
            .current_dir(&r)
            .env("RGIT_TEST_MAINT_SCHEDULER_DIR", &sched);
        isolate(&mut cmd, &r);
        assert!(cmd.status().unwrap().success());
    };
    let stop = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
        cmd.args(["--human", "maintenance", "stop"])
            .current_dir(&r)
            .env("RGIT_TEST_MAINT_SCHEDULER_DIR", &sched);
        isolate(&mut cmd, &r);
        assert!(cmd.status().unwrap().success());
    };
    start("crontab");
    let cron = std::fs::read_to_string(sched.join("crontab")).unwrap();
    assert!(
        cron.contains(
            "for-each-repo --keep-going --config=maintenance.repo maintenance run --schedule=hourly"
        ),
        "{cron}"
    );
    assert_eq!(
        git(&r, &["config", "maintenance.strategy"]),
        "incremental\n"
    );
    start("launchctl");
    let plist = std::fs::read_to_string(sched.join("org.rgit.rgit.daily.plist")).unwrap();
    assert!(
        plist.contains("<string>--schedule=daily</string>"),
        "{plist}"
    );
    start("systemd-timer");
    assert!(sched.join("rgit-maintenance@weekly.timer").exists());
    assert!(sched.join("rgit-maintenance@.service").exists());
    if cfg!(target_os = "macos") {
        stop();
        assert!(!sched.join("org.rgit.rgit.daily.plist").exists());
    }

    // for-each-repo runs the command in each registered repository.
    let out = ok(
        &r,
        &[
            "for-each-repo",
            "--config=maintenance.repo",
            "rev-parse",
            "--git-dir",
        ],
    );
    assert_eq!(out, ".git\n");
}

/// stdout, stderr and exit code of `tool args` in `dir`.
fn streams(tool: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(tool);
    if tool != "git" {
        cmd.arg("--human");
    }
    cmd.args(args).current_dir(dir);
    isolate(&mut cmd, dir);
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn fsck_reports_what_git_fsck_reports() {
    let (dir, _) = twins("fsck");
    std::fs::write(dir.join("st"), "staged\n").unwrap();
    git(&dir, &["add", "st"]);
    let rgit = env!("CARGO_BIN_EXE_rgit");
    let same = |dir: &Path, args: &[&str]| {
        assert_eq!(
            streams(rgit, dir, args),
            streams("git", dir, args),
            "{args:?}"
        );
    };
    let flags: [&[&str]; 11] = [
        &["fsck"],
        &["fsck", "--no-reflogs"],
        &["fsck", "--unreachable", "--no-reflogs"],
        &["fsck", "--name-objects", "--root", "--tags"],
        &["fsck", "--name-objects", "--unreachable", "--no-reflogs"],
        &["fsck", "--connectivity-only", "--no-reflogs"],
        &["fsck", "--no-dangling", "HEAD~1"],
        &["fsck", "HEAD~1"],
        &["fsck", "--cache", "HEAD~1"],
        &["fsck", "--no-full"],
        &["fsck", "--strict"],
    ];
    for args in flags {
        same(&dir, args);
    }
    git(&dir, &["gc", "-q", "--prune=now"]);
    for args in flags {
        same(&dir, args);
    }

    // Broken objects, in git's words, and exit code 1.
    let literally = |kind: &str, data: &[u8]| {
        let mut cmd = Command::new("git");
        cmd.args(["hash-object", "-t", kind, "-w", "--stdin", "--literally"])
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        isolate(&mut cmd, &dir);
        let mut child = cmd.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), data).unwrap();
        assert!(child.wait_with_output().unwrap().status.success());
    };
    let empty = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n";
    literally(
        "commit",
        format!("{empty}author x x> 1 +0000\ncommitter x <x> 1 +0000\n\nm\n").as_bytes(),
    );
    literally(
        "commit",
        format!("{empty}committer x <x> 1 +0000\n\nm\n").as_bytes(),
    );
    literally(
        "commit",
        format!("{empty}author x <x> 01 +0000\ncommitter x <x> 1 +0000\n\nm\n").as_bytes(),
    );
    literally(
        "tag",
        b"object 4b825dc642cb6eb9a060e54bf8d69288fbee4904\ntype tree\ntag x\n\nm\n",
    );
    let id: Vec<u8> = (1..=20).collect();
    let mut tree = b"100644 b\0".to_vec();
    tree.extend(&id);
    tree.extend(b"100644 a\0");
    tree.extend(&id);
    literally("tree", &tree);
    let mut tree = b"100664 a\0".to_vec();
    tree.extend(&id);
    literally("tree", &tree);
    same(&dir, &["fsck", "--no-reflogs"]);
    same(&dir, &["fsck", "--strict", "--no-reflogs"]);

    let lost = |tool: &str| {
        let _ = std::fs::remove_dir_all(dir.join(".git/lost-found"));
        streams(tool, &dir, &["fsck", "--lost-found"]);
        let mut found: Vec<String> = ["commit", "other"]
            .iter()
            .flat_map(|k| {
                std::fs::read_dir(dir.join(".git/lost-found").join(k))
                    .into_iter()
                    .flatten()
            })
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        found.sort();
        found
    };
    assert_eq!(lost(rgit), lost("git"));
}

/// stdout and stderr of `bin args` in `dir`, isolated, with `env` set.
fn raw(bin: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (Vec<u8>, Vec<u8>, bool) {
    let mut cmd = Command::new(bin);
    if bin != "git" {
        cmd.arg("--human");
    }
    cmd.args(args).current_dir(dir).envs(env.iter().copied());
    isolate(&mut cmd, dir);
    let out = cmd.output().unwrap();
    (out.stdout, out.stderr, out.status.success())
}

/// rgit and git write the same bytes and agree on success for each case, and
/// on success print the same stderr (-v's paths).
fn same_bytes(dir: &Path, cases: &[&[&str]], env: &[(&str, &str)]) {
    for args in cases {
        let want = raw("git", dir, args, env);
        let got = raw(env!("CARGO_BIN_EXE_rgit"), dir, args, env);
        assert!(want.0 == got.0, "stdout of {args:?} differs");
        assert_eq!(got.2, want.2, "{args:?}");
        if want.2 {
            assert_eq!(
                String::from_utf8_lossy(&got.1),
                String::from_utf8_lossy(&want.1),
                "{args:?}"
            );
        }
    }
}

#[test]
fn archive_writes_gits_bytes() {
    let dir = repo("archive-bytes");
    let long = "a".repeat(62);
    let deep = format!("d/{long}/{long}/a-longish-file-name.txt");
    let wide = format!("e/{}/x", long.repeat(3));
    for (path, text) in [
        (&deep[..], "deep\n"),
        (&wide[..], "wide\n"),
        ("bin.dat", "bin\0ary"),
        ("ign/a", "x\n"),
        ("keep/empty/.gitkeep", "y\n"),
        ("café.txt", "caf\n"),
        ("subst.txt", "$Format:%H %s$\n"),
        ("t.txt", "text\n"),
        ("run.sh", "run\n"),
    ] {
        std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
        std::fs::write(dir.join(path), text).unwrap();
    }
    std::fs::write(
        dir.join(".gitattributes"),
        "ign export-ignore\nkeep/empty/.gitkeep export-ignore\nsubst.txt export-subst\n\
         t.txt -diff\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("t.txt", dir.join("link")).unwrap();
    std::os::unix::fs::symlink(&deep, dir.join("longlink")).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["update-index", "--chmod=+x", "run.sh"]);
    let gitlink = format!("160000,{},sub", "1".repeat(40));
    git(&dir, &["update-index", "--add", "--cacheinfo", &gitlink]);
    git(&dir, &["commit", "-qm", "corners"]);
    same_bytes(
        &dir,
        &[
            &["archive", "HEAD"],
            &["archive", "-v", "--prefix=p//", "HEAD"],
            &["archive", "--format=tgz", "HEAD"],
            &["archive", "--format=tar.gz", "-9", "HEAD"],
            &["archive", "--format=zip", "HEAD"],
            &["archive", "--format=zip", "-0", "HEAD", "d", "bin.dat"],
            &[
                "archive",
                "--mtime=2020-01-02 03:04:05 +0000",
                "HEAD^{tree}",
            ],
            &[
                "archive",
                "--add-virtual-file=v.txt:hi",
                "--prefix=q/",
                "--add-file=t.txt",
                "HEAD",
                "t.txt",
            ],
            &["archive", "HEAD", "nope"],
        ],
        &[],
    );
    same_bytes(&dir.join("d"), &[&["archive", "-v", "HEAD"]], &[]);
    for name in ["x.zip", "x.tar.gz", "x.tgz", ".tgz"] {
        git(&dir, &["archive", "-o", &format!("../g{name}"), "HEAD"]);
        ok(&dir, &["archive", "-o", &format!("../r{name}"), "HEAD"]);
        let read = |p: String| std::fs::read(dir.join(p)).unwrap();
        assert!(
            read(format!("../g{name}")) == read(format!("../r{name}")),
            "{name}"
        );
    }
    git(&dir, &["config", "tar.umask", "0022"]);
    git(&dir, &["config", "tar.tar.cat.command", "cat"]);
    same_bytes(
        &dir,
        &[&["archive", "-l"], &["archive", "--format=tar.cat", "HEAD"]],
        &[],
    );
}

#[test]
fn archive_converts_like_a_checkout_and_streams_big_files() {
    let dir = repo("archive-convert");
    for (path, text) in [
        ("crlf.txt", "a\nb\r\nc\n"),
        ("auto.txt", "x\ny\n"),
        ("mixed.txt", "x\r\ny\n"),
        ("nul.txt", "x\0\ny\n"),
        ("lf.txt", "l\nf\n"),
        ("ident.txt", "$Id$ and $Id: old $ and $Id: x y $\n"),
        ("up.txt", "shout\n"),
        ("broken.txt", "kept\n"),
    ] {
        std::fs::write(dir.join(path), text).unwrap();
    }
    std::fs::write(dir.join("big.txt"), "big line\n".repeat(500)).unwrap();
    std::fs::write(
        dir.join(".gitattributes"),
        "crlf.txt text eol=crlf\nauto.txt text=auto\nmixed.txt text=auto\nlf.txt eol=lf\n\
         ident.txt ident\nup.txt filter=up\nbroken.txt filter=broken\nbig.txt text eol=crlf\n",
    )
    .unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "convert"]);
    git(&dir, &["config", "filter.up.smudge", "tr a-z A-Z"]);
    git(&dir, &["config", "filter.broken.smudge", "false"]);
    git(&dir, &["config", "core.bigFileThreshold", "1k"]);
    let cases: &[&[&str]] = &[
        &["archive", "HEAD"],
        &["archive", "--format=zip", "HEAD"],
        &["archive", "--format=zip", "-0", "HEAD"],
    ];
    same_bytes(&dir, cases, &[]);
    for (key, value) in [("core.autocrlf", "true"), ("core.eol", "crlf")] {
        git(&dir, &["config", key, value]);
        same_bytes(&dir, cases, &[]);
        git(&dir, &["config", "--unset", key]);
    }
    git(&dir, &["config", "filter.broken.required", "true"]);
    same_bytes(&dir, &[&["archive", "HEAD"]], &[]);

    // Past 65535 entries git adds zip64 records.
    let many = repo("archive-zip64");
    let blob = git(&many, &["hash-object", "-w", "a.txt"]);
    let listing: String = (0..65536)
        .map(|i| format!("100644 blob {}\tf{i}\n", blob.trim()))
        .collect();
    let mut cmd = Command::new("git");
    cmd.args(["mktree"])
        .current_dir(&many)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    isolate(&mut cmd, &many);
    let mut child = cmd.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), listing.as_bytes()).unwrap();
    let tree = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    same_bytes(
        &many,
        &[&[
            "archive",
            "--format=zip",
            "--mtime=2020-01-01 00:00:00 +0000",
            tree.trim(),
        ]],
        &[],
    );
}

#[test]
fn archive_remote_speaks_upload_archive() {
    let dir = repo("archive-remote");
    let ssh = dir.with_extension("ssh");
    // An ssh stand-in: drop the options and host, run the command here.
    std::fs::write(
        &ssh,
        "#!/bin/sh\nwhile [ \"$1\" = -p ]; do shift 2; done\nshift\n\
         PATH=\"$(git --exec-path):$PATH\" exec sh -c \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let remote = format!("--remote=host:{}", dir.display());
    let env = [("GIT_SSH_COMMAND", ssh.to_str().unwrap())];
    same_bytes(
        &dir,
        &[
            &["archive", &remote, "HEAD"],
            &["archive", &remote, "--format=zip", "-v", "HEAD", "src"],
            &["archive", &remote, "-l"],
            &["archive", &remote, "nosuchref"],
        ],
        &env,
    );
    let url = format!("--remote=ssh://me@host:22{}", dir.display());
    let (got, _, success) = raw(
        env!("CARGO_BIN_EXE_rgit"),
        &dir,
        &["archive", &url, "HEAD"],
        &env,
    );
    assert!(success);
    assert!(got == raw("git", &dir, &["archive", "HEAD"], &[]).0);
}

#[test]
fn range_diff_pairs_like_git() {
    let dir = repo("range-diff-pairs");
    let lines: String = (1..=200).map(|n| format!("{n}\n")).collect();
    commit(&dir, "a.txt", &lines, "base");
    let series = |branch: &str, edits: &[(usize, &str)]| {
        git(&dir, &["checkout", "-qb", branch, "main"]);
        let mut text: Vec<String> = lines.lines().map(str::to_owned).collect();
        for (n, (line, word)) in edits.iter().enumerate() {
            text[line - 1] = (*word).to_owned();
            let body = text.join("\n") + "\n";
            commit(
                &dir,
                "a.txt",
                &body,
                &format!("edit {line}\n\nstep {n}\n\tindented"),
            );
        }
    };
    series(
        "v1",
        &[
            (10, "a"),
            (20, "b"),
            (30, "c"),
            (40, "d"),
            (50, "f"),
            (70, "same"),
            (90, "i"),
        ],
    );
    series(
        "v2",
        &[
            (20, "b"),
            (10, "a"),
            (30, "cc"),
            (40, "d"),
            (55, "new"),
            (70, "same"),
            (120, "k"),
        ],
    );
    git(&dir, &["notes", "add", "-m", "note one\nsecond", "v1~2"]);
    git(&dir, &["notes", "add", "-m", "note changed", "v2~2"]);
    for args in [
        &["range-diff", "main", "v1", "v2"][..],
        &["range-diff", "--creation-factor=200", "main", "v2", "v1"],
        &[
            "range-diff",
            "-s",
            "--creation-factor=1000",
            "main",
            "v1",
            "v2",
        ],
        &["range-diff", "-U1", "--no-notes", "main", "v1", "v2"],
        &["range-diff", "--left-only", "v1...v2"],
        &["range-diff", "main", "v1", "v2", "--", "a.txt"],
    ] {
        assert_eq!(
            ok(&dir, args),
            git(&dir, &[&["-c", "color.ui=never"][..], args].concat()),
            "{args:?}"
        );
    }
}

/// A repo with many revisions of a few files, so packs have deltas to find.
fn history(tag: &str) -> PathBuf {
    let dir = repo(tag);
    for i in 1..=40 {
        let text: String = (0..150 + i * 4).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.join("a.txt"), format!("{text}rev {i}\n")).unwrap();
        commit(
            &dir,
            "src/lib.rs",
            &format!("fn f{i}() {{}}\n"),
            &format!("c{i}"),
        );
    }
    git(&dir, &["tag", "-a", "-m", "t", "v1", "HEAD~3"]);
    dir
}

fn pack_files(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir.join(".git/objects/pack"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn repack_searches_deltas_and_writes_bitmaps_git_reads() {
    let dir = history("delta");
    ok(
        &dir,
        &[
            "repack",
            "-a",
            "-d",
            "-f",
            "--window=20",
            "--depth=3",
            "--threads=2",
            "--window-memory=1m",
            "-b",
        ],
    );
    let packs = pack_files(&dir);
    for ext in ["pack", "idx", "rev", "bitmap"] {
        let n = packs.iter().filter(|p| p.ends_with(ext)).count();
        assert_eq!(n, 1, "{packs:?}");
    }
    let idx = packs.iter().find(|p| p.ends_with(".idx")).unwrap();
    let verify = git(
        &dir,
        &["verify-pack", "-v", &format!(".git/objects/pack/{idx}")],
    );
    assert!(verify.contains("chain length = 1:"), "{verify}");
    assert!(!verify.contains("chain length = 4:"), "{verify}");
    git(&dir, &["fsck", "--full", "--strict"]);
    for args in [
        &["rev-list", "--count", "HEAD"][..],
        &["rev-list", "--objects", "--all"],
        &["rev-list", "--count", "v1"],
    ] {
        let with = [&args[..1], &["--use-bitmap-index"], &args[1..]].concat();
        // Bitmap walks list objects without their paths.
        let ids = |s: String| -> Vec<String> {
            s.lines()
                .map(|l| l.split(' ').next().unwrap_or("").to_owned())
                .collect()
        };
        let mut a = ids(git(&dir, &with));
        let mut b = ids(git(&dir, args));
        a.sort();
        b.sort();
        assert_eq!(a, b, "{args:?}");
    }
    git(&dir, &["rev-list", "--test-bitmap", "HEAD"]);
    assert!(fails(&dir, &["repack", "-b"]).contains("incompatible with bitmap"));

    ok(&dir, &["gc", "--aggressive"]);
    git(&dir, &["fsck", "--full"]);
    let idx = pack_files(&dir)
        .into_iter()
        .find(|p| p.ends_with(".idx"))
        .unwrap();
    git(&dir, &["verify-pack", &format!(".git/objects/pack/{idx}")]);
}

/// A commit with fixed dates, so twins stay twins.
fn commit_fixed(dir: &Path, file: &str, msg: &str) {
    std::fs::write(dir.join(file), msg).unwrap();
    git(dir, &["add", "."]);
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["commit", "-qm", msg])
        .env("GIT_AUTHOR_DATE", "2021-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2021-01-01T00:00:00Z");
    isolate(&mut cmd, dir);
    assert!(cmd.status().unwrap().success());
}

fn graph_files(d: &Path) -> Vec<(String, Vec<u8>)> {
    let info = d.join(".git/objects/info");
    let mut out = vec![(
        "commit-graph".to_owned(),
        std::fs::read(info.join("commit-graph")).unwrap_or_default(),
    )];
    for e in std::fs::read_dir(info.join("commit-graphs"))
        .into_iter()
        .flatten()
        .flatten()
    {
        out.push((
            e.file_name().to_string_lossy().into_owned(),
            std::fs::read(e.path()).unwrap(),
        ));
    }
    out.sort();
    out
}

#[test]
fn commit_graph_chains_and_bloom_filters_match_git() {
    let (g, r) = twins("cgraph");
    let same = |args: &[&str]| {
        let all = [&["commit-graph", "write"][..], args].concat();
        git(&g, &all);
        ok(&r, &all);
        assert_eq!(graph_files(&r), graph_files(&g), "{args:?}");
        git(&r, &["commit-graph", "verify"]);
        ok(&r, &["commit-graph", "verify"]);
    };
    let grow = |n: &str| {
        for d in [&g, &r] {
            commit_fixed(d, n, n);
        }
    };
    same(&["--reachable", "--changed-paths"]);
    same(&["--reachable", "--split"]);
    grow("x1");
    same(&["--reachable", "--split=no-merge"]);
    grow("x2");
    grow("x3");
    same(&["--reachable", "--split", "--size-multiple=10"]);
    grow("x4");
    same(&["--reachable", "--split", "--max-commits=2"]);
    same(&["--reachable", "--split=replace"]);
    same(&[]);
    same(&["--reachable"]);
    // git's log -- <path> reads rgit's filters.
    assert_eq!(
        git(&r, &["log", "--format=%s", "--", "x2"]),
        git(&g, &["log", "--format=%s", "--", "x2"])
    );

    let graph = r.join(".git/objects/info/commit-graph");
    let mut data = std::fs::read(&graph).unwrap();
    let at = data.len() - 30;
    data[at] ^= 0xff;
    std::fs::write(&graph, data).unwrap();
    let out = fails(&r, &["commit-graph", "verify"]);
    assert!(out.contains("incorrect checksum"), "{out}");
    let (report, good) = rgit_in(&r, &["fsck"], b"");
    assert!(!good && report.contains("incorrect checksum"), "{report}");
}

#[test]
fn multi_pack_index_writes_what_git_writes() {
    let (g, r) = twins("midx");
    for d in [&g, &r] {
        for i in 0..4 {
            commit_fixed(d, &format!("p{i}"), &format!("p{i}"));
            git(d, &["repack", "-q"]);
        }
    }
    git(&g, &["multi-pack-index", "write"]);
    ok(&r, &["multi-pack-index", "write"]);
    let midx = |d: &Path| std::fs::read(d.join(".git/objects/pack/multi-pack-index")).unwrap();
    assert_eq!(midx(&r), midx(&g));
    ok(&r, &["multi-pack-index", "verify"]);
    ok(&r, &["multi-pack-index", "repack", "--batch-size=0"]);
    git(&r, &["multi-pack-index", "verify"]);
    ok(&r, &["multi-pack-index", "expire"]);
    let packs = pack_files(&r);
    assert_eq!(packs.iter().filter(|p| p.ends_with(".pack")).count(), 1);
    git(&r, &["multi-pack-index", "verify"]);
    git(&r, &["fsck"]);

    // The incremental-repack task: write, expire, then repack a batch.
    for i in 0..3 {
        commit(&r, &format!("q{i}"), "y", &format!("q{i}"));
        git(&r, &["repack", "-q"]);
    }
    for _ in 0..2 {
        ok(&r, &["maintenance", "run", "--task=incremental-repack"]);
        git(&r, &["multi-pack-index", "verify"]);
    }
    git(&r, &["fsck"]);

    let file = r.join(".git/objects/pack/multi-pack-index");
    let mut data = std::fs::read(&file).unwrap();
    let at = data.len() - 30;
    data[at] ^= 0xff;
    std::fs::write(&file, data).unwrap();
    assert!(fails(&r, &["multi-pack-index", "verify"]).contains("incorrect checksum"));
}

#[test]
fn fsck_checks_gitmodules_and_attributes_like_git() {
    let dir = repo("fsck-special");
    std::fs::write(
        dir.join(".gitmodules"),
        "[submodule \"../evil\"]\n\tpath = -x\n\turl = -oProxyCommand=boom\n\tupdate = !rm\n\
         [submodule \"ok\"]\n\tpath = ok\n\turl = https:///example.com/x.git\n\
         [submodule \"rel\"]\n\turl = ../../:foo\n",
    )
    .unwrap();
    let long = format!("{} text\n", "a".repeat(3000));
    std::fs::write(dir.join(".gitattributes"), long).unwrap();
    std::os::unix::fs::symlink("target", dir.join(".gitignore")).unwrap();
    std::os::unix::fs::symlink("target", dir.join(".mailmap")).unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "special"]);
    let (link, _) = git_in(&dir, &["hash-object", "-w", "--stdin"], b"x");
    let entries = format!(
        "120000 blob {0}\t.gitmodules\n120000 blob {0}\t.GITATTRIBUTES\n",
        link.trim()
    );
    let (tree, _) = git_in(&dir, &["mktree"], entries.as_bytes());
    let (head, _) = git_in(
        &dir,
        &["commit-tree", tree.trim(), "-p", "HEAD", "-m", "links"],
        b"",
    );
    git(&dir, &["update-ref", "refs/heads/main", head.trim()]);
    let sorted = |s: String| {
        let mut l: Vec<String> = s.lines().map(str::to_owned).collect();
        l.sort();
        l
    };
    let check = |args: &[&str]| {
        let (want, good) = git_all(&dir, args);
        let (got, rgood) = rgit_in(&dir, args, b"");
        assert_eq!(sorted(got), sorted(want), "{args:?}");
        assert_eq!(rgood, good, "{args:?}");
    };
    check(&["fsck"]);
    check(&["fsck", "--strict"]);
    git(&dir, &["config", "fsck.gitmodulesUrl", "warn"]);
    git(&dir, &["config", "fsck.gitmodulesName", "ignore"]);
    git(&dir, &["config", "fsck.gitattributesLineLength", "ignore"]);
    check(&["fsck"]);
}
