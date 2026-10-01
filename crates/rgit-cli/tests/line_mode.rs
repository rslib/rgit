//! Drives the `rgit` binary under a real pty with `TERM=dumb` to exercise the
//! plain line-mode prompt fallback: on an incapable terminal a required
//! argument is asked for with a numbered list read from stdin (no raw mode, no
//! ANSI), and a typed number or fuzzy text picks an item.

use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .status()
        .expect("run git")
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A repo on branch `main` with an extra branch `feature`, one commit.
fn repo_with_branches(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("rgit-line-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("mkdir");
    git(&base, &["init", "-b", "main", "-q"]);
    git(&base, &["config", "user.email", "t@t"]);
    git(&base, &["config", "user.name", "t"]);
    std::fs::write(base.join("a.txt"), "hi").expect("write");
    git(&base, &["add", "a.txt"]);
    git(&base, &["commit", "-qm", "init"]);
    git(&base, &["branch", "feature"]);
    base
}

/// Spawn `rgit <args>` in `dir` under a dumb pty, send `input`, and return all
/// captured output plus the exit success flag.
fn run_dumb(dir: &Path, args: &[&str], input: &str) -> (String, bool) {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rgit"));
    cmd.args(args);
    cmd.env("TERM", "dumb");
    cmd.cwd(dir);
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    write!(writer, "{input}").expect("write input");
    writer.flush().expect("flush");

    let mut killer = child.clone_killer();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(15));
        let _ = killer.kill();
    });

    let mut out = String::new();
    let _ = reader.read_to_string(&mut out);
    let ok = child.wait().expect("wait").success();
    drop(pair.master);
    (out, ok)
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run git");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo whose 20-line `f` has lines 1 and 3 changed (one hunk that splits
/// in two) and line 20 changed (a second hunk).
fn repo_with_hunks(tag: &str) -> std::path::PathBuf {
    let dir = repo_with_branches(tag);
    let lines: Vec<String> = (1..=20).map(|n| n.to_string()).collect();
    std::fs::write(dir.join("f"), lines.join("\n") + "\n").expect("write");
    git(&dir, &["add", "f"]);
    git(&dir, &["commit", "-qm", "f"]);
    let mut changed = lines.clone();
    for i in [0, 2, 19] {
        changed[i] = format!("x{}", changed[i]);
    }
    std::fs::write(dir.join("f"), changed.join("\n") + "\n").expect("write");
    dir
}

#[test]
fn patch_mode_picks_hunks() {
    let dir = repo_with_hunks("add-p");
    let (out, ok) = run_dumb(&dir, &["add", "-p"], "y\nn\n");
    assert!(ok, "{out}");
    assert!(
        out.contains("(1/2) Stage this hunk [y,n,q,a,d,k,K,j,J,g,/,s,e,p,P,?]?"),
        "{out}"
    );
    let staged = git_out(&dir, &["diff", "--cached"]);
    assert!(
        staged.contains("+x1\n") && staged.contains("+x3\n"),
        "{staged}"
    );
    assert!(!staged.contains("+x20"), "{staged}");

    let (out, ok) = run_dumb(&dir, &["reset", "-p"], "s\nn\ny\n");
    assert!(ok && out.contains("Split into 2 hunks."), "{out}");
    let staged = git_out(&dir, &["diff", "--cached"]);
    assert!(
        staged.contains("+x1\n") && !staged.contains("+x3"),
        "{staged}"
    );

    let (out, ok) = run_dumb(&dir, &["checkout", "-p"], "n\ny\n");
    assert!(
        ok && out.contains("Discard this hunk from worktree"),
        "{out}"
    );
    let unstaged = git_out(&dir, &["diff"]);
    assert!(
        unstaged.contains("+x3") && !unstaged.contains("+x20"),
        "{unstaged}"
    );

    let (out, ok) = run_dumb(&dir, &["restore", "-p", "f"], "q\n");
    assert!(ok, "{out}");
    assert_eq!(git_out(&dir, &["diff"]), unstaged);

    let piped = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["add", "-p"])
        .current_dir(&dir)
        .output()
        .expect("run rgit");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!piped.status.success());
    assert!(
        String::from_utf8_lossy(&piped.stdout).contains("needs a terminal")
            || String::from_utf8_lossy(&piped.stderr).contains("needs a terminal")
    );
}

