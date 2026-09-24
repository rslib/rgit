//! Session integrations: install a hook or plugin that runs `rgit --toon`
//! when an agent session starts, so the agent sees the repo state without
//! asking. Claude Code and Codex get a `SessionStart` command hook; OpenCode
//! gets a managed plugin that appends the same text to the system prompt.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::obj;
use crate::output::Output;
use crate::toon::{Node, Obj};

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum HookApp {
    Claude,
    Codex,
    Opencode,
    All,
}

impl std::fmt::Display for HookApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            HookApp::Claude => "claude",
            HookApp::Codex => "codex",
            HookApp::Opencode => "opencode",
            HookApp::All => "all",
        })
    }
}

const MATCHER: &str = "startup|resume|clear|compact";
const PLUGIN_MARKER: &str = "// Managed by `rgit hooks install`.";

struct Targets {
    scope: &'static str,
    claude: PathBuf,
    codex_hooks: PathBuf,
    codex_config: PathBuf,
    opencode: PathBuf,
}

impl Targets {
    fn project(root: &Path) -> Self {
        Targets {
            scope: "project",
            claude: root.join(".claude/settings.json"),
            codex_hooks: root.join(".codex/hooks.json"),
            codex_config: root.join(".codex/config.toml"),
            opencode: root.join(".opencode/plugins/rgit.js"),
        }
    }

    fn user() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set"))?;
        let codex = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(Targets {
            scope: "user",
            claude: home.join(".claude/settings.json"),
            codex_hooks: codex.join("hooks.json"),
            codex_config: codex.join("config.toml"),
            opencode: config.join("opencode/plugins/rgit.js"),
        })
    }
}

struct Row {
    app: &'static str,
    scope: &'static str,
    path: PathBuf,
    state: &'static str,
}

/// Install or repair the rgit session hook for `app`. `user` selects the home
/// directory; otherwise the current directory. Never duplicates an entry.
pub fn install(app: HookApp, user: bool) -> Result<Output> {
    let targets = if user {
        Targets::user()?
    } else {
        Targets::project(&std::env::current_dir()?)
    };
    let rows = install_at(app, &targets, &hook_bin())?;
    let mut out = render(&rows);
    if rows.iter().any(|r| r.state != "unchanged") {
        out = out.help("Start a new agent session to see rgit context");
    }
    if !user && matches!(app, HookApp::Codex | HookApp::All) {
        out = out.help("Codex runs project hooks only after you trust them with /hooks");
    }
    Ok(out)
}

/// Report which apps have an rgit hook in the project and user scopes, and
/// whether its executable path is current.
pub fn status() -> Result<Output> {
    let bin = hook_bin();
    let mut rows = status_at(&Targets::project(&std::env::current_dir()?), &bin)?;
    if let Ok(user) = Targets::user() {
        rows.extend(status_at(&user, &bin)?);
    }
    let mut out = render(&rows);
    if rows.iter().all(|r| r.state == "missing") {
        out = out.help("Run `rgit hooks install` to add rgit context to Claude Code, Codex and OpenCode sessions");
    } else if rows
        .iter()
        .any(|r| r.state == "stale" || r.state == "disabled")
    {
        out = out.help("Run `rgit hooks install` again (add --user for user scope) to repair");
    }
    Ok(out)
}

fn render(rows: &[Row]) -> Output {
    let text = rows
        .iter()
        .map(|r| {
            format!(
                "{:<9} {:<8} {:<7} {}",
                r.state,
                r.app,
                r.scope,
                r.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let data: Vec<Obj> = rows
        .iter()
        .map(|r| {
            obj! {
                "app" => r.app,
                "scope" => r.scope,
                "path" => r.path.display().to_string(),
                "state" => r.state,
            }
        })
        .collect();
    Output::new(text).list(
        "hooks",
        data,
        &["app", "scope", "path", "state"],
        "0 hook files",
    )
}

/// The command name to put in a hook: bare `rgit` when PATH resolves to this
/// executable, else the absolute path.
fn hook_bin() -> String {
    let Ok(exe) = std::env::current_exe().and_then(|p| p.canonicalize()) else {
        return "rgit".to_owned();
    };
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(if cfg!(windows) { "rgit.exe" } else { "rgit" }))
            .find(|p| p.is_file())
    });
    match on_path.and_then(|p| p.canonicalize().ok()) {
        Some(found) if found == exe => "rgit".to_owned(),
        _ => exe.display().to_string(),
    }
}

