//! `rgit maintenance` and `rgit for-each-repo`: registering repositories,
//! running the tasks, and the launchd, cron or systemd schedule that runs
//! `rgit for-each-repo --config=maintenance.repo maintenance run --schedule=…`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rgit_git::{ConfigScope, GitBackend, GitError};

use crate::cli::MaintenanceCmd;

const FREQUENCIES: [&str; 3] = ["hourly", "daily", "weekly"];

pub fn run(backend: &Arc<dyn GitBackend>, cmd: MaintenanceCmd) -> anyhow::Result<String> {
    use rgit_git::ConfigScope::{Global, Local};
    let repo = backend.workdir().canonicalize()?.display().to_string();
    let git_dir = backend.git_dir();
    let registered = || -> bool {
        rgit_git::config_list(Some(&git_dir), &Global, false)
            .unwrap_or_default()
            .iter()
            .any(|e| e.name == "maintenance.repo" && e.value.as_deref() == Some(repo.as_str()))
    };
    let register = || -> anyhow::Result<()> {
        if !registered() {
            rgit_git::config_set(
                Some(&git_dir),
                &Global,
                "maintenance.repo",
                &repo,
                None,
                rgit_git::SetMode::Add,
            )?;
        }
        rgit_git::config_set(
            Some(&git_dir),
            &Local,
            "maintenance.auto",
            "false",
            None,
            rgit_git::SetMode::Replace,
        )?;
        if backend.config_get("maintenance.strategy")?.is_none() {
            rgit_git::config_set(
                Some(&git_dir),
                &Local,
                "maintenance.strategy",
                "incremental",
                None,
                rgit_git::SetMode::Replace,
            )?;
        }
        Ok(())
    };
    match cmd {
        MaintenanceCmd::Register => {
            register()?;
            Ok(String::new())
        }
        MaintenanceCmd::Unregister { force } => {
            if !registered() {
                if force {
                    return Ok(String::new());
                }
                return Err(
                    GitError::Other(format!("repository '{repo}' is not registered")).into(),
                );
            }
            rgit_git::config_unset(
                Some(&git_dir),
                &Global,
                "maintenance.repo",
                Some(&rgit_git::config_fixed_value(&repo)),
                true,
            )?;
            Ok(String::new())
        }
        MaintenanceCmd::Run {
            task,
            auto,
            schedule,
            quiet,
        } => Ok(backend.maintenance_run(&rgit_git::MaintenanceRun {
            tasks: task,
            auto,
            schedule,
            quiet,
        })?),
        MaintenanceCmd::Start { scheduler } => {
            let which = pick(scheduler.as_deref())?;
            register()?;
            schedule(which, true)?;
            Ok(String::new())
        }
        MaintenanceCmd::Stop => {
            schedule(pick(None)?, false)?;
            Ok(String::new())
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Scheduler {
    Launchctl,
    Crontab,
    Systemd,
}

/// Tests set RGIT_TEST_MAINT_SCHEDULER_DIR: schedule files go there and no
/// launchctl, crontab or systemctl runs.
fn test_dir() -> Option<PathBuf> {
    std::env::var_os("RGIT_TEST_MAINT_SCHEDULER_DIR").map(PathBuf::from)
}

fn pick(name: Option<&str>) -> anyhow::Result<Scheduler> {
    Ok(match name.unwrap_or("auto") {
        "launchctl" => Scheduler::Launchctl,
        "crontab" => Scheduler::Crontab,
        "systemd" | "systemd-timer" => Scheduler::Systemd,
        "auto" if cfg!(target_os = "macos") => Scheduler::Launchctl,
        "auto" if test_dir().is_none() && systemd_works() => Scheduler::Systemd,
        "auto" => Scheduler::Crontab,
        "schtasks" => anyhow::bail!("schtasks scheduling is not supported by rgit"),
        other => {
            return Err(crate::cli::CliError::usage(format!(
                "unrecognized --scheduler argument '{other}'"
            )));
        }
    })
}

fn systemd_works() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "list-timers"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A minute of the hour picked once per start, as git does, so every
/// machine does not run at :00.
fn minute() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos())
        % 60
}

fn exe() -> anyhow::Result<String> {
    Ok(std::env::current_exe()?.display().to_string())
}

fn home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))
}