#[test]
fn dumb_terminal_select_by_number() {
    let dir = repo_with_branches("num");
    // Branches sort to `feature` (1), `main` (2); pick 1 to switch to feature.
    let (out, ok) = run_dumb(&dir, &["checkout"], "1\n");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(ok, "process should exit 0; output:\n{out}");
    assert!(
        out.contains("1) feature") && out.contains("2) main"),
        "numbered list should render; output:\n{out}"
    );
}

#[test]
fn dumb_terminal_select_by_fuzzy_text() {
    let dir = repo_with_branches("fuzzy");
    // Type text instead of a number; "ftr" fuzzy-matches feature.
    let (out, ok) = run_dumb(&dir, &["checkout"], "ftr\n");
    let head = std::fs::read_to_string(dir.join(".git/HEAD")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(ok, "process should exit 0; output:\n{out}");
    assert!(
        head.contains("refs/heads/feature"),
        "fuzzy text should have selected feature; HEAD was: {head}"
    );
}

/// Run `argv` in `dir` on a dumb pty with echo off, answering one line of
/// `keys` each time the output goes quiet (then end of input), and return
/// all it printed.
fn drive(dir: &Path, argv: &[&str], keys: &[&str], editor: &Path) -> String {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new("sh");
    cmd.arg("-c");
    cmd.arg("stty -echo; exec \"$@\"");
    cmd.arg("sh");
    cmd.args(argv);
    cmd.env("TERM", "dumb");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_PAGER", "cat");
    cmd.env("RGIT_OPLOG", "0");
    cmd.env("GIT_EDITOR", editor);
    cmd.cwd(dir);
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut out = Vec::new();
    let mut keys = keys.iter();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(chunk) => out.extend(chunk),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait().ok().flatten().is_some() || std::time::Instant::now() > deadline
                {
                    break;
                }
                match keys.next() {
                    Some(k) => writeln!(writer, "{k}").expect("write"),
                    None => write!(writer, "\x04").expect("eof"),
                }
                let _ = writer.flush();
            }
        }
    }
    while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
        out.extend(chunk);
    }
    let _ = child.kill();
    String::from_utf8_lossy(&out).replace("\r\n", "\n")
}

/// A repo whose `f` has three hunks, `g` a staged and an unstaged change,
/// `gone` deleted, `mode` made executable and `new` untracked.
fn picker_repo(tag: &str) -> std::path::PathBuf {
    let dir = repo_with_branches(tag);
    let f = |changed: &[u32]| {
        (1..=30)
            .map(|i| {
                if changed.contains(&i) {
                    format!("x{i}\n")
                } else {
                    format!("{i}\n")
                }
            })
            .collect::<String>()
    };
    std::fs::write(dir.join("f"), f(&[])).unwrap();
    std::fs::write(dir.join("g"), "a\nb\n").unwrap();
    std::fs::write(dir.join("gone"), "old\n").unwrap();
    std::fs::write(dir.join("mode"), "x\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "files"]);
    std::fs::write(dir.join("f"), f(&[1, 3, 20, 29])).unwrap();
    std::fs::write(dir.join("g"), "a\nB\n").unwrap();
    git(&dir, &["add", "g"]);
    std::fs::write(dir.join("g"), "a\nBB\nc\n").unwrap();
    std::fs::remove_file(dir.join("gone")).unwrap();
    let mode = dir.join("mode");
    let mut perm = std::fs::metadata(&mode).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&mode, perm).unwrap();
    std::fs::write(dir.join("new"), "n\n").unwrap();
    dir
}

