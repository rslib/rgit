//! Terminal credential prompt for the CLI: asks for a username (echoed) or a
//! password / SSH passphrase (hidden) when the backend's non-interactive
//! sources cannot authenticate. Prompts only on a real terminal, so piped or
//! scripted runs fail cleanly instead of blocking.

use std::io::{self, IsTerminal, Write};

use rgit_git::CredentialPrompt;

pub struct TerminalPrompt;

impl CredentialPrompt for TerminalPrompt {
    fn username(&self, url: &str) -> Option<String> {
        prompt(&format!("Username for {url}: "), false)
    }

    fn password(&self, url: &str, user: &str) -> Option<String> {
        prompt(&format!("Password for {user}@{url}: "), true)
    }

    fn ssh_passphrase(&self, key: &str) -> Option<String> {
        prompt(&format!("Passphrase for {key}: "), true)
    }
}

/// Ask for an SSH password on the terminal (hidden), for the in-process ssh
/// transport's fallback when key auth fails and no ControlMaster is available.
/// Returns `None` off a terminal, so scripted runs fail instead of blocking.
pub fn ssh_password(label: &str) -> Option<String> {
    prompt(label, true)
}

/// Print `label` to stderr and read one line from stdin. With `hidden`, echo is
/// disabled around the read (via `stty`, a terminal-control escape hatch) so a
/// secret is not shown. Returns `None` off a terminal or on EOF/empty.
fn prompt(label: &str, hidden: bool) -> Option<String> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return None;
    }
    // Pause any running spinner (e.g. during push) and clear its line first, so
    // the prompt is not scribbled over by the animation.
    crate::prompt::suppress_spinner(true);
    eprint!("\r\u{1b}[2K{label}");
    io::stderr().flush().ok();

    if hidden {
        set_echo(false);
    }
    let mut line = String::new();
    let read = io::stdin().read_line(&mut line);
    if hidden {
        set_echo(true);
        eprintln!();
    }
    crate::prompt::suppress_spinner(false);
    match read {
        Ok(n) if n > 0 => {
            let value = line.trim_end_matches(['\n', '\r']).to_owned();
            (!value.is_empty()).then_some(value)
        }
        _ => None,
    }
}

#[cfg(unix)]
fn set_echo(on: bool) {
    // stty acts on the controlling terminal inherited on stdin.
    let arg = if on { "echo" } else { "-echo" };
    let _ = std::process::Command::new("stty").arg(arg).status();
}

#[cfg(not(unix))]
fn set_echo(_on: bool) {
    // No portable echo toggle; the secret is visible on non-unix terminals.
}