fn install_at(app: HookApp, t: &Targets, bin: &str) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    let row = |app, path: &Path, state| Row {
        app,
        scope: t.scope,
        path: path.to_path_buf(),
        state,
    };
    if matches!(app, HookApp::Claude | HookApp::All) {
        rows.push(row("claude", &t.claude, install_json_hook(&t.claude, bin)?));
    }
    if matches!(app, HookApp::Codex | HookApp::All) {
        rows.push(row(
            "codex",
            &t.codex_hooks,
            install_json_hook(&t.codex_hooks, bin)?,
        ));
        rows.push(row(
            "codex",
            &t.codex_config,
            enable_codex_hooks(&t.codex_config)?,
        ));
    }
    if matches!(app, HookApp::Opencode | HookApp::All) {
        rows.push(row(
            "opencode",
            &t.opencode,
            install_plugin(&t.opencode, bin)?,
        ));
    }
    Ok(rows)
}

fn status_at(t: &Targets, bin: &str) -> Result<Vec<Row>> {
    let row = |app, path: &Path, state| Row {
        app,
        scope: t.scope,
        path: path.to_path_buf(),
        state,
    };
    let codex = match json_hook_state(&t.codex_hooks, bin)? {
        "missing" => "missing",
        _ if !codex_hooks_enabled(&t.codex_config)? => "disabled",
        state => state,
    };
    Ok(vec![
        row("claude", &t.claude, json_hook_state(&t.claude, bin)?),
        row("codex", &t.codex_hooks, codex),
        row("opencode", &t.opencode, plugin_state(&t.opencode, bin)),
    ])
}

fn hook_command(bin: &str) -> String {
    format!("{} --toon", shell_quote(bin))
}

fn shell_quote(s: &str) -> String {
    let plain = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c));
    if plain && !s.is_empty() {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn is_rgit_command(cmd: &str) -> bool {
    let cmd = cmd.trim();
    let Some(bin) = cmd
        .strip_suffix(" --toon")
        .or_else(|| cmd.strip_suffix(" --porcelain"))
    else {
        return false;
    };
    let bin = bin.trim();
    let bin = bin
        .strip_prefix('\'')
        .and_then(|b| b.strip_suffix('\''))
        .map(|b| b.replace(r"'\''", "'"))
        .unwrap_or_else(|| bin.to_owned());
    Path::new(&bin).file_stem().is_some_and(|s| s == "rgit")
}

/// A JSON file as an order-preserving tree, so rewriting a user's settings
/// keeps their key order.
fn read_json(path: &Path) -> Result<Node> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(Node::Obj(Obj::new())),
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("{} is not valid JSON", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Node::Obj(Obj::new())),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// Commands of every rgit hook under `hooks.SessionStart[*].hooks[*]`.
fn rgit_commands(root: &Node) -> Vec<String> {
    root.get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .and_then(Node::as_list)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Node::as_list))
        .flatten()
        .filter_map(|h| h.get("command").and_then(Node::as_str))
        .filter(|c| is_rgit_command(c))
        .map(str::to_owned)
        .collect()
}

fn json_hook_state(path: &Path, bin: &str) -> Result<&'static str> {
    let root = read_json(path)?;
    let want = hook_command(bin);
    let found = rgit_commands(&root);
    Ok(match found.first() {
        None => "missing",
        Some(c) if c == &want && found.len() == 1 => "current",
        Some(_) => "stale",
    })
}

fn install_json_hook(path: &Path, bin: &str) -> Result<&'static str> {
    let mut root = read_json(path)?;
    let want = hook_command(bin);
    let hooks = root
        .entry("hooks", Node::Obj(Obj::new()))
        .ok_or_else(|| anyhow!("{} is not a JSON object", path.display()))?;
    let groups = hooks
        .entry("SessionStart", Node::List(Vec::new()))
        .ok_or_else(|| anyhow!("{}: `hooks` is not an object", path.display()))?
        .as_list_mut()
        .ok_or_else(|| anyhow!("{}: `hooks.SessionStart` is not an array", path.display()))?;

    let mut seen = false;
    let mut changed = false;
    for group in groups.iter_mut() {
        let Some(list) = group.get_mut("hooks").and_then(Node::as_list_mut) else {
            continue;
        };
        let before = list.len();
        list.retain_mut(|h| {
            let Some(cmd) = h.get("command").and_then(Node::as_str) else {
                return true;
            };
            if !is_rgit_command(cmd) {
                return true;
            }
            if seen {
                return false;
            }
            seen = true;
            if cmd != want {
                if let Some(slot) = h.get_mut("command") {
                    *slot = Node::Str(want.clone());
                }
                changed = true;
            }
            true
        });
        changed |= list.len() != before;
    }
    let before = groups.len();
    groups.retain(|g| {
        g.get("hooks")
            .and_then(Node::as_list)
            .is_none_or(|l| !l.is_empty())
    });
    changed |= groups.len() != before;

    let state = if !seen {
        let hook = Node::Obj(obj! { "type" => "command", "command" => want });
        groups.push(Node::Obj(
            obj! { "matcher" => MATCHER, "hooks" => vec![hook] },
        ));
        "installed"
    } else if changed {
        "updated"
    } else {
        return Ok("unchanged");
    };
    let mut text = serde_json::to_string_pretty(&root)?;
    text.push('\n');
    write_file(path, &text)?;
    Ok(state)
}

