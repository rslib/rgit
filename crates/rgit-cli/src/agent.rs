//! `rgit agent`: set agent apps (harnesses) up for rgit. Each gets what it
//! supports of three parts: the rgit Agent Skill, a session-start hook that
//! runs `rgit hook session-start`, and, with `--mcp`, rgit's MCP tools.
//!
//! Claude Code gets a plugin in a local marketplace that rgit writes and
//! installs with `claude plugin`. Pi and omp get the skill and a generated
//! TypeScript extension. Codex and OpenCode get their config files edited
//! in place, keeping the user's other entries.
//!
//! `plan` turns a request into steps without touching anything; `apply`
//! checks every step can run, then runs them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::cli::{AgentCmd, CliError};
use crate::obj;
use crate::output::Output;
use crate::toon::Obj;

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum App {
    Claude,
    Codex,
    Opencode,
    Pi,
    Omp,
    All,
}

const APPS: [App; 5] = [App::Claude, App::Codex, App::Opencode, App::Pi, App::Omp];

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Claude => "claude",
            App::Codex => "codex",
            App::Opencode => "opencode",
            App::Pi => "pi",
            App::Omp => "omp",
            App::All => "all",
        }
    }

    /// Codex and OpenCode read project config; the others only per user.
    fn has_project_scope(self) -> bool {
        matches!(self, App::Codex | App::Opencode)
    }
}

impl std::fmt::Display for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The three things rgit can add to a harness.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Parts {
    skill: bool,
    hook: bool,
    mcp: bool,
}

impl Parts {
    const NONE: Parts = Parts {
        skill: false,
        hook: false,
        mcp: false,
    };

    fn any(self) -> bool {
        self.skill || self.hook || self.mcp
    }
}

pub fn run(cmd: AgentCmd) -> Result<Output> {
    match cmd {
        AgentCmd::Install {
            apps,
            project,
            mcp,
            no_mcp: _,
        } => install(&apps, project, mcp),
        AgentCmd::Status => status(),
        AgentCmd::Uninstall {
            apps,
            project,
            skill_only,
            hook_only,
            mcp_only,
        } => {
            let only = (skill_only || hook_only || mcp_only).then_some(Parts {
                skill: skill_only,
                hook: hook_only,
                mcp: mcp_only,
            });
            uninstall(&apps, project, only)
        }
        AgentCmd::Skill { reference } => Ok(Output::message(
            if reference {
                crate::cli::skill_reference()
            } else {
                crate::cli::skill_markdown()
            }
            .trim_end(),
        )),
    }
}

// Where things go

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home() -> Result<PathBuf> {
    env_path("HOME").ok_or_else(|| anyhow!("HOME is not set"))
}

/// The Claude Code plugin id: plugin `rgit` from the local marketplace `rgit`.
const PLUGIN: &str = "rgit@rgit";

/// What a report shows for the step that installs the plugin into Claude Code.
fn claude_plugin_path() -> PathBuf {
    PathBuf::from(format!("claude plugin {PLUGIN}"))
}

/// The local marketplace rgit writes for Claude Code:
/// `$XDG_DATA_HOME/rgit/claude-plugin`.
fn plugin_dir(home: &Path) -> PathBuf {
    // Unit tests pass a temporary home; the real XDG_DATA_HOME must not leak in.
    if cfg!(test) {
        return home.join(".local/share/rgit/claude-plugin");
    }
    env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local/share"))
        .join("rgit/claude-plugin")
}

/// The `claude` program install runs: `RGIT_CLAUDE` when set, so tests never
/// touch a real Claude Code.
fn claude_program() -> String {
    // A unit test that forgets the fake must fail, never reach Claude Code.
    if cfg!(test) {
        return "/nonexistent/claude-in-unit-tests".into();
    }
    std::env::var("RGIT_CLAUDE").unwrap_or_else(|_| "claude".into())
}

/// One harness's files in one scope: the user's (`base` is `$HOME`) or a
/// project's (`base` is the project directory; Codex and OpenCode only).
struct Place {
    app: App,
    project: bool,
    base: PathBuf,
}

impl Place {
    fn scope(&self) -> &'static str {
        if self.project { "project" } else { "user" }
    }

    fn codex(&self) -> PathBuf {
        if self.project {
            self.base.join(".codex")
        } else {
            env_path("CODEX_HOME").unwrap_or_else(|| self.base.join(".codex"))
        }
    }

    fn opencode(&self) -> PathBuf {
        if self.project {
            self.base.join(".opencode")
        } else {
            env_path("XDG_CONFIG_HOME")
                .unwrap_or_else(|| self.base.join(".config"))
                .join("opencode")
        }
    }

    fn opencode_config(&self) -> PathBuf {
        let dir = if self.project {
            self.base.clone()
        } else {
            self.opencode()
        };
        let (json, jsonc) = (dir.join("opencode.json"), dir.join("opencode.jsonc"));
        if !json.exists() && jsonc.exists() {
            jsonc
        } else {
            json
        }
    }

    /// Pi's or omp's agent directory.
    fn agent_dir(&self) -> PathBuf {
        match self.app {
            App::Pi => {
                env_path("PI_CODING_AGENT_DIR").unwrap_or_else(|| self.base.join(".pi/agent"))
            }
            _ => self.base.join(".omp/agent"),
        }
    }

    fn skill_dir(&self) -> PathBuf {
        match self.app {
            App::Claude => plugin_dir(&self.base).join("plugins/rgit/skills/rgit"),
            App::Codex => self.base.join(".agents/skills/rgit"),
            App::Opencode => self.opencode().join("skills/rgit"),
            App::Pi | App::Omp | App::All => self.agent_dir().join("skills/rgit"),
        }
    }

    fn extension(&self) -> PathBuf {
        self.agent_dir().join("extensions/rgit.ts")
    }

    fn opencode_plugin(&self) -> PathBuf {
        self.opencode().join("plugins/rgit.js")
    }

    fn codex_hooks(&self) -> PathBuf {
        self.codex().join("hooks.json")
    }

    fn codex_config(&self) -> PathBuf {
        self.codex().join("config.toml")
    }
}

