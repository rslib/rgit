//! `rgit settings`: each known key with its value and source, and the
//! warnings for settings files that cannot be used.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-settings-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A repo with one commit; the global config path points into the same
/// scratch folder at a file that does not exist unless a test writes it.
fn repo(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
        &["commit", "-q", "--allow-empty", "-m", "base"],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    dir
}

fn global_path(dir: &Path) -> PathBuf {
    dir.join(".git").join("global-config.toml")
}

fn rgit(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    Command::new(RGIT)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("RGIT_OPLOG", "0")
        .env("RGIT_CONFIG", global_path(dir))
        .env_remove("RGIT_OUTPUT")
        .env_remove("CLAUDECODE")
        .env_remove("AI_AGENT")
        .envs(env.iter().copied())
        .args(args)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn private_file(dir: &Path, text: &str) -> PathBuf {
    let folder = dir.join(".git").join("rgit");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("config.toml");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn settings_shows_value_and_private_file_source() {
    let dir = repo("private");
    let path = private_file(&dir, "[output]\ncompact = true\n");
    let got = rgit(&dir, &[], &["settings", "output.compact"]);
    assert!(got.status.success(), "{got:?}");
    let text = out(&got);
    assert!(text.contains("true"), "{text}");
    assert!(text.contains(&path.display().to_string()), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_unset_key_shows_default() {
    let dir = repo("default");
    let got = rgit(&dir, &[], &["settings", "commit.change_id"]);
    assert!(got.status.success(), "{got:?}");
    assert_eq!(out(&got).trim(), "commit.change_id = false  (default)");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_unknown_key_exits_1() {
    let dir = repo("unknown");
    let got = rgit(&dir, &[], &["settings", "output.nope"]);
    assert_eq!(got.status.code(), Some(1), "{got:?}");
    let text = format!("{}{}", out(&got), err(&got));
    assert!(text.contains("output.nope"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_lists_every_concrete_key() {
    let dir = repo("list");
    let got = rgit(&dir, &[], &["settings"]);
    assert!(got.status.success(), "{got:?}");
    let text = out(&got);
    for key in [
        "output.format",
        "output.compact",
        "commit.change_id",
        "workflow.preset",
    ] {
        assert!(text.contains(key), "{key}: {text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_env_source_is_named() {
    let dir = repo("env");
    let got = rgit(
        &dir,
        &[("RGIT_OUTPUT", "json")],
        &["--human", "settings", "output.format"],
    );
    assert!(got.status.success(), "{got:?}");
    assert_eq!(out(&got).trim(), "output.format = json  (RGIT_OUTPUT)");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_bad_file_warns_and_runs() {
    let dir = repo("bad");
    let global = global_path(&dir);
    std::fs::write(&global, "output = [").unwrap();
    let got = rgit(&dir, &[], &["settings", "output.compact"]);
    assert_eq!(got.status.code(), Some(0), "{got:?}");
    let stderr = err(&got);
    let warnings: Vec<&str> = stderr.lines().filter(|l| l.contains("warning")).collect();
    assert_eq!(warnings.len(), 1, "{stderr}");
    assert!(
        warnings[0].contains(&global.display().to_string()),
        "{stderr}"
    );
    assert!(warnings[0].contains("line 1"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_unknown_key_in_file_warns() {
    let dir = repo("file-key");
    let path = private_file(&dir, "[output]\ncompcat = true\n");
    let got = rgit(&dir, &[], &["settings", "output.compact"]);
    assert_eq!(got.status.code(), Some(0), "{got:?}");
    let stderr = err(&got);
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(stderr.contains("output.compcat"), "{stderr}");
    assert_eq!(out(&got).trim(), "output.compact = false  (default)");
    let _ = std::fs::remove_dir_all(&dir);
}
