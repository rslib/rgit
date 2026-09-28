//! `rgit agent`, `rgit hook session-start` and `rgit tool` against a temp
//! HOME, config and data homes and project, with fake agent app programs on
//! a temp PATH and a fake `claude` that only logs its `plugin` calls.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

struct Env {
    root: PathBuf,
    home: PathBuf,
    xdg: PathBuf,
    data: PathBuf,
    project: PathBuf,
    bin: PathBuf,
}

impl Env {
    /// A fresh environment with a git repo as the project and `apps` on
    /// PATH; `claude` is the fake from the fixtures.
    fn new(tag: &str, apps: &[&str]) -> Env {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("rgit-agent-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let env = Env {
            home: root.join("home"),
            xdg: root.join("xdg"),
            data: root.join("data"),
            project: root.join("project"),
            bin: root.join("bin"),
            root,
        };
        for dir in [&env.home, &env.xdg, &env.data, &env.project, &env.bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for app in apps {
            let path = env.bin.join(app);
            let text = if *app == "claude" {
                include_str!("fixtures/fake-claude.sh")
            } else {
                "#!/bin/sh\n"
            };
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        for args in [
            &["init", "-q", "-b", "main"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        ] {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&env.project)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
        env
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
        cmd.args(args)
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg)
            .env("XDG_DATA_HOME", &self.data)
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("RGIT_CLAUDE", self.bin.join("claude"))
            .env("FAKE_CLAUDE_HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("RGIT_OPLOG", "0")
            .env_remove("CODEX_HOME")
            .env_remove("PI_CODING_AGENT_DIR");
        cmd
    }

    /// Run `rgit <args>` in the project. Returns (stdout, stderr, exit code).
    fn rgit(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).stdin(Stdio::null()).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.rgit(args);
        assert_eq!(code, 0, "rgit {args:?}: {out}{err}");
        out
    }

    fn read(&self, path: impl AsRef<Path>) -> String {
        let path = self.home.join(path);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn json(&self, path: impl AsRef<Path>) -> Value {
        serde_json::from_str(&self.read(path)).unwrap()
    }

    fn write(&self, path: impl AsRef<Path>, text: &str) {
        let path = self.home.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn exists(&self, path: impl AsRef<Path>) -> bool {
        self.home.join(path).exists()
    }

    fn plugin(&self) -> PathBuf {
        self.data.join("rgit/claude-plugin/plugins/rgit")
    }

    /// The fake claude's calls.
    fn claude_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.bin.join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// `agent status --json` as {"app scope" => [plugin, skill, hook, mcp]}.
    fn status(&self) -> std::collections::HashMap<String, [String; 4]> {
        let out: Value = serde_json::from_str(&self.ok(&["--json", "agent", "status"])).unwrap();
        out["apps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let s = |k: &str| r[k].as_str().unwrap().to_owned();
                (
                    format!("{} {}", s("app"), s("scope")),
                    [s("plugin"), s("skill"), s("hook"), s("mcp")],
                )
            })
            .collect()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// What hooks and entries run: this rgit, by its path, as it is not on PATH.
fn bin() -> String {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_rgit"))
        .unwrap()
        .display()
        .to_string()
}

fn hook_command() -> String {
    let bin = bin();
    if bin.contains(' ') {
        format!("'{bin}' hook session-start")
    } else {
        format!("{bin} hook session-start")
    }
}

fn row(v: [&str; 4]) -> [String; 4] {
    v.map(str::to_owned)
}

fn no_change(out: &str) -> bool {
    !out.contains("written") && !out.contains("removed")
}

#[test]
fn install_sets_up_detected_apps_once() {
    let env = Env::new("detect", &["claude", "codex", "pi"]);
    let out = env.ok(&["agent", "install"]);
    assert!(out.contains("Restart Claude Code"), "{out}");

    let plugin = env.plugin();
    let skill = std::fs::read_to_string(plugin.join("skills/rgit/SKILL.md")).unwrap();
    assert!(skill.starts_with("---\nname: rgit\n"));
    assert!(plugin.join("skills/rgit/references/commands.md").exists());
    assert!(!plugin.join(".mcp.json").exists(), "MCP only with --mcp");
    let hooks: Value =
        serde_json::from_str(&std::fs::read_to_string(plugin.join("hooks/hooks.json")).unwrap())
            .unwrap();
    assert_eq!(
        hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        hook_command()
    );
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert!(manifest["version"].as_str().unwrap().contains('+'));
    assert_eq!(manifest["author"]["name"], "rgit");
    let log = env.claude_log();
    assert!(
        log.iter().any(|l| l.starts_with("plugin marketplace add ")),
        "{log:?}"
    );
    assert!(
        log.contains(&"plugin install rgit@rgit".to_owned()),
        "{log:?}"
    );

    assert_eq!(env.read(".agents/skills/rgit/SKILL.md"), skill);
    assert_eq!(
        env.json(".codex/hooks.json")["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        hook_command()
    );
    assert_eq!(env.read(".pi/agent/skills/rgit/SKILL.md"), skill);
    let ext = env.read(".pi/agent/extensions/rgit.ts");
    assert!(
        ext.starts_with("// Generated by rgit agent install;"),
        "{ext}"
    );
    assert!(ext.contains("const TOOLS_JSON: string = \"\";"));
    for absent in [
        ".omp",
        ".codex/config.toml",
        ".claude/skills",
        ".claude/settings.json",
    ] {
        assert!(!env.exists(absent), "{absent} written");
    }
    assert!(!env.xdg.join("opencode").exists());

    let before = env.claude_log().len();
    let again = env.ok(&["agent", "install"]);
    assert!(no_change(&again), "reinstall must be a no-op: {again}");
    assert!(
        env.claude_log()[before..]
            .iter()
            .all(|l| l.ends_with("--json")),
        "only read-only calls: {:?}",
        &env.claude_log()[before..]
    );

    let status = env.status();
    assert_eq!(
        status["claude user"],
        row(["current", "current", "current", "missing"])
    );
    assert_eq!(
        status["codex user"],
        row(["unsupported", "current", "current", "missing"])
    );
    assert_eq!(
        status["pi user"],
        row(["current", "current", "current", "missing"])
    );
    assert_eq!(
        status["omp user"],
        row(["missing", "missing", "missing", "missing"])
    );
    assert_eq!(status.len(), 7);
    assert!(
        env.ok(&["--toon", "agent", "status"])
            .contains("apps[7]{app,scope,plugin,skill,hook,mcp}:")
    );
}

#[test]
fn install_mcp_everywhere_and_keep_it() {
    let env = Env::new("mcp", &["claude"]);
    env.ok(&["agent", "install", "--mcp", "all"]);
    let mcp: Value =
        serde_json::from_str(&std::fs::read_to_string(env.plugin().join(".mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        mcp["mcpServers"]["rgit"],
        json!({ "command": bin(), "args": ["mcp"] })
    );
    let codex: toml::Table = env.read(".codex/config.toml").parse().unwrap();
    assert_eq!(
        codex["mcp_servers"]["rgit"]["command"].as_str(),
        Some(&*bin())
    );
    assert_eq!(
        codex["mcp_servers"]["rgit"]["args"][0].as_str(),
        Some("mcp")
    );
    let opencode: Value = serde_json::from_str(
        &std::fs::read_to_string(env.xdg.join("opencode/opencode.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        opencode["mcp"]["rgit"],
        json!({ "type": "local", "command": [bin(), "mcp"], "enabled": true })
    );
    assert!(env.xdg.join("opencode/skills/rgit/SKILL.md").exists());
    assert!(env.xdg.join("opencode/plugins/rgit.js").exists());
    for ext in [
        ".pi/agent/extensions/rgit.ts",
        ".omp/agent/extensions/rgit.ts",
    ] {
        let text = env.read(ext);
        assert!(text.contains("const TOOLS_JSON: string = \"[{"), "{ext}");
        assert!(text.contains("git_status"), "{ext}");
    }
    let all = row(["current", "current", "current", "current"]);
    let status = env.status();
    for app in ["claude", "pi", "omp"] {
        assert_eq!(status[&format!("{app} user")], all, "{app}");
    }
    for app in ["codex", "opencode"] {
        assert_eq!(
            status[&format!("{app} user")],
            row(["unsupported", "current", "current", "current"]),
            "{app}"
        );
    }

    let again = env.ok(&["agent", "install", "all"]);
    assert!(no_change(&again), "MCP stays without --mcp: {again}");
    assert!(env.plugin().join(".mcp.json").exists());
}

#[test]
fn project_scope_is_for_codex_and_opencode() {
    let env = Env::new("project", &["codex", "opencode", "claude"]);
    env.ok(&["agent", "install", "--project", "--mcp"]);
    let p = |rel: &str| env.project.join(rel);
    assert!(p(".agents/skills/rgit/SKILL.md").exists());
    assert!(p(".codex/hooks.json").exists());
    assert!(p(".codex/config.toml").exists());
    assert!(p(".opencode/skills/rgit/SKILL.md").exists());
    assert!(p(".opencode/plugins/rgit.js").exists());
    assert!(p("opencode.json").exists());
    assert!(!env.data.join("rgit").exists(), "claude is per user only");
    assert!(env.claude_log().is_empty());
    let status = env.status();
    assert_eq!(
        status["codex project"],
        row(["unsupported", "current", "current", "current"])
    );
    assert_eq!(
        status["codex user"],
        row(["unsupported", "missing", "missing", "missing"])
    );

    let (_, err, code) = env.rgit(&["agent", "install", "--project", "claude"]);
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("drop --project"), "{err}");

    env.ok(&["agent", "uninstall", "--project"]);
    for rel in [
        ".agents/skills/rgit",
        ".codex/hooks.json",
        ".codex/config.toml",
        ".opencode/plugins/rgit.js",
        ".opencode/skills/rgit",
        "opencode.json",
    ] {
        assert!(!p(rel).exists(), "{rel} left");
    }
}

#[test]
fn install_migrates_old_files_repairs_and_keeps_the_rest() {
    let env = Env::new("repair", &["claude"]);
    let old_skill = "---\nname: rgit\ndescription: old\n---\nold\n";
    env.write(".claude/skills/rgit/SKILL.md", old_skill);
    env.write(
        ".claude/settings.json",
        r#"{"model":"opus","big":18446744073709551615,"hooks":{"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"echo hi"},{"type":"command","command":"/old/rgit --toon"}]}],"Stop":[{"hooks":[{"type":"command","command":"notify"}]}]}}"#,
    );
    let project_settings = env.project.join(".claude/settings.json");
    std::fs::create_dir_all(project_settings.parent().unwrap()).unwrap();
    std::fs::write(
        &project_settings,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"rgit --toon"}]}]}}"#,
    )
    .unwrap();
    env.write(
        ".codex/hooks.json",
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"mine"},{"type":"command","command":"/old/rgit --toon"}]}]}}"#,
    );
    env.write(
        ".codex/config.toml",
        "# mine\nmodel = \"o3\"\n\n[features]\nhooks = false # off\n\n[mcp_servers.other]\ncommand = \"x\"\n\n[mcp_servers.rgit]\ncommand = \"/old/rgit\"\nargs = [\"mcp\"]\n",
    );
    std::fs::create_dir_all(env.xdg.join("opencode/plugins")).unwrap();
    std::fs::write(
        env.xdg.join("opencode/plugins/rgit.js"),
        "// Managed by `rgit hooks install`. old\n",
    )
    .unwrap();
    std::fs::write(
        env.xdg.join("opencode/opencode.json"),
        r#"{"theme":"x","mcp":{"rgit":{"type":"local","command":["/old/rgit","mcp"],"environment":{"A":"1"}}}}"#,
    )
    .unwrap();
    env.write(
        ".pi/agent/extensions/rgit.ts",
        "// Generated by rgit agent install; old\n",
    );
    let status = env.status();
    assert_eq!(
        status["codex user"],
        row(["unsupported", "missing", "stale", "stale"])
    );
    assert_eq!(
        status["opencode user"],
        row(["unsupported", "missing", "stale", "stale"])
    );
    assert_eq!(status["pi user"][0], "stale");

    let out = env.ok(&[
        "agent", "install", "--mcp", "claude", "codex", "opencode", "pi",
    ]);
    assert!(out.contains("written"), "{out}");

    assert!(
        !env.exists(".claude/skills/rgit"),
        "the old loose skill goes"
    );
    let raw = env.read(".claude/settings.json");
    assert!(raw.contains("18446744073709551615"), "{raw}");
    assert!(
        raw.find("\"model\"").unwrap() < raw.find("\"hooks\"").unwrap(),
        "{raw}"
    );
    let settings: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        settings["hooks"],
        json!({
            "SessionStart": [{ "matcher": "startup", "hooks": [{ "type": "command", "command": "echo hi" }] }],
            "Stop": [{ "hooks": [{ "type": "command", "command": "notify" }] }],
        })
    );
    assert!(!project_settings.exists(), "held only rgit's old hook");

    let hooks = env.json(".codex/hooks.json");
    let list = &hooks["hooks"]["SessionStart"][0]["hooks"];
    assert_eq!(list[0]["command"], "mine");
    assert_eq!(list[1]["command"], hook_command());
    let config = env.read(".codex/config.toml");
    assert!(
        config.starts_with("# mine\nmodel = \"o3\"\n\n[features]\nhooks = true"),
        "{config}"
    );
    assert!(
        config.contains("[mcp_servers.other]\ncommand = \"x\""),
        "{config}"
    );
    assert!(
        config.contains(&format!("command = \"{}\"", bin())),
        "{config}"
    );

    let opencode: Value = serde_json::from_str(
        &std::fs::read_to_string(env.xdg.join("opencode/opencode.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(opencode["theme"], "x");
    assert_eq!(opencode["mcp"]["rgit"]["command"], json!([bin(), "mcp"]));
    assert_eq!(opencode["mcp"]["rgit"]["environment"], json!({ "A": "1" }));
    let plugin = std::fs::read_to_string(env.xdg.join("opencode/plugins/rgit.js")).unwrap();
    assert!(plugin.starts_with("// Generated by rgit agent install;"));

    let status = env.status();
    assert_eq!(
        status["codex user"],
        row(["unsupported", "current", "current", "current"])
    );
    assert_eq!(
        status["opencode user"],
        row(["unsupported", "current", "current", "current"])
    );
    assert_eq!(
        status["pi user"],
        row(["current", "current", "current", "current"])
    );
}

#[test]
fn a_stale_plugin_is_updated() {
    let env = Env::new("stale-plugin", &["claude"]);
    env.ok(&["agent", "install", "claude"]);
    std::fs::write(env.bin.join("ver"), "0.0.0+old\n").unwrap();
    env.write(
        ".claude/plugins/installed_plugins.json",
        r#"{"version":2,"plugins":{"rgit@rgit":[{"scope":"user","version":"0.0.0+old"}]}}"#,
    );
    assert_eq!(env.status()["claude user"][0], "stale");
    let out = env.ok(&["agent", "status"]);
    assert!(out.contains("to repair"), "{out}");
    env.ok(&["agent", "install", "claude"]);
    let log = env.claude_log();
    assert!(
        log.contains(&"plugin marketplace update rgit".to_owned()),
        "{log:?}"
    );
    assert!(
        log.contains(&"plugin update rgit@rgit".to_owned()),
        "{log:?}"
    );
    assert_eq!(env.status()["claude user"][0], "current");
}

#[test]
fn install_stops_before_any_write() {
    let env = Env::new("refuse", &["pi"]);
    env.write(".pi/agent/extensions/rgit.ts", "export default () => {};\n");
    let (_, err, code) = env.rgit(&["agent", "install", "pi"]);
    assert_eq!(code, 1);
    assert!(err.contains("is not rgit's"), "{err}");
    assert!(!env.exists(".pi/agent/skills/rgit"), "nothing written");

    std::fs::create_dir_all(env.xdg.join("opencode")).unwrap();
    std::fs::write(env.xdg.join("opencode/opencode.jsonc"), "{ // mine\n}").unwrap();
    let (_, err, code) = env.rgit(&["agent", "install", "--mcp", "opencode"]);
    assert_eq!(code, 1);
    assert!(err.contains("by hand"), "{err}");
    assert!(!env.xdg.join("opencode/skills").exists());

    // Claude Code found by its folder, but without its `claude` command.
    let env = Env::new("no-claude", &["pi"]);
    std::fs::create_dir_all(env.home.join(".claude")).unwrap();
    let (_, err, code) = env
        .cmd(&["agent", "install"])
        .env("RGIT_CLAUDE", "/no/such/claude")
        .output()
        .map(|o| {
            (
                String::new(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
                o.status.code().unwrap(),
            )
        })
        .unwrap();
    assert_eq!(code, 1);
    assert!(err.contains("claude"), "{err}");
    assert!(!env.exists(".pi/agent/skills/rgit"));

    let env = Env::new("none", &[]);
    let (_, err, code) = env.rgit(&["agent", "install"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("no agent apps found") && err.contains("rgit agent install claude"),
        "{err}"
    );
}

#[test]
fn uninstall_removes_only_what_rgit_wrote() {
    let env = Env::new("uninstall", &["claude"]);
    env.write(".codex/config.toml", "# mine\nmodel = \"o3\"\n");
    env.write(".pi/agent/skills/rgit/notes.md", "mine\n");
    env.ok(&["agent", "install", "--mcp", "claude", "codex", "pi"]);

    env.ok(&["agent", "uninstall", "--mcp-only", "claude", "pi"]);
    assert!(!env.plugin().join(".mcp.json").exists());
    assert!(env.plugin().join("hooks/hooks.json").exists());
    assert!(
        env.claude_log()
            .contains(&"plugin update rgit@rgit".to_owned())
    );
    assert!(
        env.read(".pi/agent/extensions/rgit.ts")
            .contains("const TOOLS_JSON: string = \"\";")
    );
    let status = env.status();
    assert_eq!(
        status["claude user"],
        row(["current", "current", "current", "missing"])
    );
    assert_eq!(
        status["pi user"],
        row(["current", "current", "current", "missing"])
    );

    env.ok(&["agent", "uninstall", "--hook-only", "pi"]);
    assert!(
        !env.exists(".pi/agent/extensions/rgit.ts"),
        "nothing left in it"
    );
    assert!(env.exists(".pi/agent/skills/rgit/SKILL.md"));

    env.ok(&["agent", "uninstall", "codex"]);
    assert!(!env.exists(".agents/skills/rgit"));
    assert!(!env.exists(".codex/hooks.json"));
    assert_eq!(env.read(".codex/config.toml"), "# mine\nmodel = \"o3\"\n");

    let out = env.ok(&["agent", "uninstall"]);
    assert!(out.contains("removed"), "{out}");
    let log = env.claude_log();
    assert!(
        log.contains(&"plugin uninstall rgit@rgit".to_owned()),
        "{log:?}"
    );
    assert!(
        log.contains(&"plugin marketplace remove rgit".to_owned()),
        "{log:?}"
    );
    assert!(!env.data.join("rgit/claude-plugin").exists());
    assert!(!env.exists(".pi/agent/skills/rgit/SKILL.md"));
    assert_eq!(env.read(".pi/agent/skills/rgit/notes.md"), "mine\n");
    for (app, [_, skill, hook, mcp]) in env.status() {
        assert_eq!([skill, hook, mcp], ["missing"; 3], "{app}");
    }
    let again = env.ok(&["agent", "uninstall"]);
    assert!(no_change(&again), "{again}");
}

#[test]
fn session_start_hook_and_tool_entry_points() {
    let env = Env::new("entry", &[]);
    let out = env.ok(&["hook", "session-start"]);
    assert!(out.contains("branch: main"), "{out}");
    assert!(out.contains("--toon"), "{out}");
    let (out, _, code) = env.rgit(&["hook", "session-start"]);
    assert_eq!(code, 0);
    assert!(!out.contains("usage"), "{out}");
    let elsewhere = env
        .cmd(&["hook", "session-start"])
        .current_dir(&env.home)
        .output()
        .unwrap();
    assert!(elsewhere.status.success(), "outside a repo too");

    let tool = |name: &str, input: &str| {
        use std::io::Write;
        let mut child = env
            .cmd(&["tool", name])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status.code().unwrap(),
        )
    };
    let (out, code) = tool("git_status", "");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("branch: main"), "{out}");
    let (out, code) = tool("git_log", r#"{"limit": 1}"#);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("init"), "{out}");
    let (out, code) = tool("git_nope", "{}");
    assert_eq!(code, 1);
    assert!(out.contains("unknown tool"), "{out}");
    let (out, code) = tool("git_status", "[1]");
    assert_eq!(code, 1);
    assert!(out.contains("JSON object"), "{out}");
}

#[test]
fn extension_drives_rgit() {
    let ts = Command::new("node")
        .args(["-p", "process.features.typescript"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    if !matches!(ts.as_deref(), Some("strip" | "transform")) {
        eprintln!("skipped: no node with TypeScript support on PATH");
        return;
    }
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extensions/rgit");
    if !package.join("node_modules/typebox").exists() {
        eprintln!("skipped: run `npm install` in extensions/rgit to test the extension");
        return;
    }
    // Inside the package, so `import "typebox"` resolves as pi and omp resolve it.
    let generated = package.join(".test");
    std::fs::create_dir_all(&generated).unwrap();
    for (harness, flags, mode) in [("pi", &["--mcp"][..], "tools"), ("omp", &[], "no-tools")] {
        let env = Env::new(&format!("ext-{harness}"), &[]);
        let mut args = vec!["agent", "install", harness];
        args.extend(flags);
        env.ok(&args);
        let file = generated.join(format!("rgit-{harness}-{}.ts", std::process::id()));
        std::fs::copy(
            env.home
                .join(format!(".{harness}/agent/extensions/rgit.ts")),
            &file,
        )
        .unwrap();
        let out = Command::new("node")
            .env("HOME", &env.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("RGIT_OPLOG", "0")
            .arg(package.join("test/run.mjs"))
            .arg(&file)
            .arg(&env.project)
            .arg(mode)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&file);
        assert!(
            out.status.success(),
            "{harness}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn agent_skill_prints_the_committed_skill() {
    let env = Env::new("skill", &[]);
    assert_eq!(
        env.ok(&["agent", "skill"]),
        include_str!("../../../skills/rgit/SKILL.md")
    );
    assert_eq!(
        env.ok(&["agent", "skill", "--reference"]),
        include_str!("../../../skills/rgit/references/commands.md")
    );
}

#[test]
fn old_skills_and_hooks_commands_are_gone() {
    let env = Env::new("gone", &[]);
    for args in [
        &["skills", "install"][..],
        &["hooks", "install"],
        &["hooks", "status"],
    ] {
        let (out, err, code) = env.rgit(args);
        assert_ne!(code, 0, "{args:?} still works: {out}{err}");
    }
    assert!(!env.exists(".claude"));
}
