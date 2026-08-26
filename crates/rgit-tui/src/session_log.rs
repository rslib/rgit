//! An in-memory ring buffer of the session's log lines, so the in-app log
//! window can show the same git-style output and diagnostics that go to the log
//! file. A tracing layer (installed by the binary) calls [`record`]; the log
//! view reads [`snapshot`].

use std::collections::VecDeque;
use std::sync::Mutex;

/// How many recent lines to keep; older lines are dropped.
const CAP: usize = 4000;

/// Severity of a captured line, driving its color in the log view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// One captured log line.
#[derive(Debug, Clone)]
pub struct LogLine {
    /// `HH:MM:SS` in UTC, matching the file log's clock.
    pub time: String,
    pub level: LogLevel,
    /// The event target (e.g. `git` for our git-style output).
    pub target: String,
    pub message: String,
}

static LOG: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());

/// Append a line, evicting the oldest once the buffer is full.
pub fn record(level: LogLevel, target: String, message: String) {
    let line = LogLine {
        time: now_hms(),
        level,
        target,
        message,
    };
    if let Ok(mut log) = LOG.lock() {
        if log.len() >= CAP {
            log.pop_front();
        }
        log.push_back(line);
    }
}

/// A copy of the current lines, oldest first.
pub fn snapshot() -> Vec<LogLine> {
    LOG.lock()
        .map(|l| l.iter().cloned().collect())
        .unwrap_or_default()
}

/// Drop every captured line.
pub fn clear() {
    if let Ok(mut log) = LOG.lock() {
        log.clear();
    }
}

/// `HH:MM:SS` (UTC) from the wall clock, without pulling in a time crate.
fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}