/// Whether `app` looks installed: its program on PATH or its config folder.
fn detected(app: App, home: &Path) -> bool {
    let on_path = std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| dir.join(app.name()).is_file())
    });
    let place = Place {
        app,
        project: false,
        base: home.to_path_buf(),
    };
    on_path
        || match app {
            App::Claude => home.join(".claude").is_dir(),
            App::Codex => place.codex().is_dir(),
            App::Opencode => place.opencode().is_dir(),
            App::Pi => home.join(".pi").is_dir(),
            App::Omp | App::All => home.join(".omp").is_dir(),
        }
}

/// The apps to act on: the named ones, or else every installed one.
fn resolve(apps: &[App], project: bool, home: &Path, default_all: bool) -> Result<Vec<App>> {
    let mut out = Vec::new();
    let named = if apps.contains(&App::All) {
        APPS.to_vec()
    } else {
        apps.to_vec()
    };
    for app in named {
        if !out.contains(&app) {
            out.push(app);
        }
    }
    if project {
        if let Some(bad) = out.iter().find(|a| !a.has_project_scope())
            && !apps.contains(&App::All)
        {
            return Err(CliError::usage(format!(
                "{bad} reads rgit's files per user only; drop --project, which only codex and opencode support"
            )));
        }
        out.retain(|a| a.has_project_scope());
    }
    if !out.is_empty() {
        return Ok(out);
    }
    let pool = APPS
        .into_iter()
        .filter(|a| !project || a.has_project_scope());
    let found: Vec<App> = if default_all {
        pool.collect()
    } else {
        pool.filter(|a| detected(*a, home)).collect()
    };
    if found.is_empty() {
        bail!(
            "no agent apps found (claude, codex, opencode, pi or omp); name the ones to set up, e.g. `rgit agent install claude`"
        );
    }
    Ok(found)
}

/// What hooks, plugins and extensions run: bare `rgit` when PATH resolves to
/// this executable, else its absolute path, so they never run another rgit.
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

/// The session-start command a hook runs.
fn hook_command(bin: &str) -> String {
    format!("{} hook session-start", shell_quote(bin))
}

// Plan

#[derive(Debug, Clone, PartialEq)]
enum Step {
    WriteSkill {
        dir: PathBuf,
    },
    RemoveSkill {
        dir: PathBuf,
    },
    /// Write the Claude Code marketplace with the plugin holding `parts`.
    WritePlugin {
        dir: PathBuf,
        bin: String,
        parts: Parts,
    },
    /// Make Claude Code run the plugin: add the marketplace, then install or
    /// update the plugin to `version`.
    ClaudePlugin {
        dir: PathBuf,
        program: String,
        version: String,
    },
    RemovePlugin {
        dir: PathBuf,
        program: String,
    },
    /// Point the rgit SessionStart hook in a settings file at `bin`.
    AddHook {
        file: PathBuf,
        bin: String,
    },
    /// Remove every rgit SessionStart hook from a settings file.
    RemoveHook {
        file: PathBuf,
    },
    /// Codex: turn `[features].hooks` back on when the user turned it off.
    EnableCodexHooks {
        file: PathBuf,
    },
    /// A whole file rgit generates, marked on its first line.
    WriteFile {
        file: PathBuf,
        text: String,
    },
    RemoveFile {
        file: PathBuf,
    },
    AddMcpJson {
        file: PathBuf,
        key: &'static str,
        entry: Value,
    },
    RemoveMcpJson {
        file: PathBuf,
        key: &'static str,
    },
    AddMcpToml {
        file: PathBuf,
        bin: String,
    },
    RemoveMcpToml {
        file: PathBuf,
    },
}

impl Step {
    fn path(&self) -> PathBuf {
        match self {
            Step::ClaudePlugin { .. } => claude_plugin_path(),
            Step::WriteSkill { dir } | Step::RemoveSkill { dir } => dir.join("SKILL.md"),
            Step::WritePlugin { dir, .. } | Step::RemovePlugin { dir, .. } => dir.clone(),
            Step::AddHook { file, .. }
            | Step::RemoveHook { file }
            | Step::EnableCodexHooks { file }
            | Step::WriteFile { file, .. }
            | Step::RemoveFile { file }
            | Step::AddMcpJson { file, .. }
            | Step::RemoveMcpJson { file, .. }
            | Step::AddMcpToml { file, .. }
            | Step::RemoveMcpToml { file } => file.clone(),
        }
    }

    /// A clean-up of something that may never have been there; its report
    /// line shows only when it changed something.
    fn quiet(&self) -> bool {
        matches!(
            self,
            Step::RemoveSkill { .. }
                | Step::RemoveHook { .. }
                | Step::RemoveFile { .. }
                | Step::RemoveMcpJson { .. }
                | Step::RemoveMcpToml { .. }
        )
    }
}

/// The steps that leave `place` with `want` of its parts. Reads only what
/// the harness has now (to keep an MCP part the request does not name) and
/// writes nothing.
fn plan(place: &Place, want: Parts, bin: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    let skill = |steps: &mut Vec<Step>, dir: PathBuf| {
        steps.push(if want.skill {
            Step::WriteSkill { dir }
        } else {
            Step::RemoveSkill { dir }
        });
    };
    match place.app {
        App::Claude => {
            let (dir, program) = (plugin_dir(&place.base), claude_program());
            if want.any() {
                steps.push(Step::WritePlugin {
                    dir: dir.clone(),
                    bin: bin.to_owned(),
                    parts: want,
                });
                steps.push(Step::ClaudePlugin {
                    dir,
                    program,
                    version: plugin_files(bin, want).1,
                });
            } else {
                steps.push(Step::RemovePlugin { dir, program });
            }
            // What rgit wrote before it had a plugin.
            steps.push(Step::RemoveHook {
                file: place.base.join(".claude/settings.json"),
            });
            if let Ok(cwd) = std::env::current_dir()
                && cwd != place.base
            {
                steps.push(Step::RemoveHook {
                    file: cwd.join(".claude/settings.json"),
                });
            }
            steps.push(Step::RemoveSkill {
                dir: place.base.join(".claude/skills/rgit"),
            });
        }
        App::Pi | App::Omp => {
            skill(&mut steps, place.skill_dir());
            let file = place.extension();
            steps.push(if want.hook || want.mcp {
                Step::WriteFile {
                    file,
                    text: extension(place.app, bin, want),
                }
            } else {
                Step::RemoveFile { file }
            });
        }
        App::Codex => {
            skill(&mut steps, place.skill_dir());
            let file = place.codex_hooks();
            if want.hook {
                steps.push(Step::AddHook {
                    file,
                    bin: bin.to_owned(),
                });
                steps.push(Step::EnableCodexHooks {
                    file: place.codex_config(),
                });
            } else {
                steps.push(Step::RemoveHook { file });
            }
            let file = place.codex_config();
            steps.push(if want.mcp {
                Step::AddMcpToml {
                    file,
                    bin: bin.to_owned(),
                }
            } else {
                Step::RemoveMcpToml { file }
            });
        }
        App::Opencode => {
            skill(&mut steps, place.skill_dir());
            let file = place.opencode_plugin();
            steps.push(if want.hook {
                Step::WriteFile {
                    file,
                    text: opencode_plugin(bin),
                }
            } else {
                Step::RemoveFile { file }
            });
            let file = place.opencode_config();
            steps.push(if want.mcp {
                Step::AddMcpJson {
                    file,
                    key: "mcp",
                    entry: json!({ "type": "local", "command": [bin, "mcp"], "enabled": true }),
                }
            } else {
                Step::RemoveMcpJson { file, key: "mcp" }
            });
        }
        App::All => {}
    }
    steps
}

