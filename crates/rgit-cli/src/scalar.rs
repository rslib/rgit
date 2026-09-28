//! `rgit scalar`: git's `scalar` tool for large repositories. It sets
//! scalar's recommended config, keeps the global `scalar.repo` list and
//! drives rgit's native maintenance.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rgit_git::{ConfigScope, Git2Backend, GitBackend, SetMode};

use crate::cli::{MaintenanceCmd, ScalarCmd};

/// scalar's config: `(key, value, overwritten by reconfigure)`.
const CONFIG: &[(&str, &str, bool)] = &[
    ("am.keepCR", "true", true),
    ("core.untrackedCache", "true", true),
    ("core.logAllRefUpdates", "true", true),
    ("credential.https://dev.azure.com.useHttpPath", "true", true),
    ("gc.auto", "0", true),
    ("gui.GCWarning", "false", true),
    ("index.skipHash", "false", true),
    ("index.threads", "true", true),
    ("index.version", "4", true),
    ("merge.stat", "false", true),
    ("merge.renames", "true", true),
    ("pack.useBitmaps", "false", true),
    ("receive.autoGC", "false", true),
    ("feature.manyFiles", "false", true),
    ("feature.experimental", "false", true),
    ("fetch.unpackLimit", "1", true),
    ("fetch.writeCommitGraph", "false", true),
    ("status.aheadBehind", "false", false),
    ("commitGraph.changedPaths", "true", false),
    ("commitGraph.generationVersion", "1", false),
    ("core.autoCRLF", "false", false),
    ("core.safeCRLF", "false", false),
    ("fetch.showForcedUpdates", "false", false),
    ("pack.usePathWalk", "true", false),
];

/// The repository of an enlistment: `<dir>/src` when it has one, else the
/// repository at or above `dir` (the current folder by default).
fn open(enlistment: Option<&str>) -> anyhow::Result<Arc<dyn GitBackend>> {
    let dir = PathBuf::from(enlistment.unwrap_or("."));
    let src = dir.join("src");
    let start = if src.join(".git").exists() { src } else { dir };
    Ok(Arc::new(Git2Backend::open_env(&start)?))
}

fn root(backend: &Arc<dyn GitBackend>) -> anyhow::Result<String> {
    Ok(backend.workdir().canonicalize()?.display().to_string())
}

fn repos() -> Vec<String> {
    rgit_git::config_list(None, &ConfigScope::Global, false)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.name == "scalar.repo")
        .filter_map(|e| e.value)
        .collect()
}

fn set_config(backend: &Arc<dyn GitBackend>, reconfigure: bool) -> anyhow::Result<()> {
    let git_dir = backend.git_dir();
    let set = |k: &str, v: &str, mode| {
        rgit_git::config_set(Some(&git_dir), &ConfigScope::Local, k, v, None, mode)
    };
    for (key, value, overwrite) in CONFIG {
        if (reconfigure && *overwrite) || backend.config_get(key)?.is_none() {
            set(key, value, SetMode::Replace)?;
        }
    }
    let decorations = rgit_git::config_list(Some(&git_dir), &ConfigScope::Any, true)?;
    if !decorations
        .iter()
        .any(|e| e.name == "log.excludedecoration" && e.value.as_deref() == Some("refs/prefetch/*"))
    {
        set("log.excludeDecoration", "refs/prefetch/*", SetMode::Add)?;
    }
    Ok(())
}

fn add_repo(path: &str) -> anyhow::Result<()> {
    if !repos().iter().any(|r| r == path) {
        rgit_git::config_set(
            None,
            &ConfigScope::Global,
            "scalar.repo",
            path,
            None,
            SetMode::Add,
        )?;
    }
    Ok(())
}

fn remove_repo(path: &str) -> anyhow::Result<()> {
    if repos().iter().any(|r| r == path) {
        rgit_git::config_unset(
            None,
            &ConfigScope::Global,
            "scalar.repo",
            Some(&rgit_git::config_fixed_value(path)),
            true,
        )?;
    }
    Ok(())
}

fn maintenance(backend: &Arc<dyn GitBackend>, on: bool) -> anyhow::Result<()> {
    let cmd = if on {
        MaintenanceCmd::Start { scheduler: None }
    } else {
        MaintenanceCmd::Unregister { force: true }
    };
    crate::maintenance::run(backend, cmd).map(|_| ())
}