fn quiet(cmd: &str, args: &[&str]) -> anyhow::Result<bool> {
    Ok(std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?
        .success())
}

/// Write (or with `on` false, remove) the schedule for `which`.
fn schedule(which: Scheduler, on: bool) -> anyhow::Result<()> {
    match which {
        Scheduler::Launchctl => launchctl(on),
        Scheduler::Crontab => crontab(on),
        Scheduler::Systemd => systemd(on),
    }
}

/// The launchd job `org.rgit.rgit.<frequency>`, as git's own plist.
pub fn plist(label: &str, exe: &str, frequency: &str, minute: u32) -> String {
    let mut out = format!(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>{label}</string>\n<key>ProgramArguments</key>\n<array>\n\
         <string>{exe}</string>\n<string>for-each-repo</string>\n<string>--keep-going</string>\n\
         <string>--config=maintenance.repo</string>\n<string>maintenance</string>\n\
         <string>run</string>\n<string>--schedule={frequency}</string>\n</array>\n\
         <key>StartCalendarInterval</key>\n<array>\n"
    );
    let cell = |weekday: Option<u32>, hour: u32| {
        let day = weekday.map_or(String::new(), |d| {
            format!("<key>Weekday</key><integer>{d}</integer>\n")
        });
        format!(
            "<dict>\n{day}<key>Hour</key><integer>{hour}</integer>\n\
             <key>Minute</key><integer>{minute}</integer>\n</dict>\n"
        )
    };
    match frequency {
        "hourly" => (1..=23).for_each(|h| out.push_str(&cell(None, h))),
        "daily" => (1..=6).for_each(|d| out.push_str(&cell(Some(d), 0))),
        _ => out.push_str(&cell(Some(0), 0)),
    }
    out.push_str("</array>\n</dict>\n</plist>\n");
    out
}

fn launchctl(on: bool) -> anyhow::Result<()> {
    let test = test_dir();
    let dir = match &test {
        Some(d) => d.clone(),
        None => home()?.join("Library/LaunchAgents"),
    };
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    let domain = format!("gui/{uid}");
    let min = minute();
    for f in FREQUENCIES {
        let label = format!("org.rgit.rgit.{f}");
        let file = dir.join(format!("{label}.plist"));
        let path = file.display().to_string();
        if test.is_none() && file.exists() {
            quiet("launchctl", &["bootout", &domain, &path])?;
        }
        if !on {
            let _ = std::fs::remove_file(&file);
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&file, plist(&label, &exe()?, f, min))?;
        if test.is_none() && !quiet("launchctl", &["bootstrap", &domain, &path])? {
            anyhow::bail!("failed to bootstrap service {path}");
        }
    }
    Ok(())
}

const CRON_BEGIN: &str = "# BEGIN RGIT MAINTENANCE SCHEDULE";
const CRON_END: &str = "# END RGIT MAINTENANCE SCHEDULE";

/// `crontab` with rgit's block replaced by `block` (or dropped).
pub fn cron_table(current: &str, block: Option<&str>) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in current.lines() {
        if line == CRON_BEGIN {
            inside = true;
        } else if line == CRON_END {
            inside = false;
        } else if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    if let Some(b) = block {
        out.push_str(b);
    }
    out
}

fn crontab(on: bool) -> anyhow::Result<()> {
    let test = test_dir().map(|d| d.join("crontab"));
    let current = match &test {
        Some(f) => std::fs::read_to_string(f).unwrap_or_default(),
        None => std::process::Command::new("crontab")
            .arg("-l")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default(),
    };
    let block = if on {
        let exe = exe()?;
        let m = minute();
        let line = |spec: &str, f: &str| {
            format!(
                "{m} {spec} \"{exe}\" for-each-repo --keep-going --config=maintenance.repo \
                 maintenance run --schedule={f}\n"
            )
        };
        Some(format!(
            "{CRON_BEGIN}\n# The following schedule was created by rgit\n# Any edits made in \
             this region might be\n# replaced in the future by an rgit command.\n\n{}{}{}\n\
             {CRON_END}\n",
            line("1-23 * * *", "hourly"),
            line("0 * * 1-6", "daily"),
            line("0 * * 0", "weekly"),
        ))
    } else {
        None
    };
    let table = cron_table(&current, block.as_deref());
    match test {
        Some(f) => {
            if let Some(d) = f.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(f, table)?;
        }
        None => {
            use std::io::Write;
            let mut child = std::process::Command::new("crontab")
                .arg("-")
                .stdin(std::process::Stdio::piped())
                .spawn()?;
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(table.as_bytes())?;
            if !child.wait()?.success() {
                anyhow::bail!("crontab failed");
            }
        }
    }
    Ok(())
}

