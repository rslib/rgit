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

const NOT_A_REPO: &str = "not a git repository (or any of the parent directories): .git";

/// An error as git prints it for a person running `command`: the stderr text
/// with git's `fatal:`/`error:` prefix (or none where git has none), and
/// git's exit code (128 for fatal, 1 for error).
pub fn human(error: &anyhow::Error, command: Option<&str>) -> (String, i32) {
    use rgit_git::GitError;
    let git = error.downcast_ref::<GitError>();
    let cli = error.downcast_ref::<CliError>();
    let fatal = |m: &str| (format!("fatal: {m}"), 128);
    let error_line = |m: &str| (format!("error: {m}"), 1);
    match git {
        Some(GitError::NotARepository(_)) => return fatal(NOT_A_REPO),
        Some(GitError::Bare(_)) => return fatal("this operation must be run in a work tree"),
        Some(GitError::Cli(text)) => {
            let code = if text.contains("fatal: ") { 128 } else { 1 };
            return (text.trim_end().to_owned(), code);
        }
        Some(GitError::Conflict(m)) if m == rgit_git::OCTOPUS_FAILED => {
            return (m.clone(), 2);
        }
        Some(GitError::Conflict(m)) if m.starts_with("merge conflicts; resolve") => {
            return (String::new(), 1);
        }
        Some(GitError::Conflict(m))
            if m.starts_with("Your local changes") || m.starts_with("The following untracked") =>
        {
            let code = if m.ends_with("failed.") { 2 } else { 1 };
            let advice = advice();
            let m: Vec<&str> = m
                .lines()
                .map(|l| {
                    if !advice && l.starts_with("Please ") {
                        ""
                    } else {
                        l
                    }
                })
                .collect();
            return (format!("error: {}", m.join("\n")), code);
        }
        Some(GitError::Conflict(m)) if m.starts_with("could not apply ") => {
            if let Some((what, rest)) = m["could not apply ".len()..].split_once(" (")
                && let Some((subject, rest)) = rest.split_once("); ")
                && let Some(verb) = ["cherry-pick", "revert"]
                    .into_iter()
                    .find(|v| rest.contains(&format!("rgit {v} --continue")))
            {
                let mut text = format!("error: could not apply {what}... {subject}");
                let advice = advice();
                if advice {
                    text.push_str(&format!(
                        "\nhint: After resolving the conflicts, mark them with\nhint: \"rgit \
                         add/rm <pathspec>\", then run\nhint: \"rgit {verb} --continue\".\nhint: \
                         You can instead skip this commit with \"rgit {verb} --skip\".\nhint: To \
                         abort and get back to the state before \"rgit {verb}\",\nhint: run \
                         \"rgit {verb} --abort\".\nhint: Disable this message with \"rgit config \
                         set advice.mergeConflict false\""
                    ));
                }
                return (text, 1);
            }
        }
        Some(GitError::DetachedHead) => {
            return match command {
                Some("pull") => (
                    "You are not currently on a branch.\nPlease specify which branch you want \
                     to merge with.\nSee git-pull(1) for details.\n\n    git pull <remote> <branch>\n"
                        .to_owned(),
                    1,
                ),
                Some("push") => fatal(
                    "You are not currently on a branch.\nTo push the history leading to the \
                     current (detached HEAD)\nstate now, use\n\n    git push origin \
                     HEAD:<name-of-remote-branch>\n",
                ),
                _ => fatal("You are not currently on a branch."),
            };
        }
        Some(GitError::Git(e)) if e.message().starts_with("failed to parse config file") => {
            if let Some(bad) = bad_config(e.message()) {
                return fatal(&bad);
            }
        }
        _ => {}
    }
    let (message, help, code) = translate(error);
    if message.is_empty() {
        return (String::new(), code);
    }
    if message == "no git repository found" {
        return fatal(NOT_A_REPO);
    }
    if message.contains('\n') {
        let advice = advice();
        let mut text = if message.starts_with("the following ") {
            format!("error: {message}")
        } else {
            message
        };
        for h in help.iter().filter(|_| advice && cli.is_some()) {
            text.push_str(&format!("\nhint: {h}"));
        }
        return (text, code);
    }
    if message.starts_with("fatal: ") {
        return (message, 128);
    }
    if message.starts_with("error: ") {
        return (message, code);
    }
    if let Some(rev) = message
        .strip_prefix("revspec '")
        .and_then(|r| r.strip_suffix("' not found"))
    {
        return unknown_revision(command, rev);
    }
    if let Some((arg, _)) = message
        .strip_prefix("ambiguous argument '")
        .and_then(|r| r.split_once("': unknown revision"))
    {
        return ambiguous(arg);
    }
    if let Some((_, rest)) = message.split_once("cannot locate local branch '")
        && let Some((name, _)) = rest.split_once('\'')
    {
        return error_line(&format!("branch '{name}' not found"));
    }
    if command == Some("stash") && message == "reference 'refs/stash' not found" {
        return ("No stash entries found.".to_owned(), 1);
    }
    if command == Some("config") && message.ends_with(" is not set") {
        return (String::new(), 1);
    }
    // libgit2's reflog bound, in git's words.
    if let Some(rest) = message.strip_prefix("reflog for 'refs/heads/")
        && let Some((name, rest)) = rest.split_once("' has only ")
        && let Some((n, _)) = rest.split_once(" entries")
    {
        return fatal(&format!("log for '{name}' only has {n} entries"));
    }
    let is_fatal = (message.starts_with("pathspec '")
        && message.ends_with("did not match any files"))
        || crate::plumbing::rev_dies(&message)
        || message.starts_with("invalid reference: ")
        || message.contains(", source=")
        || message.starts_with("no such path '")
        || message == "Needed a single revision";
    if is_fatal || cli.is_some_and(|c| c.code == 128) {
        return fatal(&message);
    }
    match cli.map(|c| c.code) {
        Some(2) if message.starts_with("usage: ") => (message, 129),
        Some(2)
            if message.contains("unknown option")
                || message.contains("requires a value")
                || message.ends_with(" is mandatory") =>
        {
            (format!("error: {message}"), 129)
        }
        Some(2) => fatal(&message),
        _ => (format!("error: {message}"), code),
    }
}

