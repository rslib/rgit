use std::path::PathBuf;
use std::time::{Duration, Instant};

use rgit_ai::{Error, Inputs, Task, builtin_rules, prompt, run, tool_for};
use rgit_config::{Files, Settings};
use tempfile::TempDir;

fn settings(dir: &TempDir, global: &str) -> Settings {
    let path = dir.path().join("g.toml");
    std::fs::write(&path, global).unwrap();
    Settings::load_with(
        Files {
            global: Some(path),
            ..Files::default()
        },
        &|_| None,
    )
}

fn inputs(dir: &TempDir) -> Inputs {
    Inputs {
        diff: "diff --git a/f b/f\n+hello\n".to_owned(),
        stat: " f | 1 +".to_owned(),
        files: "f".to_owned(),
        log: "feat: older".to_owned(),
        branch: "topic".to_owned(),
        repo: dir.path().to_owned(),
    }
}

fn tool(command: &str) -> String {
    format!("[ai.t]\ncommand = {command:?}\n")
}

#[test]
fn render_fills_each_placeholder() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        "[commit.ai]\nrules = \"R\"\ndraft_prompt = \"{{diff}}|{{stat}}|{{files}}|{{log}}|{{branch}}|{{rules}}\"\n",
    );
    let text = prompt(&s, Task::Draft, "t", &inputs(&dir)).unwrap();
    assert_eq!(
        text,
        "diff --git a/f b/f\n+hello\n| f | 1 +|f|feat: older|topic|R"
    );
}

#[test]
fn unknown_placeholder_errors() {
    let dir = TempDir::new().unwrap();
    let s = settings(&dir, "[commit.ai]\ndraft_prompt = \"a {{nope}}\"\n");
    let error = prompt(&s, Task::Draft, "t", &inputs(&dir)).unwrap_err();
    assert!(matches!(&error, Error::Placeholder(n) if n == "nope"));
    assert!(error.to_string().contains("nope"));
}

#[test]
fn rules_with_braces_are_inserted_literally() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        "[commit.ai]\nrules = \"use {{x}} and {{diff}}\"\ndraft_prompt = \"{{rules}}\"\n",
    );
    let text = prompt(&s, Task::Draft, "t", &inputs(&dir)).unwrap();
    assert_eq!(text, "use {{x}} and {{diff}}");
}

#[test]
fn diff_is_cut_at_max_diff_bytes() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        "[commit.ai]\nmax_diff_bytes = 20\ndraft_prompt = \"{{diff}}\"\n",
    );
    let mut input = inputs(&dir);
    input.diff = "line one\nline two\nline three\nline four\n".to_owned();
    let text = prompt(&s, Task::Draft, "t", &input).unwrap();
    assert_eq!(
        text,
        "line one\nline two\n[diff cut at 20 bytes; the stat above shows the full scope]"
    );
}

#[test]
fn diff_cut_respects_char_boundaries() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        "[commit.ai]\nmax_diff_bytes = 4\ndraft_prompt = \"{{diff}}\"\n",
    );
    let mut input = inputs(&dir);
    input.diff = "aa\u{e9}\u{e9}\u{e9}".to_owned();
    let text = prompt(&s, Task::Draft, "t", &input).unwrap();
    assert!(text.starts_with("aa\u{e9}\n[diff cut at 4 bytes"));
}

#[test]
fn default_prompt_has_conventional_commits_and_diff() {
    let dir = TempDir::new().unwrap();
    let s = settings(&dir, &tool("cat"));
    let text = prompt(&s, Task::Draft, "t", &inputs(&dir)).unwrap();
    assert!(text.contains("Conventional Commits"));
    assert!(text.contains("+hello"));
    assert!(text.contains("feat: older"));
}

#[test]
fn rules_file_replaces_only_the_rules() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("rules.md"), "MY RULES {{x}}").unwrap();
    let s = settings(&dir, "[commit.ai]\nrules_file = \"rules.md\"\n");
    let text = prompt(&s, Task::Draft, "t", &inputs(&dir)).unwrap();
    assert!(text.starts_with("MY RULES {{x}}"));
    assert!(text.contains("feat: older"));
    assert!(text.contains("+hello"));
    assert!(!text.contains(builtin_rules().trim()));
}

#[test]
fn tool_prompt_beats_task_prompt_beats_builtin() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        "[commit.ai]\ndraft_prompt = \"task\"\n[ai.t]\ncommand = \"cat\"\ndraft_prompt = \"tool\"\n",
    );
    assert_eq!(prompt(&s, Task::Draft, "t", &inputs(&dir)).unwrap(), "tool");
    assert_eq!(
        prompt(&s, Task::Draft, "other", &inputs(&dir)).unwrap(),
        "task"
    );
    let s = settings(&dir, &tool("cat"));
    let review = prompt(&s, Task::Review, "t", &inputs(&dir)).unwrap();
    assert!(review.contains("senior engineer"));
}

