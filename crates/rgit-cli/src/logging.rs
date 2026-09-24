//! Logging for diagnostics: a file in the user's state directory and an
//! in-memory ring buffer for the in-app log window, both filtered by `RGIT_LOG`
//! (default `warn`, with our git-style `git` output kept at `info`). Logging
//! never fails loudly: if the file cannot be opened, the app still runs (and the
//! in-memory capture still works).
//!
//! The file appender writes synchronously (in the calling thread) rather than
//! buffering on a worker: the process ends via `exit()`, which runs no
//! destructors, so a buffered writer's flush-on-drop would never fire and a
//! panic's final log line would be lost.

use std::path::PathBuf;

use rgit_tui::session_log::{self, LogLevel};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

/// Where the log file lives: `$XDG_STATE_HOME/rgit/` or `~/.local/state/rgit/`.
fn log_dir() -> Option<PathBuf> {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(state).join("rgit"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/rgit"))
}

/// A tracing layer that mirrors each event into the in-memory session log.
struct MemoryLayer;

impl<S: Subscriber> Layer<S> for MemoryLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = match *meta.level() {
            Level::ERROR => LogLevel::Error,
            Level::WARN => LogLevel::Warn,
            Level::INFO => LogLevel::Info,
            Level::DEBUG => LogLevel::Debug,
            Level::TRACE => LogLevel::Trace,
        };
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        session_log::record(level, meta.target().to_owned(), visitor.0);
    }
}

/// Pulls just the `message` field out of an event.
struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_owned();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" && self.0.is_empty() {
            self.0 = format!("{value:?}");
        }
    }
}

/// Install the file logger and the in-memory capture. Returns whether the file
/// logger was set up (the in-memory capture is installed regardless).
pub fn init() -> bool {
    // Keep our git-style output (target `git`) at info even at the default
    // level, so the log window and file always show it.
    let filter = EnvFilter::try_from_env("RGIT_LOG").unwrap_or_else(|_| {
        EnvFilter::new("warn,git=info,rgit_git=info,rgit_tui=info,startup=info")
    });

    let (file_layer, ok) = match log_dir() {
        Some(dir) if std::fs::create_dir_all(&dir).is_ok() => {
            let appender = tracing_appender::rolling::never(&dir, "rgit.log");
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(appender)
                .with_ansi(false);
            (Some(layer), true)
        }
        _ => (None, false),
    };

    let installed = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(MemoryLayer)
        .try_init()
        .is_ok();
    installed && ok
}
