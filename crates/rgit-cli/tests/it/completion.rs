//! `rgit completion <shell>` and the candidates rgit gives on Tab. The engine
//! runs as the generated fish script runs it: `RGIT_COMPLETE=fish rgit -- <words>`,
//! one candidate per line, `value<TAB>help`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const AGENT_MARKERS: &[&str] = &[
    "AI_AGENT",
    "AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "GEMINI_CLI",
    "CURSOR_AGENT",
    "OPENCODE",
    "PI_CODING_AGENT",
    "RGIT_OUTPUT",
    "RGIT_COMPLETE",
];

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-completion-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn clean_env(cmd: &mut Command, dir: &Path) {
    cmd.current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir());
    for marker in AGENT_MARKERS {
        cmd.env_remove(marker);
    }
}

fn rgit_command(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    clean_env(&mut cmd, dir);
    cmd
}

fn rgit(dir: &Path, args: &[&str]) -> Output {
    rgit_command(dir).args(args).output().unwrap()
}

fn script(tag: &str, args: &[&str]) -> String {
    let dir = scratch(tag);
    let out = rgit(&dir, args);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(out.stdout).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn repo(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    dir
}

/// The candidate values for the command line `words`, as the fish script asks.
fn candidates(dir: &Path, words: &[&str]) -> Vec<String> {
    let out = rgit_command(dir)
        .env("RGIT_COMPLETE", "fish")
        .arg("--")
        .arg("rgit")
        .args(words)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.split('\t').next().unwrap_or_default().to_owned())
        .filter(|v| !v.starts_with('-'))
        .collect()
}

#[test]
fn script_zsh_calls_rgit() {
    let zsh = script("script-zsh", &["completion", "zsh"]);
    assert!(
        zsh.starts_with("#compdef rgit"),
        "{}",
        &zsh[..zsh.len().min(80)]
    );
    assert!(zsh.contains("RGIT_COMPLETE=\"zsh\""), "{zsh}");
    assert!(zsh.contains("rgit -- "), "{zsh}");
}

#[test]
fn script_toon_flag_leaves_the_script_raw() {
    let fish = script("script-toon", &["--toon", "completion", "fish"]);
    assert!(fish.starts_with("complete "), "{fish}");
    assert!(fish.contains("RGIT_COMPLETE=fish"), "{fish}");
}

#[test]
fn script_every_shell_prints_a_script() {
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let text = script(&format!("script-all-{shell}"), &["completion", shell]);
        assert!(text.contains("RGIT_COMPLETE"), "{shell}: {text}");
    }
}

