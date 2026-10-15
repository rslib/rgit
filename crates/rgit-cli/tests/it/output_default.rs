//! The default output mode. An agent harness marker picks TOON; the output
//! flag, `RGIT_OUTPUT` and the `output.format` setting override it. git's own
//! machine formats (`-z`, `--porcelain`, `--format`, `--pretty`) keep git's
//! bytes under the marker default unless a flag asks for TOON or JSON.

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

/// A clean environment: no marker, no `RGIT_OUTPUT`, no user config.
fn base(cmd: &mut Command, dir: &Path) {
    cmd.current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .env("RGIT_OPLOG", "0")
        .env(
            "RGIT_CONFIG",
            std::env::temp_dir().join("rgit-default-no-config.toml"),
        );
    for marker in MARKERS {
        cmd.env_remove(marker);
    }
    cmd.env_remove("RGIT_OUTPUT");
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    base(&mut cmd, dir);
    cmd.args(args).output().unwrap()
}

/// rgit with extra environment set after the clean base.
fn rgit(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(RGIT);
    base(&mut cmd, dir);
    cmd.envs(env.iter().copied()).args(args).output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// A repo with one commit and one untracked `a.txt`.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-default-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
    ] {
        let out = git(&dir, args);
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    let out = git(&dir, &["add", "a.txt"]);
    assert!(out.status.success());
    let out = git(&dir, &["commit", "-qm", "one"]);
    assert!(out.status.success());
    dir
}

