use std::path::{Path, PathBuf};

use rgit_config::{Files, Settings, Source, WarningKind};
use tempfile::TempDir;

fn write(dir: &TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn load_env(
    global: Option<&Path>,
    shared: Option<&Path>,
    private: Option<&Path>,
    home: Option<&str>,
) -> Settings {
    let files = Files {
        global: global.map(Path::to_owned),
        shared: shared.map(Path::to_owned),
        private: private.map(Path::to_owned),
    };
    let home = home.map(str::to_owned);
    Settings::load_with(files, &move |name| {
        (name == "HOME").then(|| home.clone()).flatten()
    })
}

fn load(global: Option<&Path>, shared: Option<&Path>, private: Option<&Path>) -> Settings {
    load_env(global, shared, private, None)
}

fn text(settings: &Settings, key: &str) -> Option<String> {
    let mut value = settings.table().get(key.split('.').next()?)?;
    for segment in key.split('.').skip(1) {
        value = value.as_table()?.get(segment)?;
    }
    value.as_str().map(str::to_owned)
}

#[test]
fn ai_keys_are_known() {
    let dir = TempDir::new().unwrap();
    let g = write(
        &dir,
        "g.toml",
        r#"
[ai.claude]
command = "claude -p"
timeout_secs = 60
draft_prompt = "a"
review_prompt_file = "b"

[ai.codex]
draft_prompt_file = "c"
review_prompt = "d"

[commit]
change_id = true
gpg_sign = false
auto_restack = true

[commit.ai]
draft = "claude"
review = "codex"
draft_prompt = "e"
review_prompt = "f"
rules = "g"
max_diff_bytes = 5
"#,
    );
    let p = write(
        &dir,
        "p.toml",
        "[commit.ai]\ndraft_prompt_file = \"x\"\nreview_prompt_file = \"y\"\nrules_file = \"z\"\n",
    );
    let settings = load(Some(&g), None, Some(&p));
    assert!(settings.warnings().is_empty(), "{:?}", settings.warnings());
}

#[test]
fn ai_shared_command_is_ignored_with_a_warning() {
    let dir = TempDir::new().unwrap();
    let s = write(&dir, "s.toml", "[ai.claude]\ncommand = \"rm -rf ~\"\n");
    let settings = load(None, Some(&s), None);
    assert_eq!(text(&settings, "ai.claude.command"), None);
    assert_eq!(settings.warnings().len(), 1);
    let warning = &settings.warnings()[0];
    assert_eq!(
        warning.kind,
        WarningKind::NotShared("ai.claude.command".to_owned())
    );
    let message = warning.to_string();
    assert!(message.contains("s.toml") && message.contains("ai.claude.command"));
}

#[test]
fn ai_shared_prompt_is_allowed() {
    let dir = TempDir::new().unwrap();
    let s = write(
        &dir,
        "s.toml",
        "[ai.claude]\ndraft_prompt = \"a\"\n[commit.ai]\ndraft = \"claude\"\nrules = \"r\"\nreview_prompt_file = \"f\"\n",
    );
    let settings = load(None, Some(&s), None);
    assert!(settings.warnings().is_empty());
    assert_eq!(
        text(&settings, "commit.ai.draft").as_deref(),
        Some("claude")
    );
    assert_eq!(text(&settings, "commit.ai.rules").as_deref(), Some("r"));
}

#[test]
fn ai_later_layer_prompt_replaces_file() {
    let dir = TempDir::new().unwrap();
    let g = write(
        &dir,
        "g.toml",
        "[commit.ai]\ndraft_prompt_file = \"p.md\"\n",
    );
    let p = write(
        &dir,
        "p.toml",
        "[commit.ai]\ndraft_prompt = \"P {{diff}}\"\n",
    );
    let settings = load(Some(&g), None, Some(&p));
    assert!(settings.warnings().is_empty());
    assert_eq!(
        text(&settings, "commit.ai.draft_prompt").as_deref(),
        Some("P {{diff}}")
    );
    assert_eq!(text(&settings, "commit.ai.draft_prompt_file"), None);
    assert_eq!(
        settings.source("commit.ai.draft_prompt_file"),
        Some(Source::Default)
    );
}

#[test]
fn ai_later_layer_file_replaces_prompt() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[commit.ai]\ndraft_prompt = \"inline\"\n");
    let p = write(
        &dir,
        "p.toml",
        "[commit.ai]\ndraft_prompt_file = \"p.md\"\n",
    );
    let settings = load(Some(&g), None, Some(&p));
    assert!(settings.warnings().is_empty());
    assert_eq!(text(&settings, "commit.ai.draft_prompt"), None);
    assert_eq!(
        text(&settings, "commit.ai.draft_prompt_file").as_deref(),
        Some("p.md")
    );
}

