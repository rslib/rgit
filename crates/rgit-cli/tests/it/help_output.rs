//! Help output. In the TOON and JSON modes `-h`, `--help` and `rgit help
//! <cmd>` print a command's help as a value. A bare command group prints its
//! full help on stdout and succeeds.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

/// Every agent marker these tests may inherit, so each case controls them.
const MARKERS: [&str; 9] = [
    "AI_AGENT",
    "CLAUDECODE",
    "CODEX_SANDBOX",
    "CODEX_THREAD_ID",
    "GEMINI_CLI",
    "CURSOR_AGENT",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "AGENT",
];

fn base(cmd: &mut Command, dir: &Path) {
    cmd.current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .env("RGIT_OPLOG", "0");
    for marker in MARKERS {
        cmd.env_remove(marker);
    }
    cmd.env_remove("RGIT_OUTPUT");
}

fn rgit(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(RGIT);
    base(&mut cmd, dir);
    cmd.envs(env.iter().copied()).args(args).output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A shape with no repo needed: help prints before discovery.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-help-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A repo with one commit and one modified tracked file.
fn repo(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
    ] {
        let out = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    for args in [&["add", "a.txt"][..], &["commit", "-qm", "one"]] {
        let out = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
    dir
}

#[test]
fn structured_help() {
    let dir = scratch("structured");

    // Top-level help as TOON: a value, not clap's prose.
    let top = rgit(&dir, &[], &["--help", "--toon"]);
    assert!(top.status.success(), "{:?}", top);
    let text = stdout(&top);
    assert!(!text.contains("Usage:"), "{text}");
    assert!(text.contains("subcommands["), "{text}");
    assert!(text.contains("\n  status,"), "{text}");
    assert!(text.contains("\n  forge,"), "{text}");

    // Nested help as JSON.
    let nested = rgit(&dir, &[], &["--json", "forge", "pr", "--help"]);
    assert!(nested.status.success(), "{:?}", nested);
    let value: serde_json::Value = serde_json::from_str(&stdout(&nested)).expect("json envelope");
    let names: Vec<&str> = value["subcommands"]
        .as_array()
        .expect("subcommands list")
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"view"), "{names:?}");
    assert!(names.contains(&"create"), "{names:?}");

    // Short help under the marker default is TOON, with the local flags.
    let short = rgit(&dir, &[("AI_AGENT", "pi")], &["status", "-h"]);
    assert!(short.status.success(), "{:?}", short);
    let text = stdout(&short);
    assert!(text.contains("flags["), "{text}");
    assert!(text.contains("--porcelain"), "{text}");

    // `rgit help <cmd>` matches `<cmd> --help`.
    let through = rgit(&dir, &[], &["--toon", "help", "forge"]);
    let direct = rgit(&dir, &[], &["--toon", "forge", "--help"]);
    assert!(through.status.success(), "{:?}", through);
    assert_eq!(stdout(&through), stdout(&direct));

    // A leaf command has no subcommands and at least one flag.
    let leaf = rgit(&dir, &[], &["--toon", "rev-parse", "--help"]);
    assert!(leaf.status.success(), "{:?}", leaf);
    let text = stdout(&leaf);
    assert!(text.contains("0 subcommands"), "{text}");
    assert!(text.contains("flags["), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bare_group() {
    let dir = scratch("bare");

    // A bare rgit-only group prints its full help and succeeds.
    let pr = rgit(&dir, &[], &["--human", "forge", "pr"]);
    assert_eq!(pr.status.code(), Some(0), "{:?}", pr);
    assert_eq!(stderr(&pr), "");
    let text = stdout(&pr);
    assert!(text.contains("Commands:"), "{text}");
    assert!(text.contains("view"), "{text}");
    assert!(text.contains("create"), "{text}");

    // A bare group git also has.
    let bisect = rgit(&dir, &[], &["--human", "bisect"]);
    assert_eq!(bisect.status.code(), Some(0), "{:?}", bisect);
    assert_eq!(stderr(&bisect), "");
    let text = stdout(&bisect);
    assert!(text.contains("Commands:"), "{text}");
    assert!(text.contains("start"), "{text}");
    assert!(text.contains("bad"), "{text}");

    // A bare group under TOON matches its --help.
    let bare = rgit(&dir, &[], &["--toon", "forge"]);
    let help = rgit(&dir, &[], &["--toon", "forge", "--help"]);
    assert_eq!(bare.status.code(), Some(0), "{:?}", bare);
    assert_eq!(stdout(&bare), stdout(&help));

    // An unknown subcommand still fails.
    let bogus = rgit(&dir, &[], &["forge", "pr", "bogus"]);
    assert_ne!(bogus.status.code(), Some(0), "{:?}", bogus);

    // A group with a default action is unchanged: `stash` stashes.
    let repo = repo("bare-stash");
    let stashed = rgit(&repo, &[], &["--human", "stash"]);
    assert!(stashed.status.success(), "{:?}", stashed);
    assert!(
        !stdout(&stashed).contains("Commands:"),
        "{}",
        stdout(&stashed)
    );
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn missing_argument_names() {
    let dir = scratch("missing-arg");

    // TOON: the missing positional is named, with the usage and the hint.
    let toon = rgit(&dir, &[], &["--toon", "branch", "create"]);
    assert_eq!(toon.status.code(), Some(129), "{:?}", toon);
    let text = stdout(&toon);
    assert!(text.contains("missing required argument: <NAME>"), "{text}");
    assert!(text.contains("usage: rgit branch create"), "{text}");
    assert!(
        text.contains("Run `rgit --toon branch create --help` for details"),
        "{text}"
    );

    // JSON: a missing required flag is named in the error field.
    let json_out = rgit(&dir, &[], &["--json", "reword"]);
    assert_eq!(json_out.status.code(), Some(129), "{:?}", json_out);
    let value: serde_json::Value = serde_json::from_str(&stdout(&json_out)).expect("json envelope");
    assert_eq!(
        value["error"],
        "missing required argument: --message <MESSAGE>"
    );
    let help: Vec<&str> = value["help"]
        .as_array()
        .expect("help list")
        .iter()
        .map(|h| h.as_str().unwrap())
        .collect();
    assert!(
        help.iter().any(|h| h.starts_with("usage: rgit reword")),
        "{help:?}"
    );
    assert!(
        help.contains(&"Run `rgit --json reword --help` for details"),
        "{help:?}"
    );

    // Human: git's message keeps each name on its own line and exits 129.
    let human = rgit(&dir, &[], &["--human", "remote", "add"]);
    assert_eq!(human.status.code(), Some(129), "{:?}", human);
    let err = stderr(&human);
    assert!(
        err.starts_with("error: the following required arguments were not provided:\n"),
        "{err}"
    );
    assert!(err.contains("\n    <NAME>\n"), "{err}");
    assert!(err.contains("\n    <URL>\n"), "{err}");
    assert!(err.contains("usage: rgit remote add"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn help_builtin_structured() {
    let dir = scratch("help-builtin");

    // `rgit help` in TOON is the same value as `rgit -h`.
    let help = rgit(&dir, &[], &["--toon", "help"]);
    assert_eq!(help.status.code(), Some(0), "{:?}", help);
    let text = stdout(&help);
    assert!(text.contains("subcommands["), "{text}");
    assert!(text.contains("\n  status,"), "{text}");
    let direct = rgit(&dir, &[], &["--toon", "-h"]);
    assert_eq!(stdout(&help), stdout(&direct));

    // `rgit help -a` as JSON parses and lists the subcommands.
    let all = rgit(&dir, &[], &["--json", "help", "-a"]);
    assert_eq!(all.status.code(), Some(0), "{:?}", all);
    let value: serde_json::Value = serde_json::from_str(&stdout(&all)).expect("json envelope");
    let subs = value["subcommands"].as_array().expect("subcommands list");
    assert!(subs.iter().any(|s| s["name"] == "commit"), "{subs:?}");

    // Human output is unchanged.
    let human = rgit(&dir, &[], &["help"]);
    assert_eq!(human.status.code(), Some(0), "{:?}", human);
    assert!(
        stdout(&human).starts_with("See 'rgit help <command>'"),
        "{}",
        stdout(&human)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn help_builtin_nested() {
    let dir = scratch("help-nested");
    let repo = repo("help-nested-stash");

    // `rgit help <group> <command>` is `<group> <command> --help` in TOON.
    let through = rgit(&dir, &[], &["--toon", "help", "forge", "pr"]);
    let direct = rgit(&dir, &[], &["--toon", "forge", "pr", "--help"]);
    assert_eq!(through.status.code(), Some(0), "{:?}", through);
    assert_eq!(stdout(&through), stdout(&direct));

    // The same rewrite keeps human output byte for byte.
    let nested = rgit(&repo, &[], &["help", "stash", "push"]);
    let stash = rgit(&repo, &[], &["stash", "push", "--help"]);
    assert_eq!(nested.status.code(), Some(0), "{:?}", nested);
    assert_eq!(stdout(&nested), stdout(&stash));

    // A name that does not resolve fails like an unknown command.
    let bogus = rgit(&dir, &[], &["help", "forge", "nope"]);
    assert_eq!(bogus.status.code(), Some(1), "{:?}", bogus);
    assert!(
        stderr(&bogus).contains("'forge nope' is not a git command"),
        "{}",
        stderr(&bogus)
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&repo);
}