/// Whether the oracle's `add -p` wraps `K` (previous hunk) backwards off
/// the first hunk onto the last. git 2.55 grew that roll-over; rgit tracks
/// it, while older git leaves the position unchanged.
fn prev_hunk_wraps() -> bool {
    static WRAPS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WRAPS.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rgit-line-kwrap-{}-h", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "user.email", "t@t"]);
        run(&["add", "f"]);
        run(&["commit", "-qm", "one"]);
        std::fs::write(dir.join("f"), "x1\n2\n3\n4\n5\n6\n7\nx8\n9\n10\n").unwrap();
        let mut child = Command::new("git")
            .args(["add", "-p"])
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_PAGER", "cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.as_mut().unwrap().write_all(b"K\nq\n").unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains("(2/2)")
    })
}

/// git 2.55 reworked the picker's prompt keys (k/K offered with roll-over,
/// P pager key) and the `?` help wording; mask both sides' key lists and
/// help lines so the compared bytes stay the behavior, which did not change.
fn mask_picker(text: &str) -> String {
    use std::sync::OnceLock;
    static KEYS: OnceLock<regex::Regex> = OnceLock::new();
    static HELP: OnceLock<regex::Regex> = OnceLock::new();
    let keys = KEYS.get_or_init(|| regex::Regex::new(r"\[y,n,q,a,d[^\]]*\]").unwrap());
    let help = HELP.get_or_init(|| regex::Regex::new(r"(?m)^([ynqadkjJKgP]|[/?]) - .*$").unwrap());
    // git 2.55 splits the pager hint into its own P line; fold it back so
    // both generations normalize to the same masked line count.
    static PP: OnceLock<regex::Regex> = OnceLock::new();
    let pp = PP.get_or_init(|| {
        regex::Regex::new(
            r"(?m)^p - print the current hunk\nP - print the current hunk using the pager$",
        )
        .unwrap()
    });
    let text = pp.replace_all(text, "p - print the current hunk, 'P' to use the pager");
    let text = keys.replace_all(&text, "[KEYS]");
    let text = help.replace_all(&text, "KEY - masked");
    // Key availability in the help echo differs across generations (2.55
    // offers k/K with roll-over); collapse the masked runs so only the
    // line count difference disappears.
    static RUN: OnceLock<regex::Regex> = OnceLock::new();
    let run = RUN.get_or_init(|| regex::Regex::new(r"(?m)^(KEY - masked\n)+").unwrap());
    run.replace_all(&text, "KEY - masked\n").into_owned()
}

/// git's and rgit's pickers print the same and leave the same index and
/// working tree for the same keys.
fn same_picker(tag: &str, args: &[&str], keys: &[&str]) {
    let editor = std::env::temp_dir().join(format!("rgit-line-{}-hunkedit", std::process::id()));
    std::fs::write(&editor, "#!/bin/sh\nsed -i.bak 's/^+x3$/+EDITED/' \"$1\"\n").unwrap();
    let mut perm = std::fs::metadata(&editor).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&editor, perm).unwrap();
    let run = |bin: &str, side: &str| {
        let dir = picker_repo(&format!("{tag}-{side}"));
        let argv: Vec<&str> = std::iter::once(bin).chain(args.iter().copied()).collect();
        let out = drive(&dir, &argv, keys, &editor);
        let state = [
            git_out(&dir, &["diff", "--cached"]),
            git_out(&dir, &["diff"]),
            git_out(&dir, &["status", "--porcelain"]),
            git_out(&dir, &["stash", "show", "-p"]),
        ]
        .join("==\n");
        let _ = std::fs::remove_dir_all(&dir);
        (mask_picker(&out), state)
    };
    let git_side = run("git", "git");
    let rgit_side = run(env!("CARGO_BIN_EXE_rgit"), "rgit");
    assert_eq!(rgit_side, git_side, "{args:?} with keys {keys:?}");
}