/// The parts `place` has now, from the files rgit wrote.
fn current_parts(place: &Place, bin: &str) -> Parts {
    let has = |s: &str| s != "missing" && s != "unsupported";
    let [skill, hook, mcp] = part_states(place, bin);
    Parts {
        skill: has(skill),
        hook: has(hook),
        mcp: has(mcp),
    }
}

// Install, uninstall, status

struct Change {
    app: &'static str,
    path: PathBuf,
    effect: &'static str,
}

fn places(apps: &[App], project: bool, home: &Path) -> Result<Vec<Place>> {
    let base = if project {
        std::env::current_dir()?
    } else {
        home.to_path_buf()
    };
    Ok(apps
        .iter()
        .map(|&app| Place {
            app,
            project,
            base: base.clone(),
        })
        .collect())
}

/// Install or repair the skill and hook for `apps` (the installed ones by
/// default), and with `mcp` the MCP tools. An MCP part already there stays.
fn install(apps: &[App], project: bool, mcp: bool) -> Result<Output> {
    let home = home()?;
    let apps = resolve(apps, project, &home, false)?;
    let bin = hook_bin();
    let mut steps = Vec::new();
    for place in places(&apps, project, &home)? {
        let mcp = mcp || current_parts(&place, &bin).mcp;
        let want = Parts {
            skill: true,
            hook: true,
            mcp,
        };
        steps.extend(plan(&place, want, &bin).into_iter().map(|s| (place.app, s)));
    }
    let changes = apply(&steps)?;
    let mut notes = Vec::new();
    if changes
        .iter()
        .any(|c| c.path == claude_plugin_path() && c.effect != "unchanged")
    {
        notes.push("Restart Claude Code to load the rgit plugin".to_owned());
    }
    if changes.iter().any(|c| c.effect != "unchanged") {
        notes.push("Start a new agent session to see rgit context".to_owned());
    }
    if project && apps.contains(&App::Codex) {
        notes.push("Codex runs project hooks only after you trust them with /hooks".to_owned());
    }
    Ok(report(&steps, &changes, notes))
}

/// Remove what rgit added for `apps` (every app by default); with `only`,
/// just those parts.
fn uninstall(apps: &[App], project: bool, only: Option<Parts>) -> Result<Output> {
    let home = home()?;
    let apps = resolve(apps, project, &home, true)?;
    let bin = hook_bin();
    let mut steps = Vec::new();
    for place in places(&apps, project, &home)? {
        let want = match only {
            None => Parts::NONE,
            Some(drop) => {
                let have = current_parts(&place, &bin);
                if !(have.skill && drop.skill || have.hook && drop.hook || have.mcp && drop.mcp) {
                    continue;
                }
                Parts {
                    skill: have.skill && !drop.skill,
                    hook: have.hook && !drop.hook,
                    mcp: have.mcp && !drop.mcp,
                }
            }
        };
        steps.extend(plan(&place, want, &bin).into_iter().map(|s| (place.app, s)));
    }
    let changes = apply(&steps)?;
    Ok(report(&steps, &changes, Vec::new()))
}

/// The change lines: a clean-up step that found nothing is left out.
fn report(steps: &[(App, Step)], changes: &[Change], notes: Vec<String>) -> Output {
    let shown: Vec<&Change> = changes
        .iter()
        .filter(|c| {
            c.effect != "unchanged" || !steps.iter().any(|(_, s)| s.quiet() && s.path() == c.path)
        })
        .collect();
    let mut lines: Vec<String> = shown
        .iter()
        .map(|c| format!("{:<9} {:<8} {}", c.effect, c.app, c.path.display()))
        .collect();
    if lines.is_empty() {
        lines.push("nothing to change".to_owned());
    }
    lines.extend(notes.iter().cloned());
    let rows: Vec<Obj> = shown
        .iter()
        .map(|c| {
            obj! {
                "app" => c.app,
                "effect" => c.effect,
                "path" => c.path.display().to_string(),
            }
        })
        .collect();
    let mut out = Output::new(lines.join("\n")).list(
        "changes",
        rows,
        &["app", "effect", "path"],
        "nothing to change",
    );
    for note in notes {
        out = out.help(note);
    }
    out
}

const STATUS_COLUMNS: [&str; 6] = ["app", "scope", "plugin", "skill", "hook", "mcp"];

