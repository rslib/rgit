use std::path::{Path, PathBuf};

use rgit_config::{Files, Layer, OutputFormat, Settings, Source, global_file_with};
use tempfile::TempDir;

fn write(dir: &TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn load(global: Option<&Path>, shared: Option<&Path>, private: Option<&Path>) -> Settings {
    let files = Files {
        global: global.map(Path::to_owned),
        shared: shared.map(Path::to_owned),
        private: private.map(Path::to_owned),
    };
    Settings::load_with(files, &no_env)
}

#[test]
fn private_beats_shared_beats_global() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[output]\ncompact = false\n");
    let s = write(&dir, "s.toml", "[output]\ncompact = true\n");
    let p = write(&dir, "p.toml", "[output]\ncompact = false\n");
    let settings = load(Some(&g), Some(&s), Some(&p));
    assert!(!settings.core().compact);
    assert!(settings.warnings().is_empty());
    let settings = load(Some(&g), Some(&s), None);
    assert!(settings.core().compact);
}

#[test]
fn key_set_only_in_global() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[commit]\nchange_id = true\n");
    let p = write(&dir, "p.toml", "[output]\ncompact = true\n");
    let core = load(Some(&g), None, Some(&p)).core();
    assert!(core.change_id);
    assert!(core.compact);
}

#[test]
fn no_repo_files_means_only_global_and_env() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[output]\nformat = \"json\"\n");
    let env = |name: &str| (name == "RGIT_OUTPUT").then(|| "toon".to_owned());
    let files = Files {
        global: Some(g),
        ..Files::default()
    };
    let settings = Settings::load_with(files, &env);
    assert!(settings.warnings().is_empty());
    assert_eq!(settings.core().output_format, Some(OutputFormat::Toon));
}

#[test]
fn rgit_config_wins_over_xdg_and_home() {
    let env = |name: &str| match name {
        "RGIT_CONFIG" => Some("/x/own.toml".to_owned()),
        "XDG_CONFIG_HOME" => Some("/xdg".to_owned()),
        "HOME" => Some("/home/me".to_owned()),
        _ => None,
    };
    assert_eq!(global_file_with(&env), Some(PathBuf::from("/x/own.toml")));
    let env = |name: &str| match name {
        "XDG_CONFIG_HOME" => Some("/xdg".to_owned()),
        "HOME" => Some("/home/me".to_owned()),
        _ => None,
    };
    assert_eq!(
        global_file_with(&env),
        Some(PathBuf::from("/xdg/rgit/config.toml"))
    );
    let env = |name: &str| (name == "HOME").then(|| "/home/me".to_owned());
    assert_eq!(
        global_file_with(&env),
        Some(PathBuf::from("/home/me/.config/rgit/config.toml"))
    );
    assert_eq!(global_file_with(&no_env), None);
}

#[test]
fn missing_file_is_not_a_warning() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("nope.toml");
    let settings = load(Some(&missing), None, None);
    assert!(settings.warnings().is_empty());
    assert!(!settings.core().compact);
    assert_eq!(settings.core().output_format, None);
}

#[test]
fn syntax_error_warns_with_file_and_line_and_others_apply() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "output = [");
    let s = write(&dir, "s.toml", "[output]\ncompact = true\n");
    let settings = load(Some(&g), Some(&s), None);
    assert_eq!(settings.warnings().len(), 1);
    let text = settings.warnings()[0].to_string();
    assert!(
        text.starts_with(&format!("{}: line 1: ", g.display())),
        "{text}"
    );
    assert!(settings.core().compact);
}

#[test]
fn unknown_key_warns_and_other_keys_apply() {
    let dir = TempDir::new().unwrap();
    let g = write(
        &dir,
        "g.toml",
        "[output]\ncompcat = true\nformat = \"json\"\n",
    );
    let settings = load(Some(&g), None, None);
    assert_eq!(settings.warnings().len(), 1);
    assert_eq!(
        settings.warnings()[0].to_string(),
        format!("{}: unknown key output.compcat", g.display())
    );
    assert!(!settings.core().compact);
    assert_eq!(settings.core().output_format, Some(OutputFormat::Json));
}

#[test]
fn wrong_type_and_bad_enum_warn_and_drop() {
    let dir = TempDir::new().unwrap();
    let g = write(
        &dir,
        "g.toml",
        "[output]\ncompact = \"yes\"\nformat = \"xml\"\n",
    );
    let settings = load(Some(&g), None, None);
    let texts: Vec<String> = settings.warnings().iter().map(|w| w.to_string()).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("output.compact")));
    assert!(texts.iter().any(|t| t.contains("output.format")));
    assert!(
        texts
            .iter()
            .all(|t| t.starts_with(&g.display().to_string()))
    );
    assert!(!settings.core().compact);
    assert_eq!(settings.core().output_format, None);
}

#[test]
fn env_beats_file_output_format() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[output]\nformat = \"json\"\n");
    let env = |name: &str| (name == "RGIT_OUTPUT").then(|| "human".to_owned());
    let files = Files {
        global: Some(g),
        ..Files::default()
    };
    let settings = Settings::load_with(files, &env);
    assert_eq!(settings.core().output_format, Some(OutputFormat::Human));
    assert_eq!(
        settings.source("output.format"),
        Some(Source::Env("RGIT_OUTPUT"))
    );
}

#[test]
fn bad_env_value_warns_naming_the_variable() {
    let env = |name: &str| (name == "RGIT_OUTPUT").then(|| "xml".to_owned());
    let settings = Settings::load_with(Files::default(), &env);
    assert_eq!(settings.warnings().len(), 1);
    assert!(
        settings.warnings()[0]
            .to_string()
            .starts_with("RGIT_OUTPUT: ")
    );
    assert_eq!(settings.core().output_format, None);
}