#[test]
fn ai_both_forms_in_one_file_warn_and_keep_inline() {
    let dir = TempDir::new().unwrap();
    let p = write(
        &dir,
        "p.toml",
        "[commit.ai]\ndraft_prompt = \"inline\"\ndraft_prompt_file = \"p.md\"\n",
    );
    let settings = load(None, None, Some(&p));
    assert_eq!(settings.warnings().len(), 1);
    let message = settings.warnings()[0].to_string();
    assert!(message.contains("commit.ai.draft_prompt "));
    assert!(message.contains("commit.ai.draft_prompt_file"));
    assert!(message.contains("p.toml"));
    assert_eq!(
        text(&settings, "commit.ai.draft_prompt").as_deref(),
        Some("inline")
    );
    assert_eq!(text(&settings, "commit.ai.draft_prompt_file"), None);
}

#[test]
fn ai_rules_pair() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[commit.ai]\nrules = \"inline\"\n");
    let p = write(&dir, "p.toml", "[commit.ai]\nrules_file = \"r.md\"\n");
    let settings = load(Some(&g), None, Some(&p));
    assert!(settings.warnings().is_empty());
    assert_eq!(text(&settings, "commit.ai.rules"), None);
    assert_eq!(
        text(&settings, "commit.ai.rules_file").as_deref(),
        Some("r.md")
    );
}

#[test]
fn ai_tool_prompt_pair() {
    let dir = TempDir::new().unwrap();
    let g = write(
        &dir,
        "g.toml",
        "[ai.claude]\nreview_prompt_file = \"r.md\"\ndraft_prompt = \"d\"\n",
    );
    let p = write(&dir, "p.toml", "[ai.claude]\nreview_prompt = \"inline\"\n");
    let settings = load(Some(&g), None, Some(&p));
    assert!(settings.warnings().is_empty());
    assert_eq!(
        text(&settings, "ai.claude.review_prompt").as_deref(),
        Some("inline")
    );
    assert_eq!(text(&settings, "ai.claude.review_prompt_file"), None);
    assert_eq!(
        text(&settings, "ai.claude.draft_prompt").as_deref(),
        Some("d")
    );
}

#[test]
fn ai_file_path_is_relative_to_its_config_file() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[commit.ai]\nrules_file = \"rules/r.md\"\n");
    let abs = dir.path().join("abs.md");
    let p = write(
        &dir,
        "p.toml",
        &format!(
            "[commit.ai]\ndraft_prompt_file = {:?}\n",
            abs.to_str().unwrap()
        ),
    );
    let settings = load(Some(&g), None, Some(&p));
    assert_eq!(
        settings.path("commit.ai.rules_file"),
        Some(dir.path().join("rules/r.md"))
    );
    assert_eq!(settings.path("commit.ai.draft_prompt_file"), Some(abs));
    assert_eq!(settings.path("commit.ai.review_prompt_file"), None);
}