/// Each harness's plugin (or extension), skill, hook and MCP state, per user
/// and, for Codex and OpenCode, in this project.
fn status() -> Result<Output> {
    let home = home()?;
    let bin = hook_bin();
    let mut all = places(&APPS, false, &home)?;
    all.extend(places(&[App::Codex, App::Opencode], true, &home)?);
    let rows: Vec<[&'static str; 6]> = all
        .iter()
        .map(|p| {
            let [skill, hook, mcp] = part_states(p, &bin);
            [
                p.app.name(),
                p.scope(),
                package_state(p, &bin),
                skill,
                hook,
                mcp,
            ]
        })
        .collect();
    let mut lines: Vec<String> = std::iter::once(STATUS_COLUMNS)
        .chain(rows.iter().copied())
        .map(|r| {
            format!(
                "{:<9} {:<8} {:<12} {:<8} {:<8} {}",
                r[0], r[1], r[2], r[3], r[4], r[5]
            )
        })
        .collect();
    let data: Vec<Obj> = rows
        .iter()
        .map(|r| {
            obj! {
                "app" => r[0], "scope" => r[1], "plugin" => r[2],
                "skill" => r[3], "hook" => r[4], "mcp" => r[5],
            }
        })
        .collect();
    let hint = if rows.iter().any(|r| r[2..].contains(&"stale")) {
        Some("Run `rgit agent install` to repair what is stale")
    } else if rows.iter().all(|r| r[3] == "missing" && r[4] == "missing") {
        Some("Run `rgit agent install` to set up the agent apps on this machine")
    } else {
        None
    };
    let mut out;
    if let Some(hint) = hint {
        lines.push(hint.to_owned());
        out = Output::new(lines.join("\n")).help(hint);
    } else {
        out = Output::new(lines.join("\n"));
    }
    out = out.list("apps", data, &STATUS_COLUMNS, "0 apps");
    Ok(out)
}

/// Skill, hook and MCP state: current, stale, missing or unsupported.
fn part_states(place: &Place, bin: &str) -> [&'static str; 3] {
    let or_stale = |r: Result<&'static str>| r.unwrap_or("stale");
    match place.app {
        App::Claude => {
            let dir = plugin_dir(&place.base).join("plugins/rgit");
            let file_state = |rel: &str, want: &str| match std::fs::read_to_string(dir.join(rel)) {
                Ok(have) if have == want => "current",
                Ok(_) => "stale",
                Err(_) => "missing",
            };
            [
                skill_state(&place.skill_dir()),
                file_state("hooks/hooks.json", &claude_hooks(bin)),
                file_state(".mcp.json", &claude_mcp(bin)),
            ]
        }
        App::Pi | App::Omp => {
            let file = place.extension();
            let text = std::fs::read_to_string(&file)
                .ok()
                .filter(|t| is_managed(t));
            let parts = text.as_deref().map(extension_parts).unwrap_or_default();
            let current = text
                .as_deref()
                .is_some_and(|t| t == extension(place.app, bin, parts));
            let part = |on: bool| match (on, current) {
                (false, _) => "missing",
                (true, true) => "current",
                (true, false) => "stale",
            };
            [
                skill_state(&place.skill_dir()),
                part(parts.hook),
                part(parts.mcp),
            ]
        }
        App::Codex => {
            let hook = or_stale(json_hook_state(&place.codex_hooks(), bin).and_then(|s| {
                Ok(
                    if s == "current" && codex_hooks_off(&place.codex_config())? {
                        "stale"
                    } else {
                        s
                    },
                )
            }));
            [
                skill_state(&place.skill_dir()),
                hook,
                or_stale(mcp_toml_state(&place.codex_config(), bin)),
            ]
        }
        App::Opencode => {
            let want = json!({ "type": "local", "command": [bin, "mcp"], "enabled": true });
            [
                skill_state(&place.skill_dir()),
                managed_state(&place.opencode_plugin(), &opencode_plugin(bin)),
                or_stale(mcp_json_state(&place.opencode_config(), "mcp", &want)),
            ]
        }
        App::All => ["unsupported"; 3],
    }
}

/// The Claude Code plugin as Claude Code has it installed, or the pi/omp
/// extension file; other harnesses have none.
fn package_state(place: &Place, bin: &str) -> &'static str {
    match place.app {
        App::Claude => {
            let dir = plugin_dir(&place.base);
            let Some(have) = installed_plugin_version(&place.base) else {
                return "missing";
            };
            let parts = plugin_parts(&dir.join("plugins/rgit"));
            if have == plugin_files(bin, parts).1 {
                "current"
            } else {
                "stale"
            }
        }
        App::Pi | App::Omp => {
            let file = place.extension();
            match std::fs::read_to_string(&file) {
                Ok(t) if is_managed(&t) && t == extension(place.app, bin, extension_parts(&t)) => {
                    "current"
                }
                Ok(t) if is_managed(&t) => "stale",
                _ => "missing",
            }
        }
        _ => "unsupported",
    }
}

// Apply

/// Run the steps after checking each can run, so a missing `claude` or an
/// unreadable config file stops the whole request before any write.
fn apply(steps: &[(App, Step)]) -> Result<Vec<Change>> {
    for (_, step) in steps {
        preflight(step)?;
    }
    let mut out: Vec<Change> = Vec::new();
    for (app, step) in steps {
        let effect = run_step(step)?;
        let path = step.path();
        match out.iter_mut().find(|c| c.path == path) {
            Some(c) if c.effect == "unchanged" => c.effect = effect,
            Some(_) => {}
            None => out.push(Change {
                app: app.name(),
                path,
                effect,
            }),
        }
    }
    Ok(out)
}

