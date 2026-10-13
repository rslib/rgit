//! The hints agents read: the session-start home view and the Agent Skill say
//! TOON is the default under an agent harness.

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

/// A repo with one commit and one untracked file.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-hints-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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
    let out = Command::new("git")
        .args(["add", "a.txt"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = Command::new("git")
        .args(["commit", "-qm", "one"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    dir
}

#[test]
fn toon_default_hint() {
    let dir = repo("hint");

    // The session-start view an agent sees says TOON is already the default.
    let hook = rgit(&dir, &[("AI_AGENT", "pi")], &["hook", "session-start"]);
    assert!(hook.status.success(), "{hook:?}");
    assert!(
        stdout(&hook).contains("TOON by default"),
        "{}",
        stdout(&hook)
    );

    // The skill says the same, and no longer tells the agent to add a flag.
    let skill = rgit(&dir, &[("AI_AGENT", "pi")], &["--human", "agent", "skill"]);
    assert!(skill.status.success(), "{skill:?}");
    let text = stdout(&skill);
    assert!(text.contains("TOON by default"), "{text}");
    assert!(!text.contains("Always pass `--toon`"), "{text}");

    // The command reference points at the same default.
    let reference = rgit(&dir, &[], &["--human", "agent", "skill", "--reference"]);
    assert!(reference.status.success(), "{reference:?}");
    assert!(
        stdout(&reference).contains("TOON is already the default"),
        "{}",
        stdout(&reference)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hints_carry_a_flag_only_when_the_default_differs() {
    let dir = repo("hint-flag");
    let marked = [("AI_AGENT", "pi")];

    let got = stdout(&rgit(&dir, &marked, &["status"]));
    assert!(got.contains("Run `rgit stage <path>`"), "{got}");
    assert!(!got.contains("--toon"), "{got}");

    let got = stdout(&rgit(&dir, &marked, &["--toon", "status"]));
    assert!(!got.contains("--toon"), "{got}");

    let got = stdout(&rgit(&dir, &[], &["--toon", "status"]));
    assert!(got.contains("Run `rgit --toon stage <path>`"), "{got}");

    let got = stdout(&rgit(&dir, &marked, &["--json", "status"]));
    assert!(got.contains("rgit --json stage <path>"), "{got}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_long_list_of_choices_points_at_help() {
    let dir = repo("long-list");
    let marked = [("AI_AGENT", "pi")];

    let got = stdout(&rgit(&dir, &marked, &["nosuch"]));
    assert!(got.contains("Run `rgit --help` for its "), "{got}");
    assert!(!got.contains("valid subcommands"), "{got}");

    let got = stdout(&rgit(&dir, &marked, &["log", "--bogus"]));
    assert!(got.contains("Run `rgit log --help` for its "), "{got}");

    let got = stdout(&rgit(&dir, &marked, &["stash", "bogus"]));
    assert!(
        got.contains("valid flags for `rgit stash`: --message"),
        "{got}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