#[test]
fn default_output() {
    let dir = repo("default");
    let want = out(&git(&dir, &["rev-parse", "HEAD"]));

    // An agent marker picks TOON.
    for marker in ["AI_AGENT", "CLAUDECODE", "GEMINI_CLI", "OPENCODE_CLIENT"] {
        let marked = rgit(&dir, &[(marker, "1")], &["rev-parse", "HEAD"]);
        assert!(marked.status.success(), "{marker}: {marked:?}");
        assert!(
            out(&marked).contains("result:"),
            "{marker}: {}",
            out(&marked)
        );
    }
    // `AGENT` marks only goose and amp.
    for value in ["goose", "amp"] {
        let marked = rgit(&dir, &[("AGENT", value)], &["rev-parse", "HEAD"]);
        assert!(
            out(&marked).contains("result:"),
            "AGENT={value}: {}",
            out(&marked)
        );
    }
    for value in ["jenkins", "github-actions"] {
        let out_ = rgit(&dir, &[("AGENT", value)], &["rev-parse", "HEAD"]);
        assert_eq!(out(&out_), want, "AGENT={value}");
    }
    // `CODEX_HOME` is not a marker.
    let codex = rgit(
        &dir,
        &[("CODEX_HOME", "/tmp/codex")],
        &["rev-parse", "HEAD"],
    );
    assert_eq!(out(&codex), want);

    // No marker, no RGIT_OUTPUT and no output.format: git's bytes.
    let plain = rgit(&dir, &[], &["rev-parse", "HEAD"]);
    assert_eq!(out(&plain), want);

    // The output flag wins over the marker.
    let human = rgit(
        &dir,
        &[("CLAUDECODE", "1")],
        &["--human", "rev-parse", "HEAD"],
    );
    assert_eq!(out(&human), want);

    // The environment wins over the marker.
    let env = rgit(
        &dir,
        &[("CLAUDECODE", "1"), ("RGIT_OUTPUT", "human")],
        &["rev-parse", "HEAD"],
    );
    assert_eq!(out(&env), want);
    // ...and picks the mode on its own.
    let toon = rgit(&dir, &[("RGIT_OUTPUT", "toon")], &["rev-parse", "HEAD"]);
    assert!(out(&toon).contains("result:"));
    let json = rgit(&dir, &[("RGIT_OUTPUT", "json")], &["rev-parse", "HEAD"]);
    assert!(out(&json).contains("\"result\""), "{}", out(&json));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Write `<repo>/.git/rgit/config.toml`.
fn private_config(dir: &Path, text: &str) -> PathBuf {
    let folder = dir.join(".git").join("rgit");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("config.toml");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn toml_private_file_output_format_beats_marker() {
    let dir = repo("toml-private");
    let want = out(&git(&dir, &["rev-parse", "HEAD"]));
    private_config(&dir, "[output]\nformat = \"human\"\n");
    let got = rgit(&dir, &[("CLAUDECODE", "1")], &["rev-parse", "HEAD"]);
    assert_eq!(out(&got), want);
    private_config(&dir, "[output]\nformat = \"json\"\n");
    let got = rgit(&dir, &[("CLAUDECODE", "1")], &["rev-parse", "HEAD"]);
    assert!(out(&got).contains("\"result\""), "{}", out(&got));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toml_rgit_output_env_beats_file() {
    let dir = repo("toml-env");
    let want = out(&git(&dir, &["rev-parse", "HEAD"]));
    private_config(&dir, "[output]\nformat = \"json\"\n");
    let got = rgit(&dir, &[("RGIT_OUTPUT", "human")], &["rev-parse", "HEAD"]);
    assert_eq!(out(&got), want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toml_git_config_rgit_output_is_ignored() {
    let dir = repo("toml-gitconfig");
    let want = out(&git(&dir, &["rev-parse", "HEAD"]));
    let set = git(&dir, &["config", "rgit.output", "json"]);
    assert!(set.status.success());
    let got = rgit(&dir, &[], &["rev-parse", "HEAD"]);
    assert_eq!(out(&got), want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toml_compact_from_private_file() {
    let dir = repo("toml-compact");
    let compact = out(&rgit(&dir, &[], &["log", "--compact"]));
    assert_ne!(out(&rgit(&dir, &[], &["log"])), compact);
    private_config(&dir, "[output]\ncompact = true\n");
    assert_eq!(out(&rgit(&dir, &[], &["log"])), compact);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toml_bad_file_warns_and_runs() {
    let dir = repo("toml-bad");
    let path = private_config(&dir, "output = [");
    let got = rgit(&dir, &[], &["status"]);
    assert_eq!(got.status.code(), Some(0), "{got:?}");
    let stderr = String::from_utf8_lossy(&got.stderr).into_owned();
    let warnings: Vec<&str> = stderr.lines().filter(|l| l.contains("warning")).collect();
    assert_eq!(warnings.len(), 1, "{stderr}");
    assert!(
        warnings[0].contains(&path.display().to_string()),
        "{stderr}"
    );
    assert!(warnings[0].contains("line 1"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn machine_format() {
    let dir = repo("machine");
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    let marked = [("AI_AGENT", "pi")];

    // Porcelain status under the default keeps git's bytes.
    let want = out(&git(&dir, &["status", "--porcelain"]));
    assert!(want.contains("?? b.txt"), "{want:?}");
    let got = rgit(&dir, &marked, &["status", "--porcelain"]);
    assert_eq!(out(&got), want);
    assert_eq!(got.status.code(), Some(0));

    // A custom log format keeps git's bytes.
    let want = out(&git(&dir, &["log", "-1", "--format=%H %s"]));
    let got = rgit(&dir, &marked, &["log", "-1", "--format=%H %s"]);
    assert_eq!(out(&got), want);
    let want = out(&git(&dir, &["log", "-1", "--pretty=oneline"]));
    let got = rgit(&dir, &marked, &["log", "-1", "--pretty=oneline"]);
    assert_eq!(out(&got), want);

    // -z keeps git's NUL-terminated bytes.
    let want = out(&git(&dir, &["status", "-z"]));
    let got = rgit(&dir, &marked, &["status", "-z"]);
    assert_eq!(out(&got), want);

    // -z inside a short-flag cluster, and its long spelling --null.
    let want = out(&git(&dir, &["status", "-sz"]));
    assert_eq!(out(&rgit(&dir, &marked, &["status", "-sz"])), want);
    let want = out(&git(&dir, &["ls-files", "-oz"]));
    assert_eq!(out(&rgit(&dir, &marked, &["ls-files", "-oz"])), want);
    let want = out(&git(&dir, &["status", "--porcelain", "--null"]));
    assert_eq!(
        out(&rgit(&dir, &marked, &["status", "--porcelain", "--null"])),
        want
    );

    // A format word after `--` is a path, not a format.
    let got = rgit(&dir, &marked, &["log", "-1", "--", "-z"]);
    assert!(out(&got).starts_with("commits:"), "{}", out(&got));

    // The git passthrough keeps git's bytes and reads stdin.
    let mut cmd = Command::new(RGIT);
    base(&mut cmd, &dir);
    let mut child = cmd
        .envs(marked)
        .args(["git", "hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"hi\n").unwrap();
    }
    let got = child.wait_with_output().unwrap();
    assert_eq!(out(&got), "45b983be36b73c0788dc9cbcb76cbb80fc7bb057\n");

    // An explicit --toon wins over the format flag.
    let toon = rgit(&dir, &marked, &["--toon", "status", "--porcelain"]);
    assert!(toon.status.success());
    assert!(out(&toon).contains("lines["), "{}", out(&toon));
    assert!(out(&toon).contains("?? b.txt"), "{}", out(&toon));
    let json = rgit(&dir, &marked, &["--json", "status", "--porcelain"]);
    assert!(out(&json).contains("\"lines\""), "{}", out(&json));

    let _ = std::fs::remove_dir_all(&dir);
}