fn preflight(step: &Step) -> Result<()> {
    match step {
        Step::ClaudePlugin { program, .. } if !program_found(program) => bail!(
            "installing into Claude Code needs its `{program}` command on PATH; install Claude Code, or name the other apps, e.g. `rgit agent install codex`"
        ),
        Step::AddHook { file, .. }
        | Step::RemoveHook { file }
        | Step::AddMcpJson { file, .. }
        | Step::RemoveMcpJson { file, .. } => read_json(file).map(drop),
        Step::EnableCodexHooks { file }
        | Step::AddMcpToml { file, .. }
        | Step::RemoveMcpToml { file } => read_toml(file).map(drop),
        Step::WriteFile { file, .. } => match read_text(file)? {
            Some(t) if !is_managed(&t) => bail!(
                "{} exists and is not rgit's; move it away first",
                file.display()
            ),
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

fn effect(did: bool, done: &'static str) -> &'static str {
    if did { done } else { "unchanged" }
}

fn run_step(step: &Step) -> Result<&'static str> {
    Ok(match step {
        Step::WriteSkill { dir } => effect(write_skill(dir)?, "written"),
        Step::RemoveSkill { dir } => effect(remove_skill(dir)?, "removed"),
        Step::WritePlugin { dir, bin, parts } => effect(write_plugin(dir, bin, *parts)?, "written"),
        Step::ClaudePlugin {
            dir,
            program,
            version,
        } => effect(claude_plugin(dir, program, version)?, "written"),
        Step::RemovePlugin { dir, program } => effect(remove_plugin(dir, program)?, "removed"),
        Step::AddHook { file, bin } => effect(add_hook(file, bin)?, "written"),
        Step::RemoveHook { file } => effect(remove_hook(file)?, "written"),
        Step::EnableCodexHooks { file } => effect(enable_codex_hooks(file)?, "written"),
        Step::WriteFile { file, text } => effect(write_managed(file, text)?, "written"),
        Step::RemoveFile { file } => effect(remove_managed(file)?, "removed"),
        Step::AddMcpJson { file, key, entry } => effect(add_mcp_json(file, key, entry)?, "written"),
        Step::RemoveMcpJson { file, key } => effect(remove_mcp_json(file, key)?, "written"),
        Step::AddMcpToml { file, bin } => effect(add_mcp_toml(file, bin)?, "written"),
        Step::RemoveMcpToml { file } => effect(remove_mcp_toml(file)?, "written"),
    })
}

fn read_text(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write via a temporary file and a rename, replacing (never following) a
/// link at `path`.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().expect("an absolute path");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().expect("a file").to_string_lossy();
    let tmp = dir.join(format!(".{name}.rgit-tmp"));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

fn remove_file(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

// Skill

/// The skill's files, relative to its directory.
fn skill_files() -> [(&'static str, String); 2] {
    [
        ("SKILL.md", crate::cli::skill_markdown()),
        ("references/commands.md", crate::cli::skill_reference()),
    ]
}

/// Whether `text` is an rgit SKILL.md, of this version or an older one.
fn is_rgit_skill(text: &str) -> bool {
    text.starts_with("---\nname: rgit\n")
}

fn skill_state(dir: &Path) -> &'static str {
    let files = skill_files();
    if files
        .iter()
        .all(|(name, want)| std::fs::read_to_string(dir.join(name)).is_ok_and(|s| &s == want))
    {
        "current"
    } else if dir.join("SKILL.md").exists() {
        "stale"
    } else {
        "missing"
    }
}

fn write_skill(dir: &Path) -> Result<bool> {
    // A link is replaced, never written through, so a folder elsewhere stays.
    if dir
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        std::fs::remove_file(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    let mut changed = false;
    for (name, text) in skill_files() {
        let file = dir.join(name);
        if std::fs::read_to_string(&file).is_ok_and(|t| t == text) {
            continue;
        }
        write_atomic(&file, &text)?;
        changed = true;
    }
    Ok(changed)
}

/// Remove rgit's skill files; a SKILL.md that is not rgit's, or a link,
/// stays. Other files in the folder stay too.
fn remove_skill(dir: &Path) -> Result<bool> {
    if dir
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Ok(false);
    }
    let Some(text) = read_text(&dir.join("SKILL.md"))? else {
        return Ok(false);
    };
    if !is_rgit_skill(&text) {
        return Ok(false);
    }
    for (name, _) in skill_files() {
        remove_file(&dir.join(name))?;
    }
    let _ = std::fs::remove_dir(dir.join("references"));
    let _ = std::fs::remove_dir(dir);
    Ok(true)
}

// Claude Code plugin

const CLAUDE_MATCHER: &str = "startup|resume|clear|compact";

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("json serializes") + "\n"
}

fn claude_hooks(bin: &str) -> String {
    pretty(&json!({ "hooks": { "SessionStart": [{
        "matcher": CLAUDE_MATCHER,
        "hooks": [{ "type": "command", "command": hook_command(bin), "timeout": 10 }],
    }] } }))
}

fn claude_mcp(bin: &str) -> String {
    pretty(&json!({ "mcpServers": { "rgit": { "command": bin, "args": ["mcp"] } } }))
}

/// The optional plugin files and the part each carries, relative to the
/// plugin folder.
const PLUGIN_PART_FILES: [&str; 4] = [
    "skills/rgit/SKILL.md",
    "skills/rgit/references/commands.md",
    "hooks/hooks.json",
    ".mcp.json",
];

/// The marketplace files, relative to `plugin_dir`, and the plugin version,
/// which carries a hash of the content so `claude plugin update` picks up a
/// changed skill, binary path or part.
fn plugin_files(bin: &str, parts: Parts) -> (Vec<(String, String)>, String) {
    use sha2::{Digest, Sha256};
    let mut files = Vec::new();
    if parts.skill {
        for (name, text) in skill_files() {
            files.push((format!("plugins/rgit/skills/rgit/{name}"), text));
        }
    }
    if parts.hook {
        files.push(("plugins/rgit/hooks/hooks.json".into(), claude_hooks(bin)));
    }
    if parts.mcp {
        files.push(("plugins/rgit/.mcp.json".into(), claude_mcp(bin)));
    }
    let mut h = Sha256::new();
    for (path, text) in &files {
        h.update(path.as_bytes());
        h.update(text.as_bytes());
    }
    let hash: String = h
        .finalize()
        .iter()
        .take(3)
        .map(|b| format!("{b:02x}"))
        .collect();
    let version = format!("{}+{hash}", env!("CARGO_PKG_VERSION"));
    let about = "rgit: git state at session start, the rgit skill, and optionally rgit's MCP tools";
    let author = json!({ "name": "rgit" });
    files.push((
        ".claude-plugin/marketplace.json".into(),
        pretty(&json!({
            "name": "rgit",
            "owner": author,
            "metadata": { "description": "The rgit plugin, written by `rgit agent install claude`" },
            "plugins": [{ "name": "rgit", "source": "./plugins/rgit", "description": about }],
        })),
    ));
    files.push((
        "plugins/rgit/.claude-plugin/plugin.json".into(),
        pretty(
            &json!({ "name": "rgit", "version": version, "description": about, "author": author }),
        ),
    ));
    (files, version)
}

/// The parts the plugin folder holds now.
fn plugin_parts(dir: &Path) -> Parts {
    Parts {
        skill: dir.join("skills/rgit/SKILL.md").exists(),
        hook: dir.join("hooks/hooks.json").exists(),
        mcp: dir.join(".mcp.json").exists(),
    }
}

/// The version of the rgit plugin Claude Code has installed.
fn installed_plugin_version(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join(".claude/plugins/installed_plugins.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v["plugins"][PLUGIN].as_array()?.first()?["version"]
        .as_str()
        .map(str::to_owned)
}

fn write_plugin(dir: &Path, bin: &str, parts: Parts) -> Result<bool> {
    let (files, _) = plugin_files(bin, parts);
    let mut changed = false;
    for rel in PLUGIN_PART_FILES {
        let rel = format!("plugins/rgit/{rel}");
        if !files.iter().any(|(p, _)| *p == rel) {
            changed |= remove_file(&dir.join(&rel))?;
        }
    }
    for (rel, text) in files {
        let file = dir.join(rel);
        if std::fs::read_to_string(&file).is_ok_and(|t| t == text) {
            continue;
        }
        write_atomic(&file, &text)?;
        changed = true;
    }
    Ok(changed)
}

fn run_claude(program: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("cannot run `{program}`"))?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr).into_owned()
            + &String::from_utf8_lossy(&out.stdout);
        let tail: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .rev()
            .take(3)
            .collect();
        bail!(
            "`claude {}` failed: {}",
            args.join(" "),
            tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn program_found(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

fn marketplace_known(program: &str) -> Result<bool> {
    let v: Value = serde_json::from_str(&run_claude(
        program,
        &["plugin", "marketplace", "list", "--json"],
    )?)
    .unwrap_or_default();
    Ok(v.as_array()
        .is_some_and(|a| a.iter().any(|m| m["name"] == "rgit")))
}

fn plugin_version(program: &str) -> Result<Option<String>> {
    let v: Value = serde_json::from_str(&run_claude(program, &["plugin", "list", "--json"])?)
        .unwrap_or_default();
    Ok(v.as_array()
        .and_then(|a| a.iter().find(|p| p["id"] == PLUGIN))
        .and_then(|p| p["version"].as_str())
        .map(str::to_owned))
}

/// Add the marketplace in `dir`, then install or update the plugin to
/// `want`.
fn claude_plugin(dir: &Path, program: &str, want: &str) -> Result<bool> {
    let mut changed = false;
    if !marketplace_known(program)? {
        run_claude(
            program,
            &["plugin", "marketplace", "add", &dir.display().to_string()],
        )?;
        changed = true;
    }
    match plugin_version(program)? {
        None => {
            run_claude(program, &["plugin", "install", PLUGIN])?;
            changed = true;
        }
        Some(v) if v != want => {
            run_claude(program, &["plugin", "marketplace", "update", "rgit"])?;
            run_claude(program, &["plugin", "update", PLUGIN])?;
            changed = true;
        }
        Some(_) => {}
    }
    Ok(changed)
}

fn remove_plugin(dir: &Path, program: &str) -> Result<bool> {
    let mut changed = false;
    if program_found(program) {
        if plugin_version(program)?.is_some() {
            run_claude(program, &["plugin", "uninstall", PLUGIN])?;
            changed = true;
        }
        if marketplace_known(program)? {
            run_claude(program, &["plugin", "marketplace", "remove", "rgit"])?;
            changed = true;
        }
    }
    let ours = std::fs::read_to_string(dir.join(".claude-plugin/marketplace.json"))
        .is_ok_and(|t| t.contains("\"./plugins/rgit\""));
    if ours {
        std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
        changed = true;
    }
    Ok(changed)
}

// SessionStart hook in a JSON settings file (Codex; Claude Code before the plugin)

/// Whether `cmd` runs rgit's session start: `rgit hook session-start`, or
/// the `rgit --toon` (earlier `rgit --porcelain`) that preceded it.
fn is_rgit_command(cmd: &str) -> bool {
    let cmd = cmd.trim();
    let Some(bin) = [" hook session-start", " --toon", " --porcelain"]
        .iter()
        .find_map(|s| cmd.strip_suffix(s))
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

/// A JSON config file as an object that keeps its key order.
fn read_json(path: &Path) -> Result<Map<String, Value>> {
    let Some(text) = read_text(path)? else {
        return Ok(Map::new());
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => bail!("{} is not a JSON object", path.display()),
        Err(_) => bail!(
            "{} is not plain JSON (comments?); rgit edits only plain JSON, so make the change by hand",
            path.display()
        ),
    }
}

/// Write `root` back, or remove the file when rgit's removal left it empty.
fn write_json(path: &Path, root: &Map<String, Value>) -> Result<()> {
    if root.is_empty() {
        remove_file(path)?;
        return Ok(());
    }
    write_atomic(path, &pretty(&Value::Object(root.clone())))
}

fn rgit_hooks(root: &Map<String, Value>) -> Vec<String> {
    root.get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(|h| h.get("command").and_then(Value::as_str))
        .filter(|c| is_rgit_command(c))
        .map(str::to_owned)
        .collect()
}

fn json_hook_state(path: &Path, bin: &str) -> Result<&'static str> {
    Ok(match rgit_hooks(&read_json(path)?).as_slice() {
        [] => "missing",
        [one] if *one == hook_command(bin) => "current",
        _ => "stale",
    })
}

/// Keep the first rgit hook, pointed at `want` (when given), drop the rest
/// and any group left empty. Returns (kept one, changed anything).
fn retain_rgit_hook(groups: &mut Vec<Value>, want: Option<&str>) -> (bool, bool) {
    let mut seen = false;
    let mut changed = false;
    for group in groups.iter_mut() {
        let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        let before = list.len();
        list.retain_mut(|h| {
            let Some(cmd) = h.get("command").and_then(Value::as_str) else {
                return true;
            };
            if !is_rgit_command(cmd) {
                return true;
            }
            let Some(want) = want.filter(|_| !seen) else {
                return false;
            };
            seen = true;
            if cmd != want {
                h["command"] = Value::from(want);
                changed = true;
            }
            true
        });
        changed |= list.len() != before;
    }
    let before = groups.len();
    groups.retain(|g| {
        g.get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|l| !l.is_empty())
    });
    (seen, changed || groups.len() != before)
}

fn add_hook(path: &Path, bin: &str) -> Result<bool> {
    let mut root = read_json(path)?;
    let want = hook_command(bin);
    let groups = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("{}: `hooks` is not an object", path.display()))?
        .entry("SessionStart")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow!("{}: `hooks.SessionStart` is not a list", path.display()))?;
    match retain_rgit_hook(groups, Some(&want)) {
        (false, _) => groups.push(json!({
            "matcher": CLAUDE_MATCHER,
            "hooks": [{ "type": "command", "command": want }],
        })),
        (true, true) => {}
        (true, false) => return Ok(false),
    }
    write_json(path, &root)?;
    Ok(true)
}

fn remove_hook(path: &Path) -> Result<bool> {
    let mut root = read_json(path)?;
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    let Some(groups) = hooks.get_mut("SessionStart").and_then(Value::as_array_mut) else {
        return Ok(false);
    };
    if !retain_rgit_hook(groups, None).1 {
        return Ok(false);
    }
    if groups.is_empty() {
        hooks.shift_remove("SessionStart");
    }
    if hooks.is_empty() {
        root.shift_remove("hooks");
    }
    write_json(path, &root)?;
    Ok(true)
}

// Codex config.toml

fn read_toml(path: &Path) -> Result<toml_edit::DocumentMut> {
    read_text(path)?
        .unwrap_or_default()
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))
}

fn write_toml(path: &Path, doc: &toml_edit::DocumentMut) -> Result<()> {
    let text = doc.to_string();
    if text.trim().is_empty() {
        remove_file(path)?;
        return Ok(());
    }
    write_atomic(path, &text)
}

/// Codex runs hooks unless `[features].hooks` (or its old name) is false.
fn codex_hooks_off(path: &Path) -> Result<bool> {
    let doc = read_toml(path)?;
    let flag = |k: &str| doc.get("features")?.get(k)?.as_bool();
    Ok(flag("hooks").or(flag("codex_hooks")) == Some(false))
}

fn enable_codex_hooks(path: &Path) -> Result<bool> {
    if !codex_hooks_off(path)? {
        return Ok(false);
    }
    let mut doc = read_toml(path)?;
    let features = doc["features"]
        .as_table_like_mut()
        .ok_or_else(|| anyhow!("{}: `features` is not a table", path.display()))?;
    features.remove("codex_hooks");
    features.insert("hooks", toml_edit::value(true));
    write_toml(path, &doc)?;
    Ok(true)
}

fn mcp_toml_state(path: &Path, bin: &str) -> Result<&'static str> {
    let doc = read_toml(path)?;
    let Some(server) = doc.get("mcp_servers").and_then(|s| s.get("rgit")) else {
        return Ok("missing");
    };
    let command = server.get("command").and_then(|c| c.as_str());
    let args: Option<Vec<&str>> = server
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect());
    Ok(
        if command == Some(bin) && args.as_deref() == Some(&["mcp"]) {
            "current"
        } else {
            "stale"
        },
    )
}

