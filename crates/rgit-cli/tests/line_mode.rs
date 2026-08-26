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