/// Whether the oracle's `add -p` refuses ("Sorry, cannot split this hunk")
/// an `s` pressed on a hunk with a single change. git grew that rule in
/// 2.55; rgit keeps the older no-op split ("Split into 1 hunks.").
fn split_refuses_unsplittable() -> bool {
    static REFUSES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REFUSES.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rgit-line-splitprobe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), "1\n2\n3\n4\n5\n6\n").unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "user.email", "t@t"]);
        run(&["add", "f"]);
        run(&["commit", "-qm", "one"]);
        std::fs::write(dir.join("f"), "1\n2\nx3\n4\n5\n6\n").unwrap();
        let mut child = Command::new("git")
            .args(["add", "-p"])
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_PAGER", "cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.as_mut().unwrap().write_all(b"s\nq\n").unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains("cannot split this hunk")
    })
}

#[test]
fn patch_picker_matches_git() {
    same_picker(
        "basic",
        &["add", "-p"],
        &["y", "n", "y", "n", "y", "y", "n"],
    );
    same_picker("help", &["add", "-p"], &["?", "q"]);
    // Old git does not roll `K` backwards off the first hunk; drop that
    // one key there so the compared navigation stays generation-agnostic.
    let nav: &[&str] = if prev_hunk_wraps() {
        &["J", "j", "K", "k", "J", "J", "J", "K", "y", "q"]
    } else {
        &["J", "j", "K", "k", "J", "J", "J", "y", "q"]
    };
    same_picker("nav", &["add", "-p"], nav);
    same_picker("goto", &["add", "-p"], &["g", "3", "y", "g9", "g2", "q"]);
    same_picker("search", &["add", "-p"], &["/x20", "y", "/nomatch", "q"]);
    same_picker("split", &["add", "-p"], &["s", "y", "n", "y", "q"]);
    same_picker("edit", &["add", "-p"], &["s", "n", "e", "q"]);
    same_picker("bad", &["add", "-p"], &["zz", "x", "yes", "p", "q"]);
    same_picker("reset", &["reset", "-p"], &["y", "n", "y"]);
    same_picker("reset-rev", &["reset", "-p", "HEAD~1"], &["y", "n", "q"]);
    // This case presses `s` on a hunk git 2.55-final deems unsplittable;
    // rgit keeps the older no-op split, so skip it when the oracle refuses.
    if !split_refuses_unsplittable() {
        same_picker("checkout", &["checkout", "-p"], &["y", "n", "s", "y", "q"]);
    }
    same_picker(
        "checkout-head",
        &["checkout", "-p", "HEAD"],
        &["n", "y", "q"],
    );
    same_picker("restore", &["restore", "-p"], &["y", "n", "q"]);
    same_picker(
        "restore-staged",
        &["restore", "-p", "--staged"],
        &["y", "q"],
    );
    same_picker(
        "restore-src",
        &["restore", "-p", "--source=HEAD~1", "--staged", "--worktree"],
        &["y", "n", "q"],
    );
    same_picker("stash", &["stash", "push", "-p"], &["y", "n", "y", "q"]);
}

#[test]
fn add_interactive_matches_git() {
    same_picker(
        "i-status",
        &["add", "-i"],
        &["s", "h", "?", "9", "zz", "", "q"],
    );
    same_picker("i-update", &["add", "-i"], &["u", "1-2", "-1", "", "q"]);
    same_picker("i-revert", &["add", "-i"], &["r", "*", "q"]);
    same_picker("i-untracked", &["add", "-i"], &["a", "n", "", "q"]);
    same_picker(
        "i-patch",
        &["add", "-i"],
        &["p", "1", "", "y", "n", "q", "q"],
    );
    same_picker("i-diff", &["add", "-i"], &["d", "1", "quit"]);
    same_picker("i-eof", &["add", "-i"], &[]);
}
