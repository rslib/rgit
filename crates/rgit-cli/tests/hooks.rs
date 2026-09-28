//! Hooks run as git runs them: reference-transaction around ref updates,
//! post-index-change, pre-auto-gc, `hook run` and core.hooksPath.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DATE: &str = "2020-01-01T00:00:00Z";

fn run(bin: &str, dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let out = run("git", dir, args);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    out
}

fn rgit(dir: &Path, args: &[&str]) -> Output {
    run(env!("CARGO_BIN_EXE_rgit"), dir, args)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A repo on `main` with `a` committed, at a fixed date so twins match.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-hooks-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    git(&dir, &["add", "a"]);
    git(&dir, &["commit", "-qm", "a"]);
    dir
}

fn hook(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(".git/hooks").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The `prepared`/`committed` updates a reference-transaction log shows, one
/// sorted set per state, without git's own AUTO_MERGE bookkeeping.
fn updates(log: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut state = "";
    for line in log.lines() {
        match line.strip_prefix("== ") {
            Some(s) => state = if s == "aborted" { "" } else { s },
            None if !state.is_empty() && !line.ends_with(" AUTO_MERGE") => {
                out.push(format!("{state} {line}"));
            }
            None => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn reference_transaction_sees_the_updates_git_reports() {
    let steps: &[&[&str]] = &[
        &["branch", "b"],
        &["update-ref", "refs/x", "HEAD"],
        &["tag", "t"],
        &["checkout", "-q", "-b", "c"],
        &["commit", "-q", "--allow-empty", "-m", "b"],
        &["reset", "-q", "--hard", "main"],
    ];
    let mut logs = Vec::new();
    for bin in ["git", env!("CARGO_BIN_EXE_rgit")] {
        let dir = repo(if bin == "git" { "rt-git" } else { "rt-rgit" });
        let log = dir.join("rt.log");
        hook(
            &dir,
            "reference-transaction",
            &format!("echo \"== $1\" >> {0}; cat >> {0}", log.display()),
        );
        let mut per_step = Vec::new();
        for args in steps {
            let _ = std::fs::remove_file(&log);
            let out = run(bin, &dir, args);
            assert!(out.status.success(), "{bin} {args:?}: {out:?}");
            let seen = updates(&std::fs::read_to_string(&log).unwrap_or_default());
            // git's reset also moves ORIG_HEAD, which rgit's does not keep.
            per_step.push(
                seen.into_iter()
                    .filter(|l| !l.ends_with(" ORIG_HEAD"))
                    .collect::<Vec<_>>(),
            );
        }
        logs.push(per_step);
    }
    for (i, args) in steps.iter().enumerate() {
        assert_eq!(logs[1][i], logs[0][i], "{args:?}");
    }
}

#[test]
fn a_refusing_prepared_hook_aborts_the_update() {
    let dir = repo("abort");
    let log = dir.join("rt.log");
    hook(
        &dir,
        "reference-transaction",
        &format!("echo \"$1\" >> {}; test \"$1\" != prepared", log.display()),
    );
    let out = rgit(&dir, &["--human", "branch", "b"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert!(
        text(&out.stderr).contains("ref updates aborted by hook"),
        "{out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "prepared\naborted\n"
    );
    let refs = text(&git(&dir, &["for-each-ref", "--format=%(refname)"]).stdout);
    assert_eq!(refs, "refs/heads/main\n");
}

#[test]
fn hook_run_matches_git() {
    let dir = repo("run");
    hook(&dir, "foo", "echo \"args:$*\"; cat; exit 3");
    std::fs::write(dir.join("in.txt"), "stdin-data\n").unwrap();
    for args in [
        &["hook", "run", "nope"][..],
        &["hook", "run", "--ignore-missing", "nope"],
        &["hook", "run", "--to-stdin=in.txt", "foo", "--", "x", "y"],
        &["hook", "run", "foo"],
    ] {
        let want = git_status(&dir, args);
        let mut with_human = vec!["--human"];
        with_human.extend_from_slice(args);
        let got = rgit(&dir, &with_human);
        assert_eq!(got.status.code(), want.status.code(), "{args:?}");
        assert_eq!(text(&got.stdout), text(&want.stdout), "{args:?}");
        let (g, w) = (text(&got.stderr), text(&want.stderr));
        assert_eq!(
            g.trim_start_matches("rgit: ").trim_start_matches("error: "),
            w.trim_start_matches("error: "),
            "{args:?}"
        );
    }
}

fn git_status(dir: &Path, args: &[&str]) -> Output {
    run("git", dir, args)
}

#[test]
fn hooks_path_is_honoured_and_output_streams() {
    let dir = repo("path");
    hook(
        &dir,
        "../../my-hooks/pre-commit",
        "echo from-pre-commit; exit 1",
    );
    git(&dir, &["config", "core.hooksPath", "my-hooks"]);
    std::fs::write(dir.join("a"), "b\n").unwrap();
    git(&dir, &["add", "a"]);
    let out = rgit(&dir, &["--human", "commit", "-m", "x"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(text(&out.stdout), "");
    assert_eq!(text(&out.stderr), "from-pre-commit\n");
}

#[test]
fn post_index_change_gets_gits_flags() {
    let dir = repo("pic");
    let log = dir.join("pic.log");
    hook(
        &dir,
        "post-index-change",
        &format!("echo \"$*\" >> {}", log.display()),
    );
    std::fs::write(dir.join("a"), "b\n").unwrap();
    assert!(rgit(&dir, &["add", "a"]).status.success());
    assert!(rgit(&dir, &["reset", "-q", "--hard"]).status.success());
    assert!(rgit(&dir, &["status"]).status.success());
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "0 0\n1 0\n");
}

#[test]
fn pre_auto_gc_can_stop_auto_gc() {
    for bin in ["git", env!("CARGO_BIN_EXE_rgit")] {
        let dir = repo(if bin == "git" { "gc-git" } else { "gc-rgit" });
        let log = dir.join("gc.log");
        hook(
            &dir,
            "pre-auto-gc",
            &format!("echo ran >> {}; exit 1", log.display()),
        );
        git(&dir, &["config", "gc.auto", "1"]);
        // Enough loose objects that git's sample folder, objects/17, has two.
        for i in 0..600 {
            std::fs::write(dir.join(format!("f{i}")), format!("{i}\n")).unwrap();
        }
        git(&dir, &["add", "."]);
        assert!(run(bin, &dir, &["gc", "--auto"]).status.success());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "ran\n", "{bin}");
        let packs = dir.join(".git/objects/pack").read_dir().unwrap().count();
        assert_eq!(packs, 0, "{bin}");
    }
}