#[test]
fn prompt_file_is_relative_to_its_config_file() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/d.md"), "from file {{branch}}").unwrap();
    let s = settings(&dir, "[commit.ai]\ndraft_prompt_file = \"sub/d.md\"\n");
    let mut input = inputs(&dir);
    input.repo = PathBuf::from("/elsewhere");
    assert_eq!(
        prompt(&s, Task::Draft, "t", &input).unwrap(),
        "from file topic"
    );
}

#[test]
fn missing_prompt_file_names_the_path() {
    let dir = TempDir::new().unwrap();
    let s = settings(&dir, "[commit.ai]\ndraft_prompt_file = \"gone.md\"\n");
    let error = prompt(&s, Task::Draft, "t", &inputs(&dir)).unwrap_err();
    assert!(matches!(error, Error::PromptFile { .. }));
    assert!(error.to_string().contains("gone.md"));
}

#[test]
fn tool_for_default_unset_and_unknown() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        "[commit.ai]\ndraft = \"b\"\n[ai.a]\ncommand = \"x\"\n[ai.b]\ncommand = \"y\"\n",
    );
    assert_eq!(tool_for(&s, Task::Draft, None).unwrap(), "b");
    assert_eq!(tool_for(&s, Task::Draft, Some("a")).unwrap(), "a");
    let error = tool_for(&s, Task::Review, None).unwrap_err();
    assert!(error.to_string().contains("commit.ai.review"));
    let error = tool_for(&s, Task::Draft, Some("zzz")).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("zzz") && message.contains("a, b"),
        "{message}"
    );
}

#[test]
fn run_returns_the_prompt_from_the_prompt_file() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        &format!(
            "[commit.ai]\ndraft_prompt = \"P {{{{branch}}}}\"\n{}",
            tool("cat \"$RGIT_PROMPT_FILE\"")
        ),
    );
    assert_eq!(run(&s, Task::Draft, "t", &inputs(&dir)).unwrap(), "P topic");
}

#[test]
fn run_reads_stdin() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        &format!("[commit.ai]\ndraft_prompt = \"via stdin\"\n{}", tool("cat")),
    );
    assert_eq!(
        run(&s, Task::Draft, "t", &inputs(&dir)).unwrap(),
        "via stdin"
    );
}

#[test]
fn run_sets_env_and_cwd_and_diff_file() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        &tool(
            "printf '%s|%s|%s|' \"$RGIT_TASK\" \"$RGIT_BRANCH\" \"$RGIT_REPO\"; pwd -P; cat \"$RGIT_DIFF_FILE\"",
        ),
    );
    let out = run(&s, Task::Review, "t", &inputs(&dir)).unwrap();
    let repo = dir.path().to_str().unwrap();
    assert!(out.starts_with(&format!("review|topic|{repo}|")), "{out}");
    assert!(out.contains("+hello"));
    let real = std::fs::canonicalize(dir.path()).unwrap();
    assert!(out.contains(real.to_str().unwrap()), "{out}");
}

#[test]
fn run_removes_its_temp_files() {
    let dir = TempDir::new().unwrap();
    let s = settings(&dir, &tool("printf '%s' \"$RGIT_PROMPT_FILE\""));
    let path = run(&s, Task::Draft, "t", &inputs(&dir)).unwrap();
    assert!(!std::path::Path::new(&path).exists());
}

#[test]
fn failing_tool_reports_stderr_and_name() {
    let dir = TempDir::new().unwrap();
    let s = settings(&dir, &tool("echo boom >&2; exit 3"));
    let error = run(&s, Task::Draft, "t", &inputs(&dir)).unwrap_err();
    let message = error.to_string();
    assert!(matches!(error, Error::Failed { code: Some(3), .. }));
    assert!(
        message.contains("boom") && message.contains(" t "),
        "{message}"
    );
}

#[test]
fn slow_tool_times_out() {
    let dir = TempDir::new().unwrap();
    let s = settings(&dir, "[ai.t]\ncommand = \"sleep 5\"\ntimeout_secs = 1\n");
    let start = Instant::now();
    let error = run(&s, Task::Draft, "t", &inputs(&dir)).unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(matches!(&error, Error::Timeout { secs: 1, .. }));
    assert!(error.to_string().contains("t timed out after 1 seconds"));
}

#[test]
fn draft_output_is_cleaned() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        &tool("printf '\\n\\n```text\\nfeat: x   \\n\\nbody\\n```\\n\\n'"),
    );
    assert_eq!(
        run(&s, Task::Draft, "t", &inputs(&dir)).unwrap(),
        "feat: x\n\nbody"
    );
}