#[test]
fn ai_file_path_expands_home() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[commit.ai]\nrules_file = \"~/r.md\"\n");
    let settings = load_env(Some(&g), None, None, Some("/fake/home"));
    assert_eq!(
        settings.path("commit.ai.rules_file"),
        Some(PathBuf::from("/fake/home/r.md"))
    );
    assert_eq!(
        text(&settings, "commit.ai.rules_file").as_deref(),
        Some("~/r.md")
    );
}

fn shared_in_repo(dir: &TempDir, text: &str) -> PathBuf {
    std::fs::create_dir_all(dir.path().join(".rgit")).unwrap();
    let path = dir.path().join(".rgit").join("config.toml");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn ai_shared_rules_file_outside_repo_is_ignored() {
    for bad in [
        "~/.ssh/id_ed25519",
        "/etc/passwd",
        "../../x",
        "a/../../../x",
    ] {
        let dir = TempDir::new().unwrap();
        let s = shared_in_repo(&dir, &format!("[commit.ai]\nrules_file = {bad:?}\n"));
        let settings = load(None, Some(&s), None);
        assert_eq!(text(&settings, "commit.ai.rules_file"), None, "{bad}");
        assert_eq!(settings.warnings().len(), 1, "{bad}");
        let message = settings.warnings()[0].to_string();
        assert!(
            message.contains("config.toml") && message.contains("commit.ai.rules_file"),
            "{message}"
        );
    }
    let dir = TempDir::new().unwrap();
    let s = shared_in_repo(&dir, "[ai.t]\ndraft_prompt_file = \"/etc/passwd\"\n");
    let settings = load(None, Some(&s), None);
    assert_eq!(settings.warnings().len(), 1);
}

#[test]
fn ai_shared_rules_file_inside_repo_is_allowed() {
    let dir = TempDir::new().unwrap();
    let s = shared_in_repo(
        &dir,
        "[commit.ai]\nrules_file = \"docs/commit-rules.md\"\ndraft_prompt_file = \"./a/../b.md\"\n",
    );
    let settings = load(None, Some(&s), None);
    assert!(settings.warnings().is_empty(), "{:?}", settings.warnings());
    assert_eq!(
        text(&settings, "commit.ai.rules_file").as_deref(),
        Some("docs/commit-rules.md")
    );
}

#[cfg(unix)]
#[test]
fn ai_shared_symlink_out_of_repo_is_ignored() {
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::fs::write(outside.path().join("secret"), "x").unwrap();
    let s = shared_in_repo(&dir, "[commit.ai]\nrules_file = \"link.md\"\n");
    std::os::unix::fs::symlink(
        outside.path().join("secret"),
        dir.path().join(".rgit/link.md"),
    )
    .unwrap();
    let settings = load(None, Some(&s), None);
    assert_eq!(text(&settings, "commit.ai.rules_file"), None);
    assert_eq!(settings.warnings().len(), 1);
}

#[test]
fn ai_shared_file_path_is_relative_to_its_folder() {
    let dir = TempDir::new().unwrap();
    let s = shared_in_repo(&dir, "[commit.ai]\nrules_file = \"prompts/rules.md\"\n");
    let settings = load(None, Some(&s), None);
    assert!(settings.warnings().is_empty());
    assert_eq!(
        settings.path("commit.ai.rules_file"),
        Some(dir.path().join(".rgit/prompts/rules.md"))
    );
}

#[test]
fn ai_shared_path_up_inside_repo_is_allowed() {
    let dir = TempDir::new().unwrap();
    let s = shared_in_repo(
        &dir,
        "[commit.ai]\nrules_file = \"../docs/commit-rules.md\"\n",
    );
    let settings = load(None, Some(&s), None);
    assert!(settings.warnings().is_empty(), "{:?}", settings.warnings());
    let path = settings.path("commit.ai.rules_file").unwrap();
    assert!(path.ends_with(".rgit/../docs/commit-rules.md"));
}