#[test]
fn source_reports_layer_and_path() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[output]\ncompact = false\n");
    let p = write(&dir, "p.toml", "[output]\ncompact = true\n");
    let settings = load(Some(&g), None, Some(&p));
    assert_eq!(
        settings.source("output.compact"),
        Some(Source::File {
            layer: Layer::Private,
            path: p
        })
    );
}

#[test]
fn source_of_unset_known_key_is_default_and_unknown_is_none() {
    let settings = load(None, None, None);
    assert_eq!(settings.source("workflow.preset"), Some(Source::Default));
    assert_eq!(settings.source("nope.nothing"), None);
}

#[test]
fn array_for_a_scalar_key_warns_and_is_dropped() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[workflow]\npreset = [\"a\", \"b\"]\n");
    let settings = load(Some(&g), None, None);
    assert_eq!(settings.warnings().len(), 1);
    assert_eq!(settings.core().workflow_preset, None);
}

#[test]
fn tui_shaped_keys_are_known() {
    let dir = TempDir::new().unwrap();
    let g = write(
        &dir,
        "g.toml",
        r##"
profile = "mine"
[theme]
name = "mono"
accent = "#ff00ff"
[ui]
log_limit = 50
glyphs = "nerd"
transparent = "body"
[commit]
gpg_sign = true
auto_restack = false
[keys.vim]
stage = "s"
[profiles.mine]
extends = "emacs"
[forge]
default_profile = "work"
[forge.profiles.work]
provider = "github"
account = "me"
host = "github.com"
"##,
    );
    let settings = load(Some(&g), None, None);
    assert!(settings.warnings().is_empty(), "{:?}", settings.warnings());
    for key in [
        "theme.name",
        "ui.log_limit",
        "keys.vim.stage",
        "profiles.mine.extends",
        "forge.profiles.work.host",
    ] {
        assert!(
            matches!(settings.source(key), Some(Source::File { .. })),
            "{key}"
        );
    }
    assert_eq!(settings.table()["ui"]["log_limit"].as_integer(), Some(50));
}

#[test]
fn section_deserializes_a_table_and_defaults_when_missing() {
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Ui {
        log_limit: i64,
    }
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[ui]\nlog_limit = 7\n");
    let settings = load(Some(&g), None, None);
    assert_eq!(settings.section::<Ui>("ui").unwrap().log_limit, 7);
    assert_eq!(settings.section::<Ui>("absent").unwrap().log_limit, 0);
}

#[test]
fn entries_list_set_and_default_keys_sorted() {
    let dir = TempDir::new().unwrap();
    let g = write(
        &dir,
        "g.toml",
        "[output]\ncompact = true\n[keys.vim]\nstage = \"s\"\n",
    );
    let entries = load(Some(&g), None, None).entries();
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    let compact = entries.iter().find(|e| e.key == "output.compact").unwrap();
    assert_eq!(compact.value.as_deref(), Some("true"));
    assert!(matches!(compact.source, Source::File { .. }));
    let stage = entries.iter().find(|e| e.key == "keys.vim.stage").unwrap();
    assert_eq!(stage.value.as_deref(), Some("s"));
    let limit = entries.iter().find(|e| e.key == "ui.log_limit").unwrap();
    assert_eq!(limit.value.as_deref(), Some("200"));
    assert_eq!(limit.source, Source::Default);
}

#[test]
fn shared_forge_keys_are_ignored_with_a_warning() {
    let dir = TempDir::new().unwrap();
    let g = write(&dir, "g.toml", "[forge]\ndefault_profile = \"work\"\n");
    let s = write(
        &dir,
        "s.toml",
        "[forge]\ndefault_profile = \"x\"\n[forge.profiles.x]\nhost = \"https://evil.example\"\n",
    );
    let settings = load(Some(&g), Some(&s), None);
    assert_eq!(
        settings.table()["forge"]["default_profile"].as_str(),
        Some("work")
    );
    assert!(settings.table()["forge"].get("profiles").is_none());
    let texts: Vec<String> = settings.warnings().iter().map(|w| w.to_string()).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(
        texts
            .iter()
            .all(|t| t.starts_with(&s.display().to_string()))
    );
    assert!(texts.iter().any(|t| t.contains("forge.default_profile")));
    assert!(texts.iter().any(|t| t.contains("forge.profiles.x.host")));
}

#[test]
fn shared_file_may_set_other_keys() {
    let dir = TempDir::new().unwrap();
    let s = write(&dir, "s.toml", "[output]\ncompact = true\n");
    let settings = load(None, Some(&s), None);
    assert!(settings.warnings().is_empty());
    assert!(settings.core().compact);
}

#[test]
fn shared_private_file_may_set_forge_keys() {
    let dir = TempDir::new().unwrap();
    let p = write(&dir, "p.toml", "[forge]\ndefault_profile = \"work\"\n");
    let settings = load(None, None, Some(&p));
    assert!(settings.warnings().is_empty());
    assert_eq!(
        settings.table()["forge"]["default_profile"].as_str(),
        Some("work")
    );
}

#[test]
fn empty_rgit_output_is_unset() {
    let env = |name: &str| (name == "RGIT_OUTPUT").then(|| "  ".to_owned());
    let settings = Settings::load_with(Files::default(), &env);
    assert!(settings.warnings().is_empty());
    assert_eq!(settings.core().output_format, None);
    assert_eq!(settings.source("output.format"), Some(Source::Default));
}