fn systemd(on: bool) -> anyhow::Result<()> {
    let test = test_dir();
    let dir = match &test {
        Some(d) => d.clone(),
        None => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .map_or_else(|| home().map(|h| h.join(".config")), Ok)?
            .join("systemd/user"),
    };
    let units: Vec<String> = FREQUENCIES
        .iter()
        .map(|f| format!("rgit-maintenance@{f}.timer"))
        .collect();
    let units: Vec<&str> = units.iter().map(String::as_str).collect();
    if !on {
        if test.is_none() {
            let mut args = vec!["--user", "disable", "--now"];
            args.extend(&units);
            quiet("systemctl", &args)?;
        }
        for u in &units {
            let _ = std::fs::remove_file(dir.join(u));
        }
        let _ = std::fs::remove_file(dir.join("rgit-maintenance@.service"));
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    let m = minute();
    let header = "# This file was created and is maintained by rgit.\n# Any edits made in this \
                  file might be replaced in the future\n# by an rgit command.\n\n";
    for f in FREQUENCIES {
        let when = match f {
            "hourly" => format!("*-*-* 1..23:{m:02}:00"),
            "daily" => format!("Tue..Sun *-*-* 0:{m:02}:00"),
            _ => format!("Mon 0:{m:02}:00"),
        };
        std::fs::write(
            dir.join(format!("rgit-maintenance@{f}.timer")),
            format!(
                "{header}[Unit]\nDescription=Optimize Git repositories data\n\n[Timer]\n\
                 OnCalendar={when}\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"
            ),
        )?;
    }
    std::fs::write(
        dir.join("rgit-maintenance@.service"),
        format!(
            "{header}[Unit]\nDescription=Optimize Git repositories data\n\n[Service]\n\
             Type=oneshot\nExecStart=\"{}\" for-each-repo --keep-going \
             --config=maintenance.repo maintenance run --schedule=%i\nLockPersonality=yes\n\
             MemoryDenyWriteExecute=yes\nNoNewPrivileges=yes\nRestrictAddressFamilies=AF_UNIX \
             AF_INET AF_INET6 AF_VSOCK\nRestrictNamespaces=yes\nRestrictRealtime=yes\n\
             RestrictSUIDSGID=yes\nSystemCallArchitectures=native\n\
             SystemCallFilter=@system-service\n",
            exe()?
        ),
    )?;
    if test.is_none() {
        let mut args = vec!["--user", "enable", "--now"];
        args.extend(&units);
        if !quiet("systemctl", &args)? {
            anyhow::bail!("failed to start systemd timers");
        }
    }
    Ok(())
}

/// `git for-each-repo --config=<key>`: run rgit with `args` in every
/// repository the multi-valued config key lists, in this run's output mode
/// (`human`: git's text).
pub fn for_each_repo(
    config: &str,
    keep_going: bool,
    args: &[String],
    human: bool,
) -> anyhow::Result<String> {
    let paths: Vec<String> = rgit_git::config_list(None, &ConfigScope::Any, true)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.name.eq_ignore_ascii_case(config))
        .filter_map(|e| e.value)
        .collect();
    let exe = std::env::current_exe()?;
    let mut failed = false;
    for p in paths {
        let dir = match p.strip_prefix("~/") {
            Some(rest) => home()?.join(rest),
            None => Path::new(&p).to_path_buf(),
        };
        let ok = std::process::Command::new(&exe)
            .args(human.then_some("--human"))
            .args(args)
            .current_dir(&dir)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            failed = true;
            if !keep_going {
                break;
            }
        }
    }
    if failed {
        return Err(crate::cli::CliError {
            message: String::new(),
            help: None,
            code: 1,
        }
        .into());
    }
    Ok(String::new())
}
