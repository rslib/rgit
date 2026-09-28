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
        out.contains("(1/2) Stage this hunk [y,n,q,a,d,s,?]?"),
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
