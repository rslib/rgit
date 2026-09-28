//! git's top-level options (`-C`, `-c`, `--git-dir`, `-p`, ...), aliases,
//! external `git-<cmd>`/`rgit-<cmd>` commands and the pager.

use std::path::{Path, PathBuf};
use std::process::{Command, exit};

use clap::CommandFactory;
use rgit_git::GitBackend;

use crate::cli::Cli;

fn setenv(key: &str, value: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: called while rgit is still single-threaded.
    unsafe { std::env::set_var(key, value) };
}

fn fatal(message: &str, code: i32) -> ! {
    eprintln!("fatal: {message}");
    exit(code)
}

fn usage(message: &str) -> ! {
    eprintln!("error: {message}");
    eprintln!(
        "usage: rgit [-C <path>] [-c <name>=<value>] [--git-dir=<path>] [--work-tree=<path>] [--bare] [-p | -P] <command> [<args>]"
    );
    exit(129)
}

/// rgit's own top-level flags that take a separate value (`--fields x`).
fn takes_value(flag: &str) -> bool {
    Cli::command().get_arguments().any(|a| {
        a.get_long().is_some_and(|l| flag == format!("--{l}"))
            && a.get_action().takes_values()
            && !a.is_require_equals_set()
    })
}

/// Append one `-c` setting to GIT_CONFIG_PARAMETERS as git does, so libgit2
/// reads and spawned git both see it.
fn push_config(key: &str, value: Option<&str>) {
    let sq = |s: &str| format!("'{}'", s.replace('\'', "'\\''").replace('!', "'\\!'"));
    let mut env = std::env::var("GIT_CONFIG_PARAMETERS").unwrap_or_default();
    if !env.is_empty() {
        env.push(' ');
    }
    env.push_str(&sq(key));
    if let Some(v) = value {
        env.push('=');
        env.push_str(&sq(v));
    }
    setenv("GIT_CONFIG_PARAMETERS", env);
}

fn config_arg(spec: &str) {
    match spec.split_once('=') {
        Some((k, v)) => push_config(k, Some(v)),
        None => push_config(spec, None),
    }
    if let Err(e) = rgit_git::config_key(spec.split_once('=').map_or(spec, |(k, _)| k)) {
        eprintln!("error: {e}");
        fatal("unable to parse command-line config", 128);
    }
}

fn exe_prefix() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent()?.parent().map(Path::to_path_buf))
        .unwrap_or_default()
}

fn exec_path() -> PathBuf {
    std::env::var_os("GIT_EXEC_PATH")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
        })
        .unwrap_or_default()
}