/// Whether git's advice is on (`--no-advice` sets GIT_ADVICE=0).
fn advice() -> bool {
    std::env::var("GIT_ADVICE").map_or(true, |v| v != "0" && v != "false")
}

/// git's report of a revision it cannot resolve, worded as `command` words it.
fn unknown_revision(command: Option<&str>, rev: &str) -> (String, i32) {
    let fatal = |m: String| (format!("fatal: {m}"), 128);
    match command {
        Some("cat-file") => fatal(format!("Not a valid object name {rev}")),
        Some("merge") => (format!("merge: {rev} - not something we can merge"), 1),
        Some("tag") => fatal(format!("Failed to resolve '{rev}' as a valid ref.")),
        Some("branch") => fatal(format!("not a valid object name: '{rev}'")),
        Some("switch") => fatal(format!("invalid reference: {rev}")),
        Some("rebase") => fatal(format!("invalid upstream '{rev}'")),
        Some("cherry-pick" | "revert") => fatal(format!("bad revision '{rev}'")),
        _ => ambiguous(rev),
    }
}

fn ambiguous(arg: &str) -> (String, i32) {
    (
        format!(
            "fatal: ambiguous argument '{arg}': unknown revision or path not in the working \
             tree.\nUse '--' to separate paths from revisions, like this:\n'git <command> \
             [<revision>...] -- [<file>...]'"
        ),
        128,
    )
}

/// libgit2's config parse error as git's `bad config line N in file F`.
fn bad_config(message: &str) -> Option<String> {
    let (_, at) = message.rsplit_once("(in ")?;
    let (path, line) = at.strip_suffix(')')?.rsplit_once(':')?;
    let cwd = std::env::current_dir().ok();
    let shown = cwd
        .and_then(|c| {
            std::path::Path::new(path)
                .strip_prefix(c.canonicalize().ok()?)
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_else(|| path.to_owned());
    Some(format!("bad config line {line} in file {shown}"))
}

/// ASCII stand-ins for the glyphs human output uses.
pub fn sanitize(s: impl AsRef<str>) -> String {
    s.as_ref().replace('\u{2191}', "^")
}