fn codex_hooks_enabled(path: &Path) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let table: toml::Table = text
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;
    let features = table.get("features").and_then(|f| f.as_table());
    let flag = |k: &str| features.and_then(|f| f.get(k)).and_then(|v| v.as_bool());
    Ok(flag("hooks").or(flag("codex_hooks")).unwrap_or(false))
}

/// Set `[features].hooks = true` with a line edit, so comments and layout in
/// the rest of the file survive.
fn enable_codex_hooks(path: &Path) -> Result<&'static str> {
    if codex_hooks_enabled(path)? {
        return Ok("unchanged");
    }
    let existed = path.exists();
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let header = lines.iter().position(|l| l.trim() == "[features]");
    match header {
        Some(start) => {
            let end = lines[start + 1..]
                .iter()
                .position(|l| l.trim_start().starts_with('['))
                .map_or(lines.len(), |i| start + 1 + i);
            let key = lines[start + 1..end].iter().position(|l| {
                let l = l.trim_start();
                ["hooks", "codex_hooks"].iter().any(|k| {
                    l.strip_prefix(k)
                        .is_some_and(|rest| rest.trim_start().starts_with('='))
                })
            });
            match key {
                Some(i) => lines[start + 1 + i] = "hooks = true".to_owned(),
                None => lines.insert(start + 1, "hooks = true".to_owned()),
            }
        }
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("[features]".to_owned());
            lines.push("hooks = true".to_owned());
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    let check: toml::Table = out.parse().map_err(|_| {
        anyhow!(
            "could not set [features].hooks = true in {}; add it by hand",
            path.display()
        )
    })?;
    if check
        .get("features")
        .and_then(|f| f.get("hooks"))
        .and_then(|v| v.as_bool())
        != Some(true)
    {
        bail!(
            "could not set [features].hooks = true in {}; add it by hand",
            path.display()
        );
    }
    write_file(path, &out)?;
    Ok(if existed { "updated" } else { "installed" })
}

fn plugin_source(bin: &str) -> String {
    let bin = serde_json::to_string(bin).expect("string serializes");
    format!(
        r#"{PLUGIN_MARKER} Edits are overwritten on reinstall.
const BIN = {bin};

export const RgitContext = async ({{ $, client, directory }}) => {{
  const seen = new Set();
  return {{
    event: async ({{ event }}) => {{
      try {{
        if (event?.type !== "session.created") return;
        const info = event.properties?.info;
        if (!info?.id || seen.has(info.id)) return;
        seen.add(info.id);
        const cwd = info.directory || directory;
        const text = (await $`${{BIN}} --toon`.cwd(cwd).quiet().nothrow().text()).trim();
        if (!text) return;
        await client.session.prompt({{
          path: {{ id: info.id }},
          body: {{ noReply: true, parts: [{{ type: "text", text }}] }},
        }});
      }} catch {{}}
    }},
  }};
}};
"#
    )
}

fn plugin_state(path: &Path, bin: &str) -> &'static str {
    match std::fs::read_to_string(path) {
        Ok(s) if s == plugin_source(bin) => "current",
        Ok(s) if s.starts_with(PLUGIN_MARKER) => "stale",
        _ => "missing",
    }
}