/// Consume the global options in front of the subcommand, applying each as
/// git does (a directory change or an environment variable). Returns the
/// remaining arguments and the `-p`/`-P` choice.
pub fn apply(args: Vec<String>) -> (Vec<String>, Option<bool>) {
    let mut out = Vec::new();
    let mut paginate = None;
    let mut it = args.into_iter();
    let path_env = |flag: &str, env: &str, value: Option<String>| match value {
        Some(v) => setenv(env, v),
        None => usage(&format!("no directory given for '{flag}' option")),
    };
    while let Some(arg) = it.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(v.to_owned())),
            _ => (arg.clone(), None),
        };
        let mut value = || inline.clone().or_else(|| it.next());
        match name.as_str() {
            "-C" => {
                let Some(dir) = it.next() else {
                    usage("no directory given for '-C' option")
                };
                if !dir.is_empty()
                    && let Err(e) = std::env::set_current_dir(&dir)
                {
                    fatal(&format!("cannot change to '{dir}': {}", io_reason(&e)), 128);
                }
            }
            "-c" => match it.next() {
                Some(spec) => config_arg(&spec),
                None => usage("-c expects a configuration string"),
            },
            "--config-env" => {
                let spec = value().unwrap_or_default();
                let Some((key, var)) = spec.rsplit_once('=') else {
                    fatal(&format!("missing config value for '{spec}'"), 128)
                };
                match std::env::var(var) {
                    Ok(v) => push_config(key, Some(&v)),
                    Err(_) => fatal(
                        &format!("missing environment variable '{var}' for configuration '{key}'"),
                        128,
                    ),
                }
            }
            "--git-dir" => path_env("--git-dir", "GIT_DIR", value()),
            "--work-tree" => path_env("--work-tree", "GIT_WORK_TREE", value()),
            "--namespace" => match value() {
                Some(v) => setenv("GIT_NAMESPACE", v),
                None => usage("no namespace given for --namespace"),
            },
            "--attr-source" => match value() {
                Some(v) => setenv("GIT_ATTR_SOURCE", v),
                None => usage("no attribute source given for --attr-source"),
            },
            "--bare" => {
                let cwd = std::env::current_dir().unwrap_or_default();
                setenv("GIT_DIR", cwd);
            }
            "-p" | "--paginate" => paginate = Some(true),
            "-P" | "--no-pager" => paginate = Some(false),
            "--no-replace-objects" => setenv("GIT_NO_REPLACE_OBJECTS", "1"),
            "--literal-pathspecs" => setenv("GIT_LITERAL_PATHSPECS", "1"),
            "--no-literal-pathspecs" => setenv("GIT_LITERAL_PATHSPECS", "0"),
            "--glob-pathspecs" => setenv("GIT_GLOB_PATHSPECS", "1"),
            "--noglob-pathspecs" => setenv("GIT_NOGLOB_PATHSPECS", "1"),
            "--icase-pathspecs" => setenv("GIT_ICASE_PATHSPECS", "1"),
            "--no-optional-locks" => setenv("GIT_OPTIONAL_LOCKS", "0"),
            "--no-advice" => setenv("GIT_ADVICE", "0"),
            "--no-lazy-fetch" => setenv("GIT_NO_LAZY_FETCH", "1"),
            "--exec-path" => match inline {
                Some(p) => setenv("GIT_EXEC_PATH", p),
                None => {
                    println!("{}", exec_path().display());
                    exit(0)
                }
            },
            "--html-path" | "--man-path" | "--info-path" => {
                let sub = match name.as_str() {
                    "--html-path" => "share/doc/rgit",
                    "--man-path" => "share/man",
                    _ => "share/info",
                };
                println!("{}", exe_prefix().join(sub).display());
                exit(0)
            }
            "-v" | "--version" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                exit(0)
            }
            _ if !arg.starts_with('-') || arg == "-" => {
                out.push(arg);
                break;
            }
            _ => {
                let more = inline.is_none() && takes_value(&name);
                out.push(arg);
                if more {
                    out.extend(it.next());
                }
            }
        }
    }
    out.extend(it);
    absolute_env();
    (out, paginate)
}

fn io_reason(e: &std::io::Error) -> String {
    let s = e.to_string();
    s.split(" (os error").next().unwrap_or(&s).to_owned()
}

/// Paths given relative to the folder rgit ends up in stay valid for the git
/// it spawns from the top of the work tree.
fn absolute_env() {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
    ] {
        if let Some(v) = std::env::var_os(key).filter(|v| !v.is_empty())
            && Path::new(&v).is_relative()
        {
            setenv(key, cwd.join(v));
        }
    }
}

/// Where the subcommand sits in `args`, past rgit's own top-level flags.
fn command_index(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !a.starts_with('-') {
            return Some(i);
        }
        if a == "--" {
            return None;
        }
        i += if !a.contains('=') && takes_value(a) {
            2
        } else {
            1
        };
    }
    None
}

fn builtin(name: &str) -> Option<String> {
    Cli::command()
        .find_subcommand(name)
        .map(|c| c.get_name().to_owned())
}

/// The canonical name of the subcommand in `args`, if it is one of rgit's.
pub fn command_name(args: &[String]) -> Option<String> {
    builtin(&args[command_index(args)?])
}