fn register(backend: &Arc<dyn GitBackend>, with_maintenance: bool) -> anyhow::Result<()> {
    set_config(backend, false)?;
    if with_maintenance && let Err(e) = maintenance(backend, true) {
        eprintln!("warning: could not turn on maintenance: {e}");
    }
    add_repo(&root(backend)?)
}

fn run_task(backend: &Arc<dyn GitBackend>, task: &str) -> anyhow::Result<String> {
    let native = match task {
        "config" => return register(backend, false).map(|()| String::new()),
        "commit-graph" | "loose-objects" => task,
        "fetch" => "prefetch",
        "pack-files" => "incremental-repack",
        _ => anyhow::bail!(
            "no such task: '{task}' (available: all, config, commit-graph, fetch, \
             loose-objects, pack-files)"
        ),
    };
    Ok(backend.maintenance_run(&rgit_git::MaintenanceRun {
        tasks: vec![native.to_owned()],
        auto: false,
        schedule: None,
        quiet: false,
    })?)
}

pub fn run(cmd: ScalarCmd) -> anyhow::Result<String> {
    match cmd {
        ScalarCmd::Register {
            no_maintenance,
            enlistment,
        } => {
            register(&open(enlistment.as_deref())?, !no_maintenance)?;
            Ok(String::new())
        }
        ScalarCmd::Unregister { enlistment } => {
            let backend = open(enlistment.as_deref())?;
            maintenance(&backend, false)?;
            remove_repo(&root(&backend)?)?;
            Ok(String::new())
        }
        ScalarCmd::List => Ok(repos().iter().map(|r| format!("{r}\n")).collect()),
        ScalarCmd::Run { task, enlistment } => {
            let backend = open(enlistment.as_deref())?;
            if task != "all" {
                return run_task(&backend, &task);
            }
            let mut out = String::new();
            for t in [
                "config",
                "commit-graph",
                "fetch",
                "loose-objects",
                "pack-files",
            ] {
                out.push_str(&run_task(&backend, t)?);
            }
            Ok(out)
        }
        ScalarCmd::Reconfigure {
            all,
            maintenance: mode,
            enlistment,
        } => {
            let mode = mode.unwrap_or_else(|| "enable".to_owned());
            if !matches!(mode.as_str(), "enable" | "disable" | "keep") {
                anyhow::bail!("unknown mode for --maintenance option: {mode}");
            }
            let targets = if all {
                repos().into_iter().map(Some).collect()
            } else {
                vec![enlistment]
            };
            for t in targets {
                let backend = match open(t.as_deref()) {
                    Ok(b) => b,
                    Err(e) if all => {
                        eprintln!("warning: could not open '{}': {e}", t.unwrap_or_default());
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                set_config(&backend, true)?;
                if mode != "keep" {
                    maintenance(&backend, mode == "enable")?;
                }
            }
            Ok(String::new())
        }
        ScalarCmd::Delete { enlistment } => {
            let backend = open(Some(&enlistment))?;
            maintenance(&backend, false)?;
            remove_repo(&root(&backend)?)?;
            drop(backend);
            std::fs::remove_dir_all(&enlistment)?;
            Ok(String::new())
        }
        ScalarCmd::Clone {
            url,
            enlistment,
            branch,
            single_branch,
            no_src,
            no_tags,
            full_clone: _,
        } => {
            let enlistment = enlistment.unwrap_or_else(|| {
                url.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("repo")
                    .trim_end_matches(".git")
                    .to_owned()
            });
            if Path::new(&enlistment).exists() {
                anyhow::bail!("directory '{enlistment}' exists already");
            }
            let dir = if no_src {
                PathBuf::from(&enlistment)
            } else {
                Path::new(&enlistment).join("src")
            };
            let mut clone = std::process::Command::new(std::env::current_exe()?);
            clone.args(["--human", "clone"]);
            if let Some(b) = &branch {
                clone.args(["--branch", b]);
            }
            if single_branch {
                clone.arg("--single-branch");
            }
            if no_tags {
                clone.arg("--no-tags");
            }
            if !clone.arg(&url).arg(&dir).status()?.success() {
                anyhow::bail!("failed to clone '{url}'");
            }
            register(&open(Some(&dir.to_string_lossy()))?, true)?;
            Ok(String::new())
        }
        ScalarCmd::Version => Ok(format!("rgit version {}\n", env!("CARGO_PKG_VERSION"))),
    }
}