fn install_plugin(path: &Path, bin: &str) -> Result<&'static str> {
    let want = plugin_source(bin);
    let state = match std::fs::read_to_string(path) {
        Ok(s) if s == want => return Ok("unchanged"),
        Ok(s) if s.starts_with(PLUGIN_MARKER) => "updated",
        Ok(_) => bail!(
            "{} exists and is not managed by rgit; move it away first",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "installed",
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    write_file(path, &want)?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rgit-setup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn states(rows: &[Row]) -> Vec<&'static str> {
        rows.iter().map(|r| r.state).collect()
    }

    #[test]
    fn second_install_is_unchanged() {
        let root = tempdir("twice");
        let t = Targets::project(&root);
        let first = install_at(HookApp::All, &t, "rgit").unwrap();
        assert_eq!(states(&first), ["installed"; 4]);
        let second = install_at(HookApp::All, &t, "rgit").unwrap();
        assert_eq!(states(&second), ["unchanged"; 4]);
        let settings = read_json(&t.claude).unwrap();
        assert_eq!(rgit_commands(&settings), ["rgit --toon"]);
        assert!(codex_hooks_enabled(&t.codex_config).unwrap());
        assert_eq!(states(&status_at(&t, "rgit").unwrap()), ["current"; 3]);
    }

    #[test]
    fn preserves_other_settings() {
        let root = tempdir("preserve");
        let t = Targets::project(&root);
        let settings = serde_json::json!({
            "model": "opus",
            "hooks": {
                "SessionStart": [{ "matcher": "startup", "hooks": [{ "type": "command", "command": "echo hi" }] }],
                "Stop": [{ "hooks": [{ "type": "command", "command": "notify" }] }],
            },
        });
        write_file(
            &t.claude,
            r#"{"model":"opus","hooks":{"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"echo hi"}]}],"Stop":[{"hooks":[{"type":"command","command":"notify"}]}]}}"#,
        )
        .unwrap();
        write_file(
            &t.codex_config,
            "# mine\nmodel = \"o3\"\n\n[features]\nweb = true\n",
        )
        .unwrap();

        install_at(HookApp::All, &t, "rgit").unwrap();

        let raw = std::fs::read_to_string(&t.claude).unwrap();
        assert!(
            raw.find("\"model\"").unwrap() < raw.find("\"hooks\"").unwrap(),
            "key order changed: {raw}"
        );
        let after: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(after["model"], "opus");
        assert_eq!(after["hooks"]["Stop"], settings["hooks"]["Stop"]);
        let start = after["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 2);
        assert_eq!(start[0], settings["hooks"]["SessionStart"][0]);
        let config = std::fs::read_to_string(&t.codex_config).unwrap();
        assert_eq!(
            config,
            "# mine\nmodel = \"o3\"\n\n[features]\nhooks = true\nweb = true\n"
        );
    }

    #[test]
    fn migrates_porcelain_hook_to_toon() {
        let root = tempdir("migrate");
        let t = Targets::project(&root);
        write_file(
            &t.claude,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"rgit --porcelain"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(json_hook_state(&t.claude, "rgit").unwrap(), "stale");
        let rows = install_at(HookApp::Claude, &t, "rgit").unwrap();
        assert_eq!(states(&rows), ["updated"]);
        let settings = read_json(&t.claude).unwrap();
        assert_eq!(rgit_commands(&settings), ["rgit --toon"]);
    }

    #[test]
    fn repairs_stale_path() {
        let root = tempdir("repair");
        let t = Targets::project(&root);
        install_at(HookApp::All, &t, "/old/bin/rgit").unwrap();
        assert_eq!(
            states(&status_at(&t, "/new dir/rgit").unwrap()),
            ["stale"; 3]
        );

        let rows = install_at(HookApp::All, &t, "/new dir/rgit").unwrap();
        assert_eq!(
            states(&rows),
            ["updated", "updated", "unchanged", "updated"]
        );
        let settings = read_json(&t.claude).unwrap();
        assert_eq!(rgit_commands(&settings), ["'/new dir/rgit' --toon"]);
        let plugin = std::fs::read_to_string(&t.opencode).unwrap();
        assert!(plugin.contains(r#"const BIN = "/new dir/rgit";"#));
    }

    #[test]
    fn refuses_foreign_plugin() {
        let root = tempdir("foreign");
        let t = Targets::project(&root);
        write_file(&t.opencode, "export const Mine = async () => ({});\n").unwrap();
        assert!(install_at(HookApp::Opencode, &t, "rgit").is_err());
    }

    #[test]
    fn plugin_uses_stable_hooks() {
        let src = plugin_source("rgit");
        assert!(src.starts_with(PLUGIN_MARKER));
        assert!(!src.contains("experimental"));
        assert!(src.contains(r#""session.created""#));
        assert!(src.contains("noReply: true"));
        assert!(src.is_ascii());
    }
}