/// git's split_cmdline: words split on whitespace, with '...' and "..."
/// quoting and backslash escapes.
fn split_cmdline(s: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => word.push(c),
            (_, '\\') => {
                word.push(chars.next()?);
                started = true;
            }
            (_, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(word);
    }
    Some(words)
}

fn find_on(dirs: impl IntoIterator<Item = PathBuf>, file: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    dirs.into_iter().map(|d| d.join(file)).find(|p| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// `rgit-<name>` on PATH, else `git-<name>` in git's exec path or on PATH.
fn external(name: &str) -> Option<PathBuf> {
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    find_on(path.clone(), &format!("rgit-{name}")).or_else(|| {
        let git_exec = std::env::var_os("GIT_EXEC_PATH")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                let out = Command::new("git").arg("--exec-path").output().ok()?;
                Some(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
            });
        find_on(git_exec.into_iter().chain(path), &format!("git-{name}"))
    })
}

fn run(mut cmd: Command) -> ! {
    match cmd.status() {
        Ok(s) => {
            use std::os::unix::process::ExitStatusExt;
            exit(s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)))
        }
        Err(e) => fatal(&format!("cannot run {:?}: {e}", cmd.get_program()), 128),
    }
}

/// The top of the work tree and the current folder relative to it (git's
/// prefix, with a trailing slash), when in a repository.
fn top_and_prefix() -> Option<(PathBuf, String)> {
    let repo = rgit_git::Git2Backend::open_env(".").ok()?;
    let top = std::fs::canonicalize(repo.workdir()).ok()?;
    let cwd = std::env::current_dir().ok()?.canonicalize().ok()?;
    let prefix = cwd
        .strip_prefix(&top)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty())
        .map(|p| p + "/")
        .unwrap_or_default();
    Some((top, prefix))
}

/// Resolve a subcommand rgit does not have: an external `rgit-<name>` or
/// `git-<name>` runs with the arguments, and `alias.<name>` expands (a `!`
/// alias runs in the shell from the top of the work tree), as in git.
/// Otherwise the arguments come back unchanged for clap to report.
pub fn dispatch(mut args: Vec<String>) -> Vec<String> {
    let Some(at) = command_index(&args) else {
        return args;
    };
    if args[at] == "help"
        && let Some(name) = args.get(at + 1)
        && builtin(name).is_none()
        && let Some(alias) = rgit_git::config_get(&format!("alias.{name}"))
    {
        println!("'{name}' is aliased to '{alias}'");
        exit(0);
    }
    let mut seen: Vec<String> = Vec::new();
    loop {
        let name = args[at].clone();
        if builtin(&name).is_some() {
            return args;
        }
        if let Some(pos) = seen.iter().position(|s| *s == name) {
            let mut chain = String::new();
            for (i, s) in seen.iter().enumerate() {
                chain.push_str(&format!("\n  {s}"));
                if i == pos {
                    chain.push_str(" <==");
                } else if i == seen.len() - 1 {
                    chain.push_str(" ==>");
                }
            }
            fatal(
                &format!(
                    "alias loop detected: expansion of '{}' does not terminate:{chain}",
                    seen[0]
                ),
                128,
            );
        }
        seen.push(name.clone());
        if let Some(program) = external(&name) {
            let mut cmd = Command::new(program);
            cmd.args(&args[at + 1..]);
            run(cmd);
        }
        let Some(alias) = rgit_git::config_get(&format!("alias.{name}")) else {
            return args;
        };
        if args[at + 1..].iter().any(|a| a == "--help") {
            println!("'{name}' is aliased to '{alias}'");
            exit(0);
        }
        if let Some(shell) = alias.strip_prefix('!') {
            let mut cmd = Command::new("sh");
            let rest = &args[at + 1..];
            let script = if rest.is_empty() {
                shell.to_owned()
            } else {
                format!("{shell} \"$@\"")
            };
            cmd.arg("-c").arg(script).arg(shell).args(rest);
            let (top, prefix) = top_and_prefix().unwrap_or_default();
            if !top.as_os_str().is_empty() {
                cmd.current_dir(top);
            }
            cmd.env("GIT_PREFIX", prefix);
            run(cmd);
        }
        let Some(words) = split_cmdline(&alias) else {
            fatal(&format!("bad alias.{name} string: unclosed quote"), 128)
        };
        if words.is_empty() {
            fatal(&format!("empty alias for {name}"), 128);
        }
        args.splice(at..=at, words);
    }
}