fn add_mcp_toml(path: &Path, bin: &str) -> Result<bool> {
    if mcp_toml_state(path, bin)? == "current" {
        return Ok(false);
    }
    let mut doc = read_toml(path)?;
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        })
        .as_table_like_mut()
        .ok_or_else(|| anyhow!("{}: `mcp_servers` is not a table", path.display()))?;
    let server = servers
        .entry("rgit")
        .or_insert(toml_edit::table())
        .as_table_like_mut()
        .ok_or_else(|| anyhow!("{}: `mcp_servers.rgit` is not a table", path.display()))?;
    server.insert("command", toml_edit::value(bin));
    server.insert(
        "args",
        toml_edit::value(toml_edit::Array::from_iter(["mcp"])),
    );
    write_toml(path, &doc)?;
    Ok(true)
}

fn remove_mcp_toml(path: &Path) -> Result<bool> {
    let mut doc = read_toml(path)?;
    let Some(servers) = doc
        .get_mut("mcp_servers")
        .and_then(|s| s.as_table_like_mut())
    else {
        return Ok(false);
    };
    if servers.remove("rgit").is_none() {
        return Ok(false);
    }
    if servers.is_empty() {
        doc.remove("mcp_servers");
    }
    write_toml(path, &doc)?;
    Ok(true)
}

