use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use rgit_config::Settings;

use crate::clean::clean;
use crate::interrupt::{Guard, INTERRUPTED};
use crate::prompt::{prompt, tool_for};
use crate::{Error, Inputs, Task};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const STDERR_LINES: usize = 20;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A private temp file that is removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn create(label: &str, text: &str) -> Result<TempFile, Error> {
        loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let path = std::env::temp_dir().join(format!(
                "rgit-ai-{}-{n}-{nanos}-{label}",
                std::process::id()
            ));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let failed = |error: std::io::Error| Error::TempFile {
                path: path.clone(),
                error: error.to_string(),
            };
            match options.open(&path) {
                Ok(mut file) => {
                    let temp = TempFile(path.clone());
                    file.write_all(text.as_bytes()).map_err(failed)?;
                    return Ok(temp);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(failed(error)),
            }
        }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn read_pipe<R: Read + Send + 'static>(mut pipe: R) -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

#[cfg(unix)]
fn kill_group(pid: u32, signal: libc::c_int) {
    // SAFETY: signals the process group the child leads.
    unsafe { libc::kill(-(pid as libc::pid_t), signal) };
}

/// Stop the child and, on unix, everything in its process group.
fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        kill_group(child.id(), libc::SIGTERM);
        let grace = Instant::now() + Duration::from_secs(1);
        while Instant::now() < grace {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        kill_group(child.id(), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.trim().lines().collect();
    lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n")
}

/// Render the prompt, run the tool and clean its output. Blocks until the tool
/// ends. No signal handler is installed.
pub fn run(settings: &Settings, task: Task, tool: &str, inputs: &Inputs) -> Result<String, Error> {
    run_with_cancel(settings, task, tool, inputs, &AtomicBool::new(false))
}

/// [`run`] for a command line program: SIGINT and SIGTERM stop the tool and
/// give [`Error::Interrupted`] while it runs.
pub fn run_interruptible(
    settings: &Settings,
    task: Task,
    tool: &str,
    inputs: &Inputs,
) -> Result<String, Error> {
    let _guard = Guard::install();
    run_with_cancel(settings, task, tool, inputs, &INTERRUPTED)
}

/// [`run`] without the signal handlers: it stops when `cancel` becomes true.
pub fn run_with_cancel(
    settings: &Settings,
    task: Task,
    tool: &str,
    inputs: &Inputs,
    cancel: &AtomicBool,
) -> Result<String, Error> {
    let tool = tool_for(settings, task, Some(tool))?;
    let text = prompt(settings, task, &tool, inputs)?;
    let setting = |key: &str| {
        settings
            .table()
            .get("ai")
            .and_then(|ai| ai.get(&tool))
            .and_then(|t| t.get(key))
    };
    let command = setting("command")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let secs = setting("timeout_secs")
        .and_then(toml::Value::as_integer)
        .map_or(DEFAULT_TIMEOUT_SECS, |n| {
            u64::try_from(n).unwrap_or(1).max(1)
        });

    let prompt_file = TempFile::create("prompt", &text)?;
    let diff_file = TempFile::create("diff", &inputs.diff)?;
    let mut command_line = Command::new("sh");
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command_line, 0);
    let mut child = command_line
        .arg("-c")
        .arg(&command)
        .current_dir(&inputs.repo)
        .env("RGIT_PROMPT_FILE", &prompt_file.0)
        .env("RGIT_DIFF_FILE", &diff_file.0)
        .env("RGIT_TASK", task.as_str())
        .env("RGIT_REPO", &inputs.repo)
        .env("RGIT_BRANCH", &inputs.branch)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Error::Spawn {
            tool: tool.clone(),
            error: error.to_string(),
        })?;
    let pid = child.id();

    let stdin = child.stdin.take();
    std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(text.as_bytes());
        }
    });
    let stdout = read_pipe(child.stdout.take().expect("stdout is piped"));
    let stderr = read_pipe(child.stderr.take().expect("stderr is piped"));

    let deadline = Instant::now() + Duration::from_secs(secs);
    let timeout = |tool: String| Error::Timeout { tool, secs };
    let status = loop {
        if cancel.load(Ordering::SeqCst) {
            stop(&mut child);
            return Err(Error::Interrupted);
        }
        match child.try_wait().map_err(|error| Error::Spawn {
            tool: tool.clone(),
            error: error.to_string(),
        })? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                stop(&mut child);
                return Err(timeout(tool));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // A background child can still hold the pipes open; end it so they close.
    #[cfg(unix)]
    kill_group(pid, libc::SIGKILL);
    #[cfg(not(unix))]
    let _ = pid;
    let collect = |rx: Receiver<Vec<u8>>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    };
    let (Ok(stdout), Ok(stderr)) = (collect(stdout), collect(stderr)) else {
        return Err(timeout(tool));
    };
    if !status.success() {
        return Err(Error::Failed {
            tool,
            code: status.code(),
            stderr: tail(&stderr),
        });
    }
    Ok(clean(task, &stdout))
}
