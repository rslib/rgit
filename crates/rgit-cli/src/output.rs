//! One command result in two shapes: human text for a terminal, and a
//! structured value that the agent modes print as TOON or JSON.

use serde_json::{Map, Value};

use crate::cli::CliError;
use crate::toon::{Node, Obj};

/// Long text fields are cut to this many chars unless `--full` is given.
const TRUNCATE_AT: usize = 1500;
/// Plain multi-line output is cut to this many lines unless `--full` is given.
const MAX_LINES: usize = 200;

pub struct Output {
    pub text: String,
    pub data: Obj,
    pub help: Vec<String>,
    lists: Vec<(String, &'static [&'static str])>,
    has_table: bool,
    long: Vec<String>,
}

impl Output {
    pub fn new(text: impl Into<String>) -> Self {
        Output {
            text: text.into(),
            data: Obj::new(),
            help: Vec::new(),
            lists: Vec::new(),
            has_table: false,
            long: Vec::new(),
        }
    }

    /// A result that arrived as a JSON object; its fields keep JSON's order.
    pub fn from_json(text: impl Into<String>, map: Map<String, Value>) -> Self {
        let mut out = Output::new(text);
        out.data = map.into_iter().map(|(k, v)| (k, v.into())).collect();
        out
    }

    /// A plain message: `result: <msg>`, or `lines[N]` when it spans lines.
    /// Empty output still says the command succeeded.
    pub fn message(text: impl Into<String>) -> Self {
        let text = text.into();
        let out = Output::new(text.clone());
        if text.trim().is_empty() {
            out.with("result", "done (no output)")
        } else if text.contains('\n') {
            out.with("lines", text.lines().collect::<Vec<_>>())
        } else {
            out.with("result", text)
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Node> {
        self.data.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Set `key`, replacing an existing value in place or appending.
    pub fn set(&mut self, key: &str, value: impl Into<Node>) {
        let value = value.into();
        match self.get_mut(key) {
            Some(slot) => *slot = value,
            None => self.data.push((key.to_owned(), value)),
        }
    }

    pub fn with(mut self, key: &str, value: impl Into<Node>) -> Self {
        self.set(key, value);
        self
    }

    /// A table under `key`, printed with `defaults` columns plus any `--fields`.
    /// An empty table prints `empty` instead, so "nothing" is explicit.
    pub fn list(
        mut self,
        key: &str,
        rows: Vec<Obj>,
        defaults: &'static [&'static str],
        empty: impl Into<String>,
    ) -> Self {
        self.has_table = true;
        if rows.is_empty() {
            self.set(key, empty.into());
        } else {
            self.set(key, Node::List(rows.into_iter().map(Node::Obj).collect()));
            self.lists.push((key.to_owned(), defaults));
        }
        self
    }

    /// A text field that is truncated unless `--full` is given.
    pub fn long(mut self, key: &str, text: String) -> Self {
        self.set(key, text);
        self.long.push(key.to_owned());
        self
    }

    pub fn help(mut self, line: impl Into<String>) -> Self {
        self.help.push(line.into());
        self
    }

    /// The structured value to print: tables cut to their columns (defaults
    /// first, then requested fields in schema order), long text truncated with
    /// a size note, and `help` last. `rerun` is the current invocation; hints
    /// re-run it with `--full` placed right after `rgit`.
    pub fn finalize(mut self, fields: &[String], full: bool, rerun: &str) -> anyhow::Result<Obj> {
        if !fields.is_empty() && !self.has_table {
            return Err(CliError::usage(
                "--fields applies only to commands that print a table",
            ));
        }
        let lists = std::mem::take(&mut self.lists);
        for (key, defaults) in &lists {
            let Some(Node::List(rows)) = self.get_mut(key) else {
                continue;
            };
            let available: Vec<String> = match rows.first() {
                Some(Node::Obj(first)) => first.iter().map(|(k, _)| k.clone()).collect(),
                _ => Vec::new(),
            };
            if let Some(bad) = fields.iter().find(|f| !available.contains(f)) {
                return Err(anyhow::Error::new(CliError {
                    message: format!("unknown field {bad} for {key}"),
                    help: Some(format!("valid fields for {key}: {}", available.join(", "))),
                    code: 2,
                }));
            }
            let columns: Vec<&str> = defaults
                .iter()
                .copied()
                .filter(|d| available.iter().any(|a| a == d))
                .chain(
                    available
                        .iter()
                        .map(String::as_str)
                        .filter(|a| !defaults.contains(a) && fields.iter().any(|f| f == a)),
                )
                .collect();
            for r in rows.iter_mut() {
                if let Node::Obj(cells) = r {
                    let mut kept = Obj::new();
                    for c in &columns {
                        if let Some(i) = cells.iter().position(|(k, _)| k == c) {
                            kept.push(cells.swap_remove(i));
                        }
                    }
                    *cells = kept;
                }
            }
        }
        let with_full = match rerun.strip_prefix("rgit ") {
            Some(rest) => format!("rgit --full {rest}"),
            None => format!("{rerun} --full"),
        };
        if !full
            && lists.is_empty()
            && let Some(Node::List(lines)) = self.get_mut("lines")
            && lines.len() > MAX_LINES
        {
            let total = lines.len();
            lines.truncate(MAX_LINES);
            self.set("count", format!("{MAX_LINES} of {total} lines"));
            self.help
                .push(format!("Run `{with_full}` to see all {total} lines"));
        }
        if !full {
            for key in std::mem::take(&mut self.long) {
                let Some(Node::Str(text)) = self.get_mut(&key) else {
                    continue;
                };
                let total = text.chars().count();
                if total > TRUNCATE_AT {
                    let cut: String = text.chars().take(TRUNCATE_AT).collect();
                    *text = format!("{cut}\n... (truncated, {total} chars total)");
                    self.help
                        .push(format!("Run `{with_full}` to see the complete {key}"));
                }
            }
        }
        if !self.help.is_empty() {
            let help = std::mem::take(&mut self.help);
            self.set("help", help);
        }
        Ok(self.data)
    }
}

impl From<String> for Output {
    fn from(text: String) -> Self {
        Output::message(text)
    }
}

impl From<&str> for Output {
    fn from(text: &str) -> Self {
        Output::message(text)
    }
}

/// An error's message, fix-it hints, and exit code, with library noise removed.
pub fn translate(error: &anyhow::Error) -> (String, Vec<String>, i32) {
    use rgit_git::GitError;
    if let Some(e) = error.downcast_ref::<CliError>() {
        return (
            sanitize(&e.message),
            e.help.iter().cloned().collect(),
            e.code,
        );
    }
    let Some(e) = error.downcast_ref::<GitError>() else {
        return (sanitize(error.to_string()), Vec::new(), 1);
    };
    let help = match e {
        GitError::NotARepository(_) => "Run `rgit init` to create one here",
        GitError::NothingToCommit => "Run `rgit stage <path>` to stage changes first",
        GitError::NotFastForward => "Run `rgit pull --rebase` to integrate upstream commits",
        GitError::PushRejected => "Run `rgit pull --rebase`, then `rgit push`",
        GitError::DetachedHead => "Run `rgit checkout <branch>` to get on a branch",
        GitError::HunkNotFound { .. } => "Run `rgit diff --patch` to see the current hunks",
        GitError::Conflict(_) => "Run `rgit status` to see the conflicted files",
        _ => "",
    };
    let message = match e {
        GitError::Git(g) => g.message().to_owned(),
        GitError::Cli(text) => text
            .lines()
            .map(|l| {
                l.trim_start_matches("fatal: ")
                    .trim_start_matches("error: ")
            })
            .filter(|l| !l.trim().is_empty() && !l.starts_with("hint: "))
            .collect::<Vec<_>>()
            .join("; "),
        other => other.to_string(),
    };
    let help = if help.is_empty() {
        Vec::new()
    } else {
        vec![help.to_owned()]
    };
    (sanitize(message), help, 1)
}

/// ASCII stand-ins for the glyphs human output uses.
pub fn sanitize(s: impl AsRef<str>) -> String {
    s.as_ref().replace('\u{2191}', "^")
}