// OpenCode mcp entry

/// Current when every key rgit sets has rgit's value; other keys (say
/// `environment`) are the user's.
fn covers(have: &Value, want: &Value) -> bool {
    want.as_object()
        .is_some_and(|w| w.iter().all(|(k, v)| have.get(k) == Some(v)))
}

fn mcp_json_state(path: &Path, key: &str, want: &Value) -> Result<&'static str> {
    Ok(
        match read_json(path)?.get(key).and_then(|s| s.get("rgit")) {
            None => "missing",
            Some(have) if covers(have, want) => "current",
            Some(_) => "stale",
        },
    )
}

fn add_mcp_json(path: &Path, key: &str, want: &Value) -> Result<bool> {
    let mut root = read_json(path)?;
    let servers = root
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("{}: `{key}` is not an object", path.display()))?;
    match servers.get_mut("rgit") {
        Some(have) if covers(have, want) => return Ok(false),
        Some(Value::Object(have)) => {
            for (k, v) in want.as_object().expect("an object") {
                have.insert(k.clone(), v.clone());
            }
        }
        _ => {
            servers.insert("rgit".to_owned(), want.clone());
        }
    }
    write_json(path, &root)?;
    Ok(true)
}

fn remove_mcp_json(path: &Path, key: &str) -> Result<bool> {
    let mut root = read_json(path)?;
    let Some(servers) = root.get_mut(key).and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    if servers.shift_remove("rgit").is_none() {
        return Ok(false);
    }
    if servers.is_empty() {
        root.shift_remove(key);
    }
    write_json(path, &root)?;
    Ok(true)
}

// Generated files: the OpenCode plugin and the pi/omp extension

/// The first line of every file rgit generates; uninstall removes only these.
const MARKER: &str = "// Generated by rgit agent install;";

fn is_managed(text: &str) -> bool {
    // Earlier rgit marked its OpenCode plugin "Managed by `rgit hooks install`".
    text.starts_with(MARKER) || text.starts_with("// Managed by `rgit ")
}

fn managed_state(path: &Path, want: &str) -> &'static str {
    match std::fs::read_to_string(path) {
        Ok(s) if s == want => "current",
        Ok(s) if is_managed(&s) => "stale",
        _ => "missing",
    }
}