/// Default paging, as git's builtins with USE_PAGER (and branch/tag lists).
const PAGED: &[&str] = &[
    "log",
    "show",
    "diff",
    "grep",
    "blame",
    "annotate",
    "shortlog",
    "reflog",
    "whatchanged",
    "branch",
    "tag",
    "smartlog",
];

/// Start the pager for `command` when git would, sending stdout (and a
/// terminal stderr) to it until rgit exits. Only for human text on a
/// terminal.
pub fn start_pager(command: Option<&str>, paginate: Option<bool>) {
    use std::io::IsTerminal;
    if paginate == Some(false) || !std::io::stdout().is_terminal() {
        return;
    }
    let mut program = None;
    let wanted = match (paginate, command) {
        (Some(true), _) => true,
        (_, Some(cmd)) => match rgit_git::config_get(&format!("pager.{cmd}")) {
            Some(v) => match rgit_git::config_typed(Some(&v), "bool") {
                Ok(b) => b == "true",
                Err(_) => {
                    program = Some(v);
                    true
                }
            },
            None => PAGED.contains(&cmd),
        },
        _ => false,
    };
    if !wanted {
        return;
    }
    let pager = std::env::var("GIT_PAGER")
        .ok()
        .or(program)
        .or_else(|| rgit_git::config_get("core.pager"))
        .or_else(|| std::env::var("PAGER").ok())
        .unwrap_or_else(|| "less".to_owned());
    if pager.is_empty() || pager == "cat" {
        return;
    }
    if std::env::var_os("COLUMNS").is_none()
        && let Ok((w, _)) = crossterm::terminal::size()
    {
        setenv("COLUMNS", w.to_string());
    }
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&pager)
        .stdin(std::process::Stdio::piped());
    for (key, default) in [("LESS", "FRX"), ("LV", "-c")] {
        if std::env::var_os(key).is_none() {
            cmd.env(key, default);
        }
    }
    let Ok(mut child) = cmd.spawn() else {
        return;
    };
    let Some(stdin) = child.stdin.take() else {
        return;
    };
    setenv("GIT_PAGER_IN_USE", "true");
    use std::os::fd::AsRawFd;
    // SAFETY: plain fd duplication; the pipe stays open through fd 1.
    unsafe {
        libc::dup2(stdin.as_raw_fd(), 1);
        if std::io::stderr().is_terminal() {
            libc::dup2(stdin.as_raw_fd(), 2);
        }
    }
    drop(stdin);
    let _ = PAGER.set(std::sync::Mutex::new(child));
    // SAFETY: registers a plain extern "C" function.
    unsafe { libc::atexit(wait_for_pager) };
}

static PAGER: std::sync::OnceLock<std::sync::Mutex<std::process::Child>> =
    std::sync::OnceLock::new();

/// At exit: flush, close our ends of the pipe so the pager sees EOF, and wait.
extern "C" fn wait_for_pager() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // SAFETY: closing our own standard descriptors at exit.
    unsafe {
        libc::close(1);
        libc::close(2);
    }
    if let Some(child) = PAGER.get()
        && let Ok(mut child) = child.lock()
    {
        let _ = child.wait();
    }
}

/// Whether output goes to a terminal, directly or through the pager.
pub fn stdout_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
        || std::env::var_os("GIT_PAGER_IN_USE").is_some_and(|v| v == "true")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_like_git() {
        assert_eq!(
            split_cmdline(r#"log --format='%h %s' "a b" c\ d"#).unwrap(),
            ["log", "--format=%h %s", "a b", "c d"]
        );
        assert!(split_cmdline("log 'x").is_none());
    }
}