#[test]
fn review_output_only_trims_blank_edges() {
    let dir = TempDir::new().unwrap();
    let s = settings(&dir, &tool("printf '\\n```\\nkeep  \\n\\n'"));
    assert_eq!(
        run(&s, Task::Review, "t", &inputs(&dir)).unwrap(),
        "```\nkeep  "
    );
}

#[test]
fn nothing_staged_does_not_run_the_tool() {
    let dir = TempDir::new().unwrap();
    let marker = dir.path().join("ran");
    let s = settings(&dir, &tool(&format!("touch {}", marker.display())));
    let mut input = inputs(&dir);
    input.diff = String::new();
    let error = run(&s, Task::Draft, "t", &input).unwrap_err();
    assert!(matches!(error, Error::NothingStaged));
    assert_eq!(error.to_string(), "nothing staged");
    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn timeout_kills_the_whole_process_group() {
    let dir = TempDir::new().unwrap();
    let pidfile = dir.path().join("pid");
    let command = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
    let s = settings(
        &dir,
        &format!("[ai.t]\ncommand = {command:?}\ntimeout_secs = 1\n"),
    );
    let error = run(&s, Task::Draft, "t", &inputs(&dir)).unwrap_err();
    assert!(matches!(error, Error::Timeout { .. }));
    let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(), "grandchild {pid} is still running");
}

#[cfg(unix)]
fn pid_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[cfg(unix)]
#[test]
fn background_child_does_not_hold_the_result() {
    let dir = TempDir::new().unwrap();
    let pidfile = dir.path().join("pid");
    let command = format!(
        "sleep 30 & echo $! > {}; printf 'feat: x\\n'",
        pidfile.display()
    );
    let s = settings(&dir, &tool(&command));
    let start = Instant::now();
    let out = run(&s, Task::Draft, "t", &inputs(&dir)).unwrap();
    assert_eq!(out, "feat: x");
    assert!(start.elapsed() < Duration::from_secs(5));
    let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
    let deadline = Instant::now() + Duration::from_secs(2);
    while pid_alive(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!pid_alive(&pid), "background child {pid} is still running");
}

#[cfg(unix)]
#[test]
fn temp_files_are_private() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        &tool("ls -l \"$RGIT_PROMPT_FILE\" \"$RGIT_DIFF_FILE\""),
    );
    let out = run(&s, Task::Draft, "t", &inputs(&dir)).unwrap();
    assert_eq!(out.matches("-rw-------").count(), 2, "{out}");
}

#[test]
fn cancel_flag_stops_the_tool_and_removes_temp_files() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = TempDir::new().unwrap();
    let pathfile = dir.path().join("path");
    let command = format!(
        "echo \"$RGIT_PROMPT_FILE\" > {}; sleep 30",
        pathfile.display()
    );
    let s = settings(&dir, &tool(&command));
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        flag.store(true, Ordering::SeqCst);
    });
    let start = Instant::now();
    let error = rgit_ai::run_with_cancel(&s, Task::Draft, "t", &inputs(&dir), &cancel).unwrap_err();
    assert!(matches!(error, Error::Interrupted));
    assert!(start.elapsed() < Duration::from_secs(5));
    let path = std::fs::read_to_string(&pathfile).unwrap();
    assert!(!std::path::Path::new(path.trim()).exists());
}

#[test]
fn review_does_not_read_a_missing_rules_file() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        &format!(
            "[commit.ai]\nrules_file = \"gone.md\"\n{}",
            tool("printf ok")
        ),
    );
    assert_eq!(run(&s, Task::Review, "t", &inputs(&dir)).unwrap(), "ok");
    assert!(prompt(&s, Task::Draft, "t", &inputs(&dir)).is_err());
}

#[test]
fn draft_keeps_inner_fenced_blocks() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        &tool("printf 'feat: x\\n\\nrun:\\n```sh\\nmake\\n```\\n'"),
    );
    assert_eq!(
        run(&s, Task::Draft, "t", &inputs(&dir)).unwrap(),
        "feat: x\n\nrun:\n```sh\nmake\n```"
    );
}

#[test]
fn draft_strips_a_fence_around_the_whole_message() {
    let dir = TempDir::new().unwrap();
    let s = settings(
        &dir,
        &tool("printf '```\\nfeat: x\\n\\n```sh\\nmake\\n```\\n```\\n'"),
    );
    assert_eq!(
        run(&s, Task::Draft, "t", &inputs(&dir)).unwrap(),
        "feat: x\n\n```sh\nmake\n```"
    );
}