#[test]
fn script_matches_the_env_registration() {
    let dir = scratch("script-env");
    let out = rgit_command(&dir)
        .env("RGIT_COMPLETE", "bash")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        script("script-env-cmd", &["completion", "bash"])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn script_bash_completes_a_subcommand() {
    if Command::new("bash").arg("--version").output().is_err() {
        eprintln!("skipped: bash is not installed");
        return;
    }
    let dir = scratch("script-bash");
    let bin = Path::new(env!("CARGO_BIN_EXE_rgit")).parent().unwrap();
    let driver = "eval \"$(rgit completion bash)\"\n\
        COMP_WORDS=(rgit sta)\nCOMP_CWORD=1\nCOMP_TYPE=9\n\
        _clap_complete_rgit rgit sta rgit\n\
        printf '%s\\n' \"${COMPREPLY[@]}\"\n";
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut cmd = Command::new("bash");
    cmd.args(["--norc", "--noprofile", "-c", driver])
        .env("PATH", path);
    clean_env(&mut cmd, &dir);
    let out = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let words: Vec<&str> = text.lines().collect();
    assert!(
        words.contains(&"status") && words.contains(&"stash"),
        "stdout: {text}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn script_unknown_shell_is_a_usage_error() {
    let dir = scratch("script-nope");
    let out = rgit(&dir, &["completion", "nope"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        assert!(err.contains(shell), "stderr misses {shell}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_switch_offers_branches() {
    let dir = repo("values-switch");
    git(&dir, &["branch", "feature"]);
    assert_eq!(candidates(&dir, &["switch", "fe"]), ["feature"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_push_offers_remotes() {
    let dir = repo("values-push");
    git(&dir, &["remote", "add", "origin", "/nowhere"]);
    assert!(candidates(&dir, &["push", "o"]).contains(&"origin".to_owned()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_stage_offers_changed_files() {
    let dir = repo("values-stage");
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "second"]);
    std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
    std::fs::write(dir.join("c.txt"), "c\n").unwrap();
    let found = candidates(&dir, &["stage", ""]);
    assert!(found.contains(&"a.txt".to_owned()), "{found:?}");
    assert!(found.contains(&"c.txt".to_owned()), "{found:?}");
    assert!(!found.contains(&"b.txt".to_owned()), "{found:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_stash_pop_offers_stash_entries() {
    let dir = repo("values-stash");
    std::fs::write(dir.join("b.txt"), "changed\n").unwrap();
    git(&dir, &["stash", "-q"]);
    assert!(candidates(&dir, &["stash", "pop", ""]).contains(&"stash@{0}".to_owned()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_outside_a_repository_give_nothing() {
    let dir = scratch("values-outside");
    assert!(candidates(&dir, &["switch", ""]).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_tag_delete_offers_tags() {
    let dir = repo("values-tag");
    git(&dir, &["tag", "v1"]);
    assert!(candidates(&dir, &["tag", "-d", "v"]).contains(&"v1".to_owned()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_remote_remove_offers_remotes() {
    let dir = repo("values-remote");
    git(&dir, &["remote", "add", "origin", "/nowhere"]);
    assert!(candidates(&dir, &["remote", "remove", "o"]).contains(&"origin".to_owned()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_blame_offers_files() {
    let dir = repo("values-blame");
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "second"]);
    assert!(candidates(&dir, &["blame", "a"]).contains(&"a.txt".to_owned()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_bisect_good_offers_revisions() {
    let dir = repo("values-bisect");
    git(&dir, &["branch", "feature"]);
    assert!(candidates(&dir, &["bisect", "good", "fe"]).contains(&"feature".to_owned()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_stage_from_a_subfolder_gives_paths_relative_to_it() {
    let dir = repo("values-subdir");
    let sub = dir.join("crates");
    std::fs::create_dir_all(sub.join("src")).unwrap();
    std::fs::write(sub.join("src/code.rs"), "x\n").unwrap();
    std::fs::write(dir.join("top.txt"), "t\n").unwrap();
    let all = candidates(&sub, &["stage", ""]);
    assert!(all.contains(&"src/code.rs".to_owned()), "{all:?}");
    assert!(!all.iter().any(|c| c.contains("top.txt")), "{all:?}");
    assert_eq!(candidates(&sub, &["stage", "src/co"]), ["src/code.rs"]);
    let up = candidates(&sub, &["stage", "../t"]);
    assert!(up.contains(&"../top.txt".to_owned()), "{up:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_numbers_offer_nothing() {
    let dir = repo("values-number");
    git(&dir, &["branch", "feature"]);
    for words in [
        &["log", "-n", ""][..],
        &["log", "--max-count", ""],
        &["cherry-pick", "-m", ""],
        &["revert", "--mainline", ""],
    ] {
        let found = candidates(&dir, words);
        assert!(found.is_empty(), "{words:?}: {found:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_set_branches_offers_revisions_after_the_remote() {
    let dir = repo("values-set-branches");
    git(&dir, &["remote", "add", "origin", "/nowhere"]);
    git(&dir, &["branch", "feature"]);
    let found = candidates(&dir, &["remote", "set-branches", "origin", "fe"]);
    assert_eq!(found, ["feature"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_tag_names_follow_the_flag() {
    let dir = repo("values-tag-names");
    git(&dir, &["tag", "v1"]);
    git(&dir, &["branch", "feature"]);
    assert!(candidates(&dir, &["tag", "v9", "fe"]).contains(&"feature".to_owned()));
    assert!(!candidates(&dir, &["tag", "-d", "v1", "fe"]).contains(&"feature".to_owned()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn values_branch_delete_offers_local_branches_once() {
    let dir = repo("values-branch-delete");
    git(&dir, &["branch", "feature"]);
    git(&dir, &["tag", "fe-tag"]);
    assert_eq!(candidates(&dir, &["branch", "delete", "fe"]), ["feature"]);
    assert_eq!(candidates(&dir, &["branch", "-d", "fe"]), ["feature"]);
    assert_eq!(
        candidates(&dir, &["branch", "--contains", "fe"]),
        ["feature", "fe-tag"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn script_an_unknown_complete_value_runs_rgit_normally() {
    let dir = repo("script-bogus-env");
    let out = rgit_command(&dir)
        .env("RGIT_COMPLETE", "1")
        .args(["--human", "rev-parse", "--git-dir"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), ".git");
    let _ = std::fs::remove_dir_all(&dir);
}