fn write_managed(file: &Path, text: &str) -> Result<bool> {
    if file
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        std::fs::remove_file(file).with_context(|| format!("removing {}", file.display()))?;
    }
    if std::fs::read_to_string(file).is_ok_and(|t| t == text) {
        return Ok(false);
    }
    write_atomic(file, text)?;
    Ok(true)
}

fn remove_managed(file: &Path) -> Result<bool> {
    let ours = file.symlink_metadata().is_ok_and(|m| m.is_file())
        && std::fs::read_to_string(file).is_ok_and(|t| is_managed(&t));
    if !ours {
        return Ok(false);
    }
    remove_file(file)
}

/// An OpenCode plugin that adds rgit's session start text to each new
/// session as a context-only message.
fn opencode_plugin(bin: &str) -> String {
    let bin = serde_json::to_string(bin).expect("a string serializes");
    format!(
        r#"{MARKER} changes are overwritten. Remove with: rgit agent uninstall opencode
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
        const text = (await $`${{BIN}} hook session-start`.cwd(cwd).quiet().nothrow().text()).trim();
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

/// The pi and omp extension template.
const EXTENSION: &str = include_str!("../../../extensions/rgit/src/rgit.ts");

/// The extension for `app` running `bin`: the session-start hook when
/// `parts.hook`, and rgit's MCP tools as native tools when `parts.mcp`.
fn extension(app: App, bin: &str, parts: Parts) -> String {
    let string = |s: &str| serde_json::to_string(s).expect("a string serializes");
    let tools = if parts.mcp {
        serde_json::to_string(&crate::mcp::definitions()).expect("definitions serialize")
    } else {
        String::new()
    };
    EXTENSION
        .replace("__HARNESS__", app.name())
        .replace("\"__RGIT__\"", &string(bin))
        .replace(
            "\"__SESSION_START__\"",
            &string(if parts.hook { "yes" } else { "no" }),
        )
        .replace("\"__TOOLS__\"", &string(&tools))
}

/// The parts a generated extension carries.
fn extension_parts(text: &str) -> Parts {
    Parts {
        skill: false,
        hook: text.contains("const SESSION_START: string = \"yes\""),
        mcp: text.contains("const TOOLS_JSON: string = \"["),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rgit-agent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn repairs_old_hook_commands() {
        let path = tempdir("migrate").join("hooks.json");
        std::fs::write(
            &path,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"rgit --porcelain"},{"type":"command","command":"/x/rgit --toon"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(json_hook_state(&path, "rgit").unwrap(), "stale");
        assert!(add_hook(&path, "rgit").unwrap());
        assert_eq!(
            rgit_hooks(&read_json(&path).unwrap()),
            ["rgit hook session-start"]
        );
        assert!(!add_hook(&path, "rgit").unwrap());
        assert!(add_hook(&path, "/new dir/rgit").unwrap());
        assert_eq!(
            rgit_hooks(&read_json(&path).unwrap()),
            ["'/new dir/rgit' hook session-start"]
        );
        assert!(remove_hook(&path).unwrap());
        assert!(!path.exists(), "nothing but rgit's hook was there");
    }

    #[test]
    fn codex_hook_flag_is_set_only_when_off() {
        let config = tempdir("codex").join("config.toml");
        assert!(!enable_codex_hooks(&config).unwrap());
        assert!(!config.exists());
        std::fs::write(
            &config,
            "# mine\n[features]\ncodex_hooks = false\nweb = true\n",
        )
        .unwrap();
        assert!(enable_codex_hooks(&config).unwrap());
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.starts_with("# mine\n[features]\n"), "{text}");
        assert!(
            text.contains("web = true") && text.contains("hooks = true"),
            "{text}"
        );
        assert!(!codex_hooks_off(&config).unwrap());
    }

    #[test]
    fn plugin_version_follows_the_content() {
        let all = Parts {
            skill: true,
            hook: true,
            mcp: true,
        };
        let no_mcp = Parts { mcp: false, ..all };
        let (files, version) = plugin_files("rgit", all);
        assert!(version.starts_with(env!("CARGO_PKG_VERSION")));
        assert_eq!(version.len(), env!("CARGO_PKG_VERSION").len() + 7);
        assert!(files.iter().any(|(p, _)| p == "plugins/rgit/.mcp.json"));
        assert!(
            !plugin_files("rgit", no_mcp)
                .0
                .iter()
                .any(|(p, _)| p.ends_with(".mcp.json"))
        );
        assert_ne!(plugin_files("rgit", no_mcp).1, version);
        assert_ne!(plugin_files("/new/rgit", all).1, version);
        let hooks: Value = serde_json::from_str(&claude_hooks("rgit")).unwrap();
        assert_eq!(
            hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "rgit hook session-start"
        );
    }

    #[test]
    fn extension_fills_every_placeholder() {
        let parts = Parts {
            skill: true,
            hook: true,
            mcp: true,
        };
        let text = extension(App::Omp, "/x/rgit", parts);
        assert!(text.starts_with(MARKER));
        assert!(!text.contains("__"), "a placeholder is left");
        assert!(text.contains("const RGIT: string = \"/x/rgit\";"));
        assert_eq!(
            extension_parts(&text),
            Parts {
                skill: false,
                ..parts
            }
        );
        let bare = extension(
            App::Pi,
            "rgit",
            Parts {
                mcp: false,
                ..parts
            },
        );
        assert!(bare.contains("const TOOLS_JSON: string = \"\";"));
        assert!(!extension_parts(&bare).mcp);
        assert!(opencode_plugin("rgit").starts_with(MARKER));
    }

    /// Runs only where Claude Code's `claude` exists; `validate` only reads.
    #[test]
    fn rendered_plugin_validates() {
        if !program_found("claude") {
            return;
        }
        let dir = tempdir("validate");
        let all = Parts {
            skill: true,
            hook: true,
            mcp: true,
        };
        write_plugin(&dir, "rgit", all).unwrap();
        for target in [dir.clone(), dir.join("plugins/rgit")] {
            let out = std::process::Command::new("claude")
                .args(["plugin", "validate"])
                .arg(&target)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success() && !text.contains("warning"), "{text}");
        }
    }

    #[test]
    fn unit_tests_never_reach_claude_code() {
        assert!(!program_found(&claude_program()));
    }
}
