//! `rgit ai draft`, `review` and `prompt`, and `rgit commit --draft`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-ai-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repo with one commit (`base subject`) and a staged new file.
fn repo(tag: &str, global: &str) -> PathBuf {
    let dir = scratch(tag);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(&dir, &["add", "base.txt"]);
    git(&dir, &["commit", "-q", "-m", "base subject"]);
    std::fs::write(dir.join("new.txt"), "hello world\n").unwrap();
    git(&dir, &["add", "new.txt"]);
    std::fs::write(global_path(&dir), global).unwrap();
    dir
}

fn global_path(dir: &Path) -> PathBuf {
    dir.join(".git").join("global-config.toml")
}

fn rgit(dir: &Path, args: &[&str]) -> Output {
    Command::new(RGIT)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_PAGER", "cat")
        .env("GIT_EDITOR", "true")
        .env("RGIT_OPLOG", "0")
        .env("RGIT_CONFIG", global_path(dir))
        .env_remove("RGIT_OUTPUT")
        .env_remove("CLAUDECODE")
        .env_remove("AI_AGENT")
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

fn tool(name: &str, command: &str) -> String {
    format!("[ai.{name}]\ncommand = {command:?}\n")
}

fn with_default(command: &str, extra: &str) -> String {
    format!(
        "[commit.ai]\ndraft = \"t\"\nreview = \"t\"\n{extra}\n{}",
        tool("t", command)
    )
}

#[test]
fn draft_prints_the_tool_output() {
    let dir = repo("draft", &tool("t", "printf 'feat: add x\\n'"));
    let o = rgit(&dir, &["ai", "draft", "--tool", "t"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "feat: add x\n");
}

#[test]
fn draft_uses_the_configured_default_tool() {
    let dir = repo("default", &with_default("printf 'fix: y\\n'", ""));
    let o = rgit(&dir, &["ai", "draft"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "fix: y\n");
}

#[test]
fn no_tool_set_names_the_key() {
    let dir = repo("notool", &tool("t", "true"));
    let o = rgit(&dir, &["ai", "draft"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("commit.ai.draft"), "{}", err(&o));
}

#[test]
fn unknown_tool_lists_the_defined_tools() {
    let dir = repo(
        "unknown",
        &format!("{}{}", tool("a", "true"), tool("b", "true")),
    );
    let o = rgit(&dir, &["ai", "draft", "--tool", "zzz"]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("zzz") && e.contains("a, b"), "{e}");
}

#[test]
fn nothing_staged_exits_1_without_running_the_tool() {
    let dir = repo("empty", "");
    let marker = dir.join("ran");
    std::fs::write(
        global_path(&dir),
        tool("t", &format!("touch {}", marker.display())),
    )
    .unwrap();
    git(&dir, &["reset", "-q", "new.txt"]);
    let o = rgit(&dir, &["ai", "draft", "--tool", "t"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("nothing staged"), "{}", err(&o));
    assert!(!marker.exists());
}

#[test]
fn failing_tool_exits_1_with_stderr_and_name() {
    let dir = repo("fail", &tool("t", "echo boom >&2; exit 3"));
    let o = rgit(&dir, &["ai", "draft", "--tool", "t"]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("boom") && e.contains("t "), "{e}");
}

#[test]
fn slow_tool_times_out() {
    let dir = repo(
        "timeout",
        "[ai.t]\ncommand = \"sleep 5\"\ntimeout_secs = 1\n",
    );
    let start = Instant::now();
    let o = rgit(&dir, &["ai", "draft", "--tool", "t"]);
    assert!(start.elapsed() < Duration::from_secs(4));
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("timed out after 1 seconds"), "{}", err(&o));
}

#[test]
fn review_prints_the_tool_output() {
    let dir = repo(
        "review",
        &with_default("printf 'new.txt: looks fine\\n'", ""),
    );
    let o = rgit(&dir, &["ai", "review"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "new.txt: looks fine\n");
}

#[test]
fn prompt_draft_shows_rules_and_placeholders() {
    let dir = repo("prompt", "");
    let o = rgit(&dir, &["ai", "prompt", "draft"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("Conventional Commits"), "{text}");
    assert!(
        text.contains("{{diff}}") && text.contains("{{log}}"),
        "{text}"
    );
    assert!(!text.contains("{{rules}}"));
}

#[test]
fn rules_file_replaces_the_rules() {
    let dir = repo("rules", "");
    std::fs::write(dir.join(".git").join("rules.md"), "MY RULES").unwrap();
    std::fs::write(
        global_path(&dir),
        with_default("cat \"$RGIT_PROMPT_FILE\"", "rules_file = \"rules.md\""),
    )
    .unwrap();
    let o = rgit(&dir, &["ai", "draft"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("MY RULES"), "{text}");
    assert!(text.contains("base subject"), "{text}");
    assert!(text.contains("+hello world"), "{text}");
    assert!(!text.contains("Conventional Commits"), "{text}");
}

#[test]
fn unknown_placeholder_exits_1_naming_it() {
    let dir = repo(
        "placeholder",
        &with_default("cat", "draft_prompt = \"x {{nope}}\""),
    );
    let o = rgit(&dir, &["ai", "draft"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("{{nope}}"), "{}", err(&o));
}

#[test]
fn long_diff_is_cut() {
    let dir = repo(
        "cut",
        &with_default("cat", "max_diff_bytes = 100\ndraft_prompt = \"{{diff}}\""),
    );
    let o = rgit(&dir, &["ai", "draft"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("[diff cut at 100 bytes; the stat above shows the full scope]"));
}

#[test]
fn tool_prompt_beats_task_prompt() {
    let dir = repo(
        "toolprompt",
        "[commit.ai]\ndraft = \"t\"\ndraft_prompt = \"task\"\n[ai.t]\ncommand = \"cat\"\ndraft_prompt = \"tool\"\n",
    );
    let o = rgit(&dir, &["ai", "draft"]);
    assert_eq!(out(&o), "tool\n");
}

#[test]
fn prompt_file_is_relative_to_its_config_file() {
    let dir = repo("relative", "");
    std::fs::write(dir.join(".git").join("d.md"), "from file").unwrap();
    std::fs::write(
        global_path(&dir),
        with_default("cat", "draft_prompt_file = \"d.md\""),
    )
    .unwrap();
    let o = rgit(&dir, &["ai", "draft"]);
    assert_eq!(out(&o), "from file\n", "{}", err(&o));
}

#[test]
fn private_prompt_overrides_global_prompt_file() {
    let dir = repo("private", "");
    std::fs::write(dir.join(".git").join("d.md"), "from file").unwrap();
    std::fs::write(
        global_path(&dir),
        with_default("cat", "draft_prompt_file = \"d.md\""),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join(".git/rgit")).unwrap();
    std::fs::write(
        dir.join(".git/rgit/config.toml"),
        "[commit.ai]\ndraft_prompt = \"private\"\n",
    )
    .unwrap();
    let o = rgit(&dir, &["ai", "draft"]);
    assert_eq!(out(&o), "private\n", "{}", err(&o));
    assert!(err(&o).is_empty(), "{}", err(&o));
}

#[test]
fn both_forms_in_one_file_warn() {
    let dir = repo(
        "both",
        &with_default(
            "cat",
            "draft_prompt = \"inline\"\ndraft_prompt_file = \"d.md\"",
        ),
    );
    let o = rgit(&dir, &["ai", "draft"]);
    assert_eq!(out(&o), "inline\n");
    let e = err(&o);
    assert!(
        e.contains("warning") && e.contains("commit.ai.draft_prompt_file"),
        "{e}"
    );
}

#[test]
fn shared_command_is_ignored_with_a_warning() {
    let dir = repo("shared", "");
    std::fs::create_dir_all(dir.join(".rgit")).unwrap();
    std::fs::write(
        dir.join(".rgit/config.toml"),
        "[ai.x]\ncommand = \"touch pwned\"\n",
    )
    .unwrap();
    let o = rgit(&dir, &["ai", "draft", "--tool", "x"]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(
        e.contains("ai.x.command") && e.contains("config.toml"),
        "{e}"
    );
    assert!(!dir.join("pwned").exists());
}

#[test]
fn draft_fields_in_toon_and_json() {
    let dir = repo("fields", &with_default("printf 'feat: add x\\n'", ""));
    let toon = out(&rgit(&dir, &["--toon", "ai", "draft"]));
    assert!(
        toon.contains("message") && toon.contains("feat: add x"),
        "{toon}"
    );
    assert!(toon.contains("tool"), "{toon}");
    let json = out(&rgit(&dir, &["--json", "ai", "draft"]));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["message"], "feat: add x");
    assert_eq!(value["tool"], "t");
}

fn head_message(dir: &Path) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "-1", "--format=%B"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    out(&o).trim().to_owned()
}

#[test]
fn commit_draft_no_edit_commits_the_message() {
    let dir = repo("commit", &with_default("printf 'feat: add x\\n'", ""));
    let o = rgit(&dir, &["commit", "--draft", "--no-edit"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(head_message(&dir), "feat: add x");
}

#[test]
fn commit_draft_commits_cleaned_output() {
    let dir = repo(
        "clean",
        &with_default("printf '\\n```\\nfix: y\\n```\\n'", ""),
    );
    let o = rgit(&dir, &["commit", "--draft=t", "--no-edit"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(head_message(&dir), "fix: y");
}

#[test]
fn commit_draft_error_aborts_before_committing() {
    let dir = repo("abort", &tool("t", "exit 3"));
    let o = rgit(&dir, &["commit", "--draft=t", "--no-edit"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(head_message(&dir), "base subject");
}

#[test]
fn commit_draft_conflicts_with_message() {
    let dir = repo("conflict", &with_default("true", ""));
    let o = rgit(&dir, &["commit", "--draft", "-m", "x"]);
    assert_eq!(o.status.code(), Some(128), "{}", err(&o));
}

fn tracked_change(dir: &Path) {
    git(dir, &["commit", "-q", "-m", "add new"]);
    std::fs::write(dir.join("new.txt"), "changed line\n").unwrap();
}

#[test]
fn commit_draft_all_uses_tracked_changes() {
    let dir = repo(
        "all",
        &with_default("cat", "draft_prompt = \"{{files}}|{{diff}}\""),
    );
    tracked_change(&dir);
    let o = rgit(&dir, &["commit", "-a", "--draft", "--no-edit"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let message = head_message(&dir);
    assert!(
        message.contains("new.txt") && message.contains("+changed line"),
        "{message}"
    );
    let shown = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["show", "--stat", "--format=", "HEAD"])
        .output()
        .unwrap();
    assert!(out(&shown).contains("new.txt"));
}

#[test]
fn commit_draft_paths_use_only_those_paths() {
    let dir = repo(
        "paths",
        &with_default("cat", "draft_prompt = \"{{files}}\""),
    );
    tracked_change(&dir);
    std::fs::write(dir.join("base.txt"), "other\n").unwrap();
    git(&dir, &["add", "base.txt"]);
    let o = rgit(&dir, &["commit", "--draft", "--no-edit", "new.txt"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(head_message(&dir), "new.txt");
}

#[test]
fn commit_draft_all_failure_keeps_index() {
    let dir = repo("allfail", &tool("t", "exit 3"));
    tracked_change(&dir);
    let o = rgit(&dir, &["commit", "-a", "--draft=t", "--no-edit"]);
    assert_eq!(o.status.code(), Some(1));
    let cached = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["diff", "--cached"])
        .output()
        .unwrap();
    assert!(out(&cached).is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.join("new.txt")).unwrap(),
        "changed line\n"
    );
}

#[test]
fn commit_draft_paths_work_from_a_subfolder() {
    let dir = repo(
        "subdir",
        &with_default("cat", "draft_prompt = \"{{files}}\""),
    );
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/file.txt"), "one\n").unwrap();
    git(&dir, &["add", "sub/file.txt"]);
    git(&dir, &["commit", "-q", "-m", "add sub"]);
    std::fs::write(dir.join("sub/file.txt"), "two\n").unwrap();
    let o = rgit(
        &dir,
        &["-C", "sub", "commit", "file.txt", "--draft", "--no-edit"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(head_message(&dir), "sub/file.txt");
}

#[test]
fn shared_rules_file_inside_the_repo_lands_in_the_prompt() {
    let dir = repo("sharedrules", &tool("t", "cat \"$RGIT_PROMPT_FILE\""));
    std::fs::create_dir_all(dir.join(".rgit/prompts")).unwrap();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(dir.join(".rgit/prompts/rules.md"), "FOLDER RULES").unwrap();
    std::fs::write(dir.join("docs/commit-rules.md"), "TEAM RULES").unwrap();
    for (path, expected) in [
        ("prompts/rules.md", "FOLDER RULES"),
        ("../docs/commit-rules.md", "TEAM RULES"),
    ] {
        std::fs::write(
            dir.join(".rgit/config.toml"),
            format!("[commit.ai]\nrules_file = {path:?}\n"),
        )
        .unwrap();
        let o = rgit(&dir, &["ai", "draft", "--tool", "t"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert!(out(&o).contains(expected), "{}", out(&o));
        assert!(err(&o).is_empty(), "{}", err(&o));
    }
}

#[test]
fn shared_rules_file_outside_the_repo_is_ignored() {
    let dir = repo("sharedoutside", &tool("t", "cat \"$RGIT_PROMPT_FILE\""));
    std::fs::create_dir_all(dir.join(".rgit")).unwrap();
    std::fs::write(
        dir.join(".rgit/config.toml"),
        "[commit.ai]\nrules_file = \"/etc/hosts\"\n",
    )
    .unwrap();
    let o = rgit(&dir, &["ai", "draft", "--tool", "t"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("commit.ai.rules_file"), "{}", err(&o));
    assert!(out(&o).contains("Conventional Commits"));
}

#[test]
fn commit_draft_amend_uses_the_whole_commit() {
    let dir = repo(
        "amend",
        &with_default("cat", "draft_prompt = \"{{files}}\""),
    );
    git(&dir, &["commit", "-q", "-m", "add new"]);
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    let o = rgit(&dir, &["commit", "--amend", "--draft", "--no-edit"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let message = head_message(&dir);
    assert!(
        message.contains("new.txt") && message.contains("b.txt"),
        "{message}"
    );
}
