//! A native rebase sequencer. Its state is git's `.git/rebase-merge`, so a
//! rebase rgit starts can be continued by git and the other way round.

use crate::rev::RevParse;
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use git2::build::CheckoutBuilder;
use git2::{Commit, Oid, Repository, Signature};

use crate::error::GitError;
use crate::git_repo::{
    checked_out_at, checkout, checkout_merged, edit_message, file_favor, pick_labels, short_ref,
    short7, signoff, store_stash,
};

const STOP_FILES: [&str; 5] = ["stopped-sha", "message", "author-script", "amend", "patch"];
const OP_FILES: [&str; 6] = [
    "REBASE_HEAD",
    "MERGE_MSG",
    "MERGE_HEAD",
    "MERGE_MODE",
    "CHERRY_PICK_HEAD",
    "AUTO_MERGE",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cmd {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Exec,
    Break,
    Drop,
    Label,
    Reset,
    Merge,
    UpdateRef,
    Noop,
}

const VERBS: [(&str, &str, Cmd); 13] = [
    ("pick", "p", Cmd::Pick),
    ("reword", "r", Cmd::Reword),
    ("edit", "e", Cmd::Edit),
    ("squash", "s", Cmd::Squash),
    ("fixup", "f", Cmd::Fixup),
    ("exec", "x", Cmd::Exec),
    ("break", "b", Cmd::Break),
    ("drop", "d", Cmd::Drop),
    ("label", "l", Cmd::Label),
    ("reset", "t", Cmd::Reset),
    ("merge", "m", Cmd::Merge),
    ("update-ref", "u", Cmd::UpdateRef),
    ("noop", "noop", Cmd::Noop),
];

impl Cmd {
    fn verb(self) -> &'static str {
        VERBS.iter().find(|v| v.2 == self).map_or("noop", |v| v.0)
    }

    fn takes_commit(self) -> bool {
        matches!(
            self,
            Cmd::Pick | Cmd::Reword | Cmd::Edit | Cmd::Squash | Cmd::Fixup | Cmd::Drop
        )
    }

    fn is_fixup(self) -> bool {
        matches!(self, Cmd::Squash | Cmd::Fixup)
    }
}

/// One todo line: `flag` is fixup's or merge's `C`/`c`, `arg` the exec
/// command, label, ref or merge parent, `note` what follows a commit id
/// (`# subject`) as the user left it.
#[derive(Clone, Debug)]
struct Item {
    cmd: Cmd,
    flag: Option<char>,
    oid: Option<Oid>,
    arg: String,
    note: Option<String>,
}

impl Item {
    fn pick(cmd: Cmd, oid: Oid) -> Self {
        Item {
            cmd,
            flag: None,
            oid: Some(oid),
            arg: String::new(),
            note: None,
        }
    }

    fn with_arg(cmd: Cmd, arg: &str) -> Self {
        Item {
            cmd,
            flag: None,
            oid: None,
            arg: arg.to_owned(),
            note: None,
        }
    }
}

/// How todo lines are written: rebase.abbreviateCommands and
/// rebase.instructionFormat.
#[derive(Default)]
struct Style {
    abbreviate: bool,
    format: Option<String>,
}

impl Style {
    fn load(repo: &Repository) -> Self {
        let cfg = repo.config().ok();
        Style {
            abbreviate: cfg
                .as_ref()
                .and_then(|c| c.get_bool("rebase.abbreviateCommands").ok())
                .unwrap_or(false),
            format: cfg
                .and_then(|c| c.get_string("rebase.instructionFormat").ok())
                .filter(|f| !f.is_empty()),
        }
    }
}

/// A commit through a pretty format's common placeholders (git's
/// rebase.instructionFormat).
// ponytail: the placeholders todo lines use; the full pretty engine lives in
// rgit-cli.
fn pretty(repo: &Repository, oid: Oid, format: &str) -> String {
    let Ok(c) = repo.find_commit(oid) else {
        return String::new();
    };
    // git makes the todo with abbreviation off, so %h is the full id too.
    let short = |o: Oid| o.to_string();
    let msg = message(&c);
    let (subj, body) = match msg.split_once("\n\n") {
        Some((s, b)) => (s.replace('\n', " "), b.to_owned()),
        None => (msg.trim_end().replace('\n', " "), String::new()),
    };
    let (a, m) = (c.author(), c.committer());
    let mut out = String::new();
    let mut rest = format;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        let two = rest.get(..2).unwrap_or("");
        let (text, used) = match two {
            "an" => (a.name().unwrap_or("").to_owned(), 2),
            "ae" => (a.email().unwrap_or("").to_owned(), 2),
            "cn" => (m.name().unwrap_or("").to_owned(), 2),
            "ce" => (m.email().unwrap_or("").to_owned(), 2),
            _ => match rest.chars().next() {
                Some('H') => (oid.to_string(), 1),
                Some('h') => (short(oid), 1),
                Some('T') => (c.tree_id().to_string(), 1),
                Some('t') => (short(c.tree_id()), 1),
                Some('P') => (
                    c.parent_ids()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(" "),
                    1,
                ),
                Some('p') => (c.parent_ids().map(short).collect::<Vec<_>>().join(" "), 1),
                Some('s') => (subj.clone(), 1),
                Some('b') => (body.clone(), 1),
                Some('B') => (msg.clone(), 1),
                Some('n') => ("\n".to_owned(), 1),
                Some('%') => ("%".to_owned(), 1),
                _ => ("%".to_owned(), 0),
            },
        };
        out.push_str(&text);
        rest = &rest[used..];
    }
    out.push_str(rest);
    out
}

fn parse(repo: &Repository, line: &str) -> Result<Option<Item>, GitError> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let bad = || GitError::Other(format!("invalid todo line: {line}"));
    let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let cmd = VERBS
        .iter()
        .find(|v| v.0 == word || v.1 == word)
        .ok_or_else(bad)?
        .2;
    let rest = rest.trim();
    let commit = |rev: &str| -> Result<Oid, GitError> {
        Ok(repo
            .rev_single(rev)
            .and_then(|o| o.peel_to_commit())
            .map_err(|_| bad())?
            .id())
    };
    let mut item = Item::with_arg(cmd, "");
    let mut words = rest.split_whitespace().peekable();
    match cmd {
        Cmd::Exec => item.arg = rest.to_owned(),
        Cmd::Break | Cmd::Noop => {}
        Cmd::Label | Cmd::Reset | Cmd::UpdateRef => {
            let arg = rest.split(" # ").next().unwrap_or("").trim();
            if arg.is_empty() {
                return Err(bad());
            }
            item.arg = arg.to_owned();
        }
        Cmd::Merge => {
            if let Some(f) = words.peek().and_then(|w| w.strip_prefix('-')) {
                item.flag = f.chars().next().filter(|c| matches!(c, 'C' | 'c'));
                item.flag.ok_or_else(bad)?;
                words.next();
                item.oid = Some(commit(words.next().ok_or_else(bad)?)?);
            }
            item.arg = words
                .next()
                .filter(|w| *w != "#")
                .ok_or_else(bad)?
                .to_owned();
        }
        _ => {
            let mut body = rest;
            if cmd == Cmd::Fixup
                && let Some(f) = body.strip_prefix('-')
            {
                item.flag = f.chars().next().filter(|c| matches!(c, 'C' | 'c'));
                item.flag.ok_or_else(bad)?;
                body = f[1..].trim_start();
            }
            let (id, note) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
            if id.is_empty() {
                return Err(bad());
            }
            item.oid = Some(commit(id)?);
            let note = note.trim();
            item.note = (!note.is_empty()).then(|| note.to_owned());
        }
    }
    Ok(Some(item))
}

fn subject(repo: &Repository, oid: Oid) -> String {
    repo.find_commit(oid)
        .ok()
        .and_then(|c| c.summary().ok().flatten().map(str::to_owned))
        .unwrap_or_default()
}

/// A todo line; `short` abbreviates the commit ids, as the editor shows them.
fn render(repo: &Repository, item: &Item, short: bool, style: &Style) -> String {
    let verb = |cmd: Cmd| match VERBS.iter().find(|v| v.2 == cmd) {
        Some(v) if style.abbreviate => v.1,
        _ => cmd.verb(),
    };
    let id = |oid: Oid| {
        if short {
            abbrev(repo, oid)
        } else {
            oid.to_string()
        }
    };
    let flag = item.flag.map(|f| format!("-{f} ")).unwrap_or_default();
    match (item.cmd, item.oid) {
        (Cmd::Merge, oid) => {
            let from = oid.map(|o| format!("{flag}{} ", id(o))).unwrap_or_default();
            let about = oid
                .map(|o| format!(" # {}", subject(repo, o)))
                .unwrap_or_default();
            format!("{} {from}{}{about}", verb(Cmd::Merge), item.arg)
        }
        (cmd, Some(oid)) => format!(
            "{} {flag}{} {}",
            verb(cmd),
            id(oid),
            note(repo, item, oid, style)
        ),
        (Cmd::Break | Cmd::Noop, None) => verb(item.cmd).to_owned(),
        (cmd, None) => format!("{} {}", verb(cmd), item.arg),
    }
}

/// `oid` abbreviated as git's find_unique_abbrev does (core.abbrev).
fn abbrev(repo: &Repository, oid: Oid) -> String {
    repo.find_object(oid, None)
        .ok()
        .and_then(|o| o.short_id().ok())
        .and_then(|b| b.as_str().ok().map(str::to_owned))
        .unwrap_or_else(|| short7(oid))
}

/// What follows a todo line's commit id: the user's text, else `# ` and the
/// commit in rebase.instructionFormat (default its subject).
fn note(repo: &Repository, item: &Item, oid: Oid, style: &Style) -> String {
    match (&item.note, &style.format) {
        (Some(n), _) => n.clone(),
        (None, Some(f)) => format!("# {}", pretty(repo, oid, f)),
        (None, None) => format!("# {}", subject(repo, oid)),
    }
}

/// git's append_todo_help: `shortrevisions` and `shortonto` for the first
/// edit, `None` for `--edit-todo`.
fn todo_help(repo: &Repository, count: usize, range: Option<(&str, &str)>) -> String {
    let mut out = String::new();
    if let Some((revs, onto)) = range {
        let s = if count == 1 { "" } else { "s" };
        out.push_str(&format!(
            "\n# Rebase {revs} onto {onto} ({count} command{s})\n"
        ));
    }
    out.push_str(HELP);
    out.push_str(if check_level(repo) == Check::Error {
        "#\n# Do not remove any line. Use 'drop' explicitly to remove a commit.\n"
    } else {
        "#\n# If you remove a line here THAT COMMIT WILL BE LOST.\n"
    });
    out.push_str(if range.is_some() {
        "#\n# However, if you remove everything, the rebase will be aborted.\n#\n"
    } else {
        "#\n# You are editing the todo file of an ongoing interactive rebase.\n# To continue \
         rebase after editing, run:\n#     git rebase --continue\n#\n"
    });
    out
}

#[derive(PartialEq, Eq)]
enum Check {
    Ignore,
    Warn,
    Error,
}

fn check_level(repo: &Repository) -> Check {
    let v = repo
        .config()
        .and_then(|c| c.get_string("rebase.missingCommitsCheck"))
        .unwrap_or_default();
    match v.to_ascii_lowercase().as_str() {
        "warn" => Check::Warn,
        "error" => Check::Error,
        "ignore" | "" => Check::Ignore,
        _ => {
            eprintln!(
                "warning: unrecognized setting {v} for option rebase.missingCommitsCheck. \
                 Ignoring."
            );
            Check::Ignore
        }
    }
}

const EDIT_TODO_ADVICE: &str = "You can fix this with 'git rebase --edit-todo' and then run 'git \
                                rebase --continue'.\nOr you can abort the rebase with 'git \
                                rebase --abort'.\n";

/// git's todo_list_check: warn about the commits of `old` that `new` lost;
/// `true` when rebase.missingCommitsCheck=error makes that an error.
fn missing_commits(repo: &Repository, old: &[Item], new: &[Item], style: &Style) -> bool {
    let level = check_level(repo);
    if level == Check::Ignore {
        return false;
    }
    let mut seen: HashSet<Oid> = new.iter().filter_map(|i| i.oid).collect();
    let mut missing = String::new();
    for item in old.iter().rev() {
        if let Some(oid) = item.oid.filter(|o| seen.insert(*o)) {
            let note = note(repo, item, oid, style);
            missing.push_str(&format!(" - {} {note}\n", abbrev(repo, oid)));
        }
    }
    if missing.is_empty() {
        return false;
    }
    eprint!(
        "Warning: some commits may have been dropped accidentally.\nDropped commits (newer to \
         older):\n{missing}To avoid this message, use \"drop\" to explicitly remove a \
         commit.\n\nUse 'git config rebase.missingCommitsCheck' to change the level of \
         warnings.\nThe possible behaviours are: ignore, warn, error.\n\n{EDIT_TODO_ADVICE}"
    );
    level == Check::Error
}

/// The todo lines of `text`, skipping comments.
fn parse_all(repo: &Repository, text: &str) -> Result<Vec<Item>, GitError> {
    let mut items = Vec::new();
    for line in text.lines() {
        items.extend(parse(repo, line)?);
    }
    Ok(items)
}

fn todo_body(repo: &Repository, items: &[Item], short: bool, style: &Style) -> String {
    items
        .iter()
        .map(|i| format!("{}\n", render(repo, i, short, style)))
        .collect()
}

fn read(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

fn write(dir: &Path, name: &str, text: impl AsRef<[u8]>) -> Result<(), GitError> {
    std::fs::write(dir.join(name), text)?;
    Ok(())
}

fn read_oid(dir: &Path, name: &str) -> Option<Oid> {
    read(dir, name).and_then(|s| Oid::from_str(s.trim()).ok())
}

fn state_dir(repo: &Repository) -> PathBuf {
    repo.path().join("rebase-merge")
}

/// git's `strip` cleanup: no comment lines, no trailing blanks, runs of blank
/// lines collapsed and none at either end.
fn cleanup(msg: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in msg.lines().filter(|l| !l.starts_with('#')) {
        let line = line.trim_end();
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last() == Some(&"") {
        out.pop();
    }
    if out.is_empty() {
        String::new()
    } else {
        format!("{}\n", out.join("\n"))
    }
}

fn commented(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                "#\n".to_owned()
            } else {
                format!("# {l}\n")
            }
        })
        .collect()
}

fn message(commit: &Commit) -> String {
    String::from_utf8_lossy(commit.message_raw_bytes()).into_owned()
}

fn zone(offset: i32) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    format!("{sign}{:02}{:02}", offset.abs() / 60, offset.abs() % 60)
}

fn author_script(sig: &Signature) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let when = sig.when();
    format!(
        "GIT_AUTHOR_NAME={}\nGIT_AUTHOR_EMAIL={}\nGIT_AUTHOR_DATE={}\n",
        q(sig.name().unwrap_or("")),
        q(sig.email().unwrap_or("")),
        q(&format!(
            "@{} {}",
            when.seconds(),
            zone(when.offset_minutes())
        ))
    )
}

fn read_author_script(text: &str) -> Option<Signature<'static>> {
    let mut vars = HashMap::new();
    for line in text.lines() {
        let (key, value) = line.split_once('=')?;
        let value = value.trim().trim_start_matches('\'').trim_end_matches('\'');
        vars.insert(key.trim(), value.replace("'\\''", "'"));
    }
    let (secs, offset) = crate::plumbing::parse_git_date(vars.get("GIT_AUTHOR_DATE")?, 0)?;
    Signature::new(
        vars.get("GIT_AUTHOR_NAME")?,
        vars.get("GIT_AUTHOR_EMAIL")?,
        &git2::Time::new(secs, offset),
    )
    .ok()
}

/// The committer, honoring GIT_COMMITTER_NAME/EMAIL/DATE as git does.
fn committer(repo: &Repository) -> Result<Signature<'static>, GitError> {
    let ident = crate::plumbing::ident(repo, true)?;
    let bad = || GitError::Other(format!("bad committer identity: {ident}"));
    let (who, when) = ident.rsplit_once('>').ok_or_else(bad)?;
    let (name, email) = who.split_once('<').ok_or_else(bad)?;
    let (secs, offset) = crate::plumbing::parse_git_date(when, 0).ok_or_else(bad)?;
    Ok(Signature::new(
        name.trim(),
        email.trim(),
        &git2::Time::new(secs, offset),
    )?)
}

fn may_edit(sequence: bool) -> bool {
    let set = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty());
    std::io::stdin().is_terminal() || set("GIT_EDITOR") || (sequence && set("GIT_SEQUENCE_EDITOR"))
}

/// Edit the todo in the sequence editor (GIT_SEQUENCE_EDITOR, sequence.editor,
/// then the commit message editor).
fn edit_todo_file(repo: &Repository, path: &Path) -> Result<(), GitError> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let config = repo.config()?;
    let editor = env("GIT_SEQUENCE_EDITOR")
        .or_else(|| config.get_string("sequence.editor").ok())
        .or_else(|| env("GIT_EDITOR"))
        .or_else(|| config.get_string("core.editor").ok())
        .or_else(|| env("VISUAL"))
        .or_else(|| env("EDITOR"))
        .unwrap_or_else(|| "vi".to_owned());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor)
        .arg(path)
        .current_dir(repo.workdir().unwrap_or(repo.path()))
        .status()
        .map_err(|e| GitError::Cli(format!("could not run the editor: {e}")))?;
    if !status.success() {
        return Err(GitError::Other(format!(
            "there was a problem with the editor '{editor}'"
        )));
    }
    Ok(())
}

const HELP: &str = "#
# Commands:
# p, pick <commit> = use commit
# r, reword <commit> = use commit, but edit the commit message
# e, edit <commit> = use commit, but stop for amending
# s, squash <commit> = use commit, but meld into previous commit
# f, fixup [-C | -c] <commit> = like \"squash\" but keep only the previous
#                    commit's log message, unless -C is used, in which case
#                    keep only this commit's message; -c is same as -C but
#                    opens the editor
# x, exec <command> = run command (the rest of the line) using shell
# b, break = stop here (continue rebase later with 'git rebase --continue')
# d, drop <commit> = remove commit
# l, label <label> = label current HEAD with a name
# t, reset <label> = reset HEAD to a label
# m, merge [-C <commit> | -c <commit>] <label> [# <oneline>]
#         create a merge commit using the original merge commit's
#         message (or the oneline, if no original merge commit was
#         specified); use -c <commit> to reword the commit message
# u, update-ref <ref> = track a placeholder for the <ref> to be updated
#                       to this position in the new commits. The <ref> is
#                       updated at the end of the rebase
#
# These lines can be re-ordered; they are executed from top to bottom.
";

/// Run hook `name` if there is one, its output on stderr; whether it passed.
fn run_hook(repo: &Repository, name: &str, args: &[&str], stdin: &str) -> Result<bool, GitError> {
    use std::io::Write;
    let workdir = repo.workdir().unwrap_or(repo.path());
    let dir = crate::git_repo::hooks_dir(repo);
    let Some(out) = crate::git_repo::run_hook(&dir, workdir, name, args, Some(stdin.as_bytes()))?
    else {
        return Ok(true);
    };
    let mut err = std::io::stderr();
    let _ = err.write_all(&out.stdout);
    let _ = err.write_all(&out.stderr);
    Ok(out.status.success())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Empty {
    Drop,
    Keep,
    Stop,
}

/// A rebase in progress, read from its state directory.
struct Seq<'r> {
    repo: &'r Repository,
    dir: PathBuf,
    strategy: Option<String>,
    signoff: bool,
    cdate: bool,
    ignore_date: bool,
    allow_ff: bool,
    empty: Empty,
    quiet: bool,
    squash_onto: Option<Oid>,
    /// The key commits are signed with (git's gpg_sign_opt), `""` for the
    /// default one.
    gpg: Option<String>,
    out: Vec<String>,
}

impl<'r> Seq<'r> {
    fn load(repo: &'r Repository) -> Result<Self, GitError> {
        let dir = state_dir(repo);
        if !dir.join("head-name").exists() {
            return Err(GitError::Other("no rebase in progress".into()));
        }
        let has = |f: &str| dir.join(f).exists();
        let strategy = read(&dir, "strategy_opts").and_then(|s| {
            s.split_whitespace()
                .map(|w| w.trim_matches(['"', '\'']).trim_start_matches("--"))
                .rfind(|w| !w.is_empty())
                .map(str::to_owned)
        });
        let (signoff, cdate, ignore_date) =
            (has("signoff"), has("cdate_is_adate"), has("ignore_date"));
        let empty = if has("drop_redundant_commits") {
            Empty::Drop
        } else if has("keep_redundant_commits") {
            Empty::Keep
        } else {
            Empty::Stop
        };
        Ok(Seq {
            repo,
            strategy,
            signoff,
            cdate,
            ignore_date,
            allow_ff: !(signoff || cdate || ignore_date),
            empty,
            quiet: has("quiet"),
            squash_onto: read_oid(&dir, "squash-onto"),
            gpg: read(&dir, "gpg_sign_opt")
                .and_then(|s| s.trim().strip_prefix("-S").map(str::to_owned)),
            out: Vec::new(),
            dir,
        })
    }

    fn lines(&self, name: &str) -> Vec<String> {
        read(&self.dir, name)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn head(&self) -> Result<Commit<'r>, GitError> {
        Ok(self.repo.head()?.peel_to_commit()?)
    }

    /// The next command still to do, if any.
    fn peek(&self) -> Option<Cmd> {
        self.lines("git-rebase-todo")
            .iter()
            .find_map(|l| parse(self.repo, l).ok().flatten())
            .map(|i| i.cmd)
    }

    fn set_head(&self, oid: Oid, why: &str) -> Result<(), GitError> {
        self.repo.reference("HEAD", oid, true, why)?;
        Ok(())
    }

    /// Move HEAD, the index and the working tree to `oid`.
    fn move_to(&self, oid: Oid, why: &str) -> Result<(), GitError> {
        let commit = self.repo.find_commit(oid)?;
        self.repo
            .checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe()))?;
        self.set_head(oid, why)
    }

    fn rewritten(&self, old: Oid, new: Oid, replacing: Option<Oid>) -> Result<(), GitError> {
        let mut list: Vec<String> = self.lines("rewritten-list");
        if let Some(prev) = replacing.map(|o| o.to_string()) {
            for line in &mut list {
                if let Some((from, to)) = line.split_once(' ')
                    && to == prev
                {
                    *line = format!("{from} {new}");
                }
            }
        }
        list.push(format!("{old} {new}"));
        write(
            &self.dir,
            "rewritten-list",
            format!("{}\n", list.join("\n")),
        )
    }

    fn commit(
        &self,
        author: &Signature,
        msg: &str,
        tree: Oid,
        parents: &[&Commit],
    ) -> Result<Oid, GitError> {
        let mut committer = committer(self.repo)?;
        if self.cdate {
            committer = Signature::new(
                committer.name().unwrap_or(""),
                committer.email().unwrap_or(""),
                &author.when(),
            )?;
        }
        let tree = self.repo.find_tree(tree)?;
        crate::sign::commit(
            self.repo,
            None,
            author,
            &committer,
            msg,
            &tree,
            parents,
            self.gpg.as_deref(),
        )
    }

    fn author_of(&self, commit: &Commit) -> Result<Signature<'static>, GitError> {
        let a = commit.author();
        if !self.ignore_date {
            return Ok(a.to_owned());
        }
        Ok(Signature::now(
            a.name().unwrap_or(""),
            a.email().unwrap_or(""),
        )?)
    }

    fn empty_tree(&self) -> Result<Oid, GitError> {
        Ok(self.repo.treebuilder(None)?.write()?)
    }

    /// The empty root commit that `--root` without `--onto` replays onto.
    fn new_root(&mut self) -> Result<Oid, GitError> {
        if let Some(oid) = self.squash_onto {
            return Ok(oid);
        }
        let sig = committer(self.repo)?;
        let tree = self.repo.find_tree(self.empty_tree()?)?;
        let oid = self.repo.commit(None, &sig, &sig, "", &tree, &[])?;
        write(&self.dir, "squash-onto", format!("{oid}\n"))?;
        self.squash_onto = Some(oid);
        Ok(oid)
    }

    fn resolve(&mut self, label: &str) -> Result<Oid, GitError> {
        if label == "[new root]" {
            return self.new_root();
        }
        if let Ok(r) = self.repo.find_reference(&format!("refs/rewritten/{label}"))
            && let Some(oid) = r.target()
        {
            return Ok(oid);
        }
        self.repo
            .rev_single(label)
            .and_then(|o| o.peel_to_commit())
            .map(|c| c.id())
            .map_err(|_| GitError::Other(format!("could not resolve '{label}'")))
    }

    fn merge_opts(&self) -> Result<git2::MergeOptions, GitError> {
        let mut opts = git2::MergeOptions::new();
        if let Some(side) = &self.strategy {
            opts.file_favor(file_favor(side)?);
        }
        Ok(opts)
    }

    /// Record a stop at `commit` for `--continue`: REBASE_HEAD and what the
    /// commit is to be made with.
    fn record_stop(&self, commit: &Commit, msg: &str, author: &Signature) -> Result<(), GitError> {
        let git = self.repo.path();
        write(git, "REBASE_HEAD", format!("{}\n", commit.id()))?;
        write(&self.dir, "stopped-sha", format!("{}\n", commit.id()))?;
        write(&self.dir, "message", msg)?;
        write(git, "MERGE_MSG", msg)?;
        write(&self.dir, "author-script", author_script(author))
    }

    /// Stop for the conflicts `index` has from applying `commit` (`trees`:
    /// base, ours, theirs), reporting as git does; `note` ends the todo line.
    fn conflict(
        &self,
        index: &git2::Index,
        commit: &Commit,
        trees: [&git2::Tree; 3],
        note: &str,
    ) -> Result<GitError, GitError> {
        let theirs = pick_labels(commit, false)[2].clone();
        let mut lines = crate::git_repo::merge_report(self.repo, index, trees, &theirs)?;
        let mut paths: Vec<String> = index
            .conflicts()?
            .flatten()
            .filter_map(|c| c.our.or(c.their))
            .map(|e| String::from_utf8_lossy(&e.path).into_owned())
            .collect();
        paths.dedup();
        let msg = read(&self.dir, "message").unwrap_or_default();
        let mut msg = msg.trim_end().to_owned();
        msg.push_str("\n\n# Conflicts:\n");
        for p in &paths {
            msg.push_str(&format!("#\t{p}\n"));
        }
        write(&self.dir, "message", &msg)?;
        write(self.repo.path(), "MERGE_MSG", &msg)?;
        let short = abbrev(self.repo, commit.id());
        let subject = commit.summary().ok().flatten().unwrap_or("").to_owned();
        lines.push(format!("error: could not apply {short}... {subject}"));
        let auto =
            read(&self.dir, "allow_rerere_autoupdate").map(|s| s.trim() == "--rerere-autoupdate");
        let tail = crate::git_repo::conflict_advice(self.repo, "rebase", false)
            + &crate::rerere::report(self.repo, auto);
        lines.extend(tail.lines().map(str::to_owned));
        lines.push(format!("Could not apply {short}... {note}"));
        Ok(GitError::Conflict(self.with_out(lines)))
    }

    fn with_out(&self, lines: Vec<String>) -> String {
        self.out
            .iter()
            .cloned()
            .chain(lines)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Fold `commit` into the squash message being built (git's
    /// `message-squash`), returning it.
    fn squash_message(
        &self,
        head: &Commit,
        commit: &Commit,
        item: &Item,
    ) -> Result<String, GitError> {
        let fixups = self.lines("current-fixups");
        let n = fixups.len() + 2;
        let mut buf = match read(&self.dir, "message-squash").filter(|_| !fixups.is_empty()) {
            Some(prev) => {
                let rest = prev.split_once('\n').map_or("", |(_, r)| r);
                format!("# This is a combination of {n} commits.\n{rest}")
            }
            None => format!(
                "# This is a combination of 2 commits.\n# This is the 1st commit message:\n\n{}",
                message(head)
            ),
        };
        if !buf.ends_with('\n') {
            buf.push('\n');
        }
        let body = message(commit);
        match (item.cmd, item.flag) {
            (Cmd::Fixup, None) => {
                buf.push_str(&format!(
                    "\n# The commit message #{n} will be skipped:\n\n{}",
                    commented(&body)
                ));
            }
            (Cmd::Fixup, Some(_)) => {
                buf = commented_all(&buf);
                let body = match body.strip_prefix("amend! ") {
                    Some(rest) => rest
                        .split_once('\n')
                        .map_or("", |(_, r)| r)
                        .trim_start_matches('\n')
                        .to_owned(),
                    None => body,
                };
                buf.push_str(&format!("\n# This is the commit message #{n}:\n\n{body}"));
            }
            _ => {
                let body = match body.split_once('\n') {
                    Some((first, rest))
                        if ["squash! ", "fixup! ", "amend! "]
                            .iter()
                            .any(|p| first.starts_with(p)) =>
                    {
                        format!("# {first}\n{rest}")
                    }
                    _ => body,
                };
                buf.push_str(&format!("\n# This is the commit message #{n}:\n\n{body}"));
            }
        }
        write(&self.dir, "message-squash", &buf)?;
        let verb = match (item.cmd, item.flag) {
            (Cmd::Squash, _) => "squash".to_owned(),
            (_, Some(f)) => format!("fixup -{f}"),
            _ => "fixup".to_owned(),
        };
        let mut fixups = fixups;
        fixups.push(format!("{verb} {}", commit.id()));
        write(
            &self.dir,
            "current-fixups",
            format!("{}\n", fixups.join("\n")),
        )?;
        Ok(buf)
    }

    /// The squash chain's final message once its last fixup is in: the editor
    /// opens when a squash or `fixup -c` asked for it.
    fn end_chain(&self, buf: &str) -> Result<String, GitError> {
        let fixups = self.lines("current-fixups");
        let _ = std::fs::remove_file(self.dir.join("current-fixups"));
        let _ = std::fs::remove_file(self.dir.join("message-squash"));
        let edit = fixups
            .iter()
            .any(|l| l.starts_with("squash") || l.starts_with("fixup -c"));
        if edit && may_edit(false) {
            return Ok(cleanup(&edit_message(self.repo, "COMMIT_EDITMSG", buf)?));
        }
        Ok(cleanup(buf))
    }

    fn reword(&self, msg: &str) -> Result<String, GitError> {
        if may_edit(false) {
            Ok(cleanup(&edit_message(self.repo, "COMMIT_EDITMSG", msg)?))
        } else {
            Ok(msg.to_owned())
        }
    }

    fn stop_edit(&self, commit: &Commit, new: Oid) -> Result<Option<String>, GitError> {
        self.record_stop(commit, &message(commit), &commit.author())?;
        write(&self.dir, "amend", format!("{new}\n"))?;
        Ok(Some(self.with_out(vec![
            format!(
                "Stopped at {}...  {}",
                short7(commit.id()),
                commit.summary().ok().flatten().unwrap_or("")
            ),
            "You can amend the commit now, with `rgit commit --amend`; once you are \
             satisfied, run `rgit rebase --continue`"
                .to_owned(),
        ])))
    }

    fn pick(&mut self, item: &Item) -> Result<Option<String>, GitError> {
        let repo = self.repo;
        let commit = repo.find_commit(item.oid.expect("a pick names a commit"))?;
        let head = self.head()?;
        let rootish = self.squash_onto == Some(head.id());
        let fixup = item.cmd.is_fixup();
        let subject = commit.summary().ok().flatten().unwrap_or("").to_owned();
        if fixup && rootish {
            return Err(GitError::Other(format!(
                "cannot '{}' without a previous commit",
                item.cmd.verb()
            )));
        }
        if commit.parent_count() > 1 {
            return Err(GitError::Other(format!(
                "commit {} is a merge; rebase it with --rebase-merges",
                short7(commit.id())
            )));
        }
        let parent = commit.parent_ids().next();
        if !fixup && self.allow_ff && (parent == Some(head.id()) || (parent.is_none() && rootish)) {
            self.move_to(commit.id(), &format!("rebase (pick): {subject}"))?;
            let mut new = commit.id();
            if item.cmd == Cmd::Reword {
                let msg = self.reword(&message(&commit))?;
                if msg != message(&commit) {
                    let parents: Vec<Commit> = commit.parents().collect();
                    let parents: Vec<&Commit> = parents.iter().collect();
                    new = self.commit(&commit.author(), &msg, commit.tree_id(), &parents)?;
                    self.set_head(new, &format!("rebase (reword): {subject}"))?;
                    self.rewritten(commit.id(), new, None)?;
                }
            }
            return if item.cmd == Cmd::Edit {
                self.stop_edit(&commit, new)
            } else {
                Ok(None)
            };
        }
        let empty_tree = repo.find_tree(self.empty_tree()?)?;
        let base = match commit.parent(0) {
            Ok(p) => p.tree()?,
            Err(_) => empty_tree.clone(),
        };
        let ours = if rootish {
            empty_tree.clone()
        } else {
            head.tree()?
        };
        let mut index =
            repo.merge_trees(&base, &ours, &commit.tree()?, Some(&self.merge_opts()?))?;
        let mut msg = message(&commit);
        if self.signoff {
            msg = signoff(&msg, &committer(repo)?);
        }
        let author = if fixup {
            head.author().to_owned()
        } else {
            self.author_of(&commit)?
        };
        if index.has_conflicts() {
            checkout_merged(
                repo,
                &mut index,
                &head.tree()?,
                "rebase",
                Some(&pick_labels(&commit, false)),
            )?;
            let text = if fixup {
                write(&self.dir, "amend", format!("{}\n", head.id()))?;
                cleanup(&self.squash_message(&head, &commit, item)?)
            } else {
                msg
            };
            self.record_stop(&commit, &text, &author)?;
            let note = note(repo, item, commit.id(), &Style::load(repo));
            let trees = [&base, &ours, &commit.tree()?];
            return Err(self.conflict(&index, &commit, trees, &note)?);
        }
        let tree = index.write_tree_to(repo)?;
        let was_empty = base.id() == commit.tree_id();
        if !fixup && tree == ours.id() && !was_empty {
            match self.empty {
                Empty::Drop => return Ok(None),
                Empty::Keep => {}
                Empty::Stop => {
                    self.record_stop(&commit, &msg, &author)?;
                    return Err(GitError::Conflict(self.with_out(vec![format!(
                        "{} ({subject}) is now empty; run `rgit rebase --skip` to drop it, or \
                         `rgit commit --allow-empty` then `rgit rebase --continue` to keep it",
                        short7(commit.id())
                    )])));
                }
            }
        }
        let new = if fixup {
            let buf = self.squash_message(&head, &commit, item)?;
            let text = if self.peek().is_some_and(Cmd::is_fixup) {
                cleanup(&buf)
            } else {
                self.end_chain(&buf)?
            };
            let parents: Vec<Commit> = head.parents().collect();
            let parents: Vec<&Commit> = parents.iter().collect();
            let new = self.commit(&author, &text, tree, &parents)?;
            self.rewritten(commit.id(), new, Some(head.id()))?;
            new
        } else {
            if item.cmd == Cmd::Reword {
                msg = self.reword(&msg)?;
            }
            let parents: Vec<&Commit> = if rootish { vec![] } else { vec![&head] };
            let new = self.commit(&author, &msg, tree, &parents)?;
            self.rewritten(commit.id(), new, None)?;
            new
        };
        repo.checkout_tree(
            repo.find_tree(tree)?.as_object(),
            Some(CheckoutBuilder::new().safe()),
        )?;
        self.set_head(new, &format!("rebase ({}): {subject}", item.cmd.verb()))?;
        if item.cmd == Cmd::Edit {
            return self.stop_edit(&commit, new);
        }
        Ok(None)
    }

    fn merge(&mut self, item: &Item) -> Result<Option<String>, GitError> {
        let repo = self.repo;
        let head = self.head()?;
        let target = repo.find_commit(self.resolve(&item.arg)?)?;
        let orig = item.oid.map(|o| repo.find_commit(o)).transpose()?;
        if let Some(orig) = &orig
            && self.allow_ff
            && item.flag != Some('c')
            && orig.parent_ids().collect::<Vec<_>>() == [head.id(), target.id()]
        {
            return self
                .move_to(orig.id(), "rebase (merge): fast-forward")
                .map(|()| None);
        }
        let bases = crate::git_repo::all_merge_bases(repo, head.id(), target.id())?;
        let base = crate::git_repo::merge_base_tree(repo, &bases)?;
        let mut index = repo.merge_trees(
            &base,
            &head.tree()?,
            &target.tree()?,
            Some(&self.merge_opts()?),
        )?;
        let msg = match &orig {
            Some(o) => message(o),
            None => format!("Merge branch '{}'\n", item.arg),
        };
        let author = match &orig {
            Some(o) => self.author_of(o)?,
            None => committer(repo)?,
        };
        if index.has_conflicts() {
            let labels = [
                "merged common ancestors".to_owned(),
                "HEAD".to_owned(),
                item.arg.clone(),
            ];
            checkout_merged(repo, &mut index, &head.tree()?, "rebase", Some(&labels))?;
            let shown = orig.as_ref().unwrap_or(&target);
            self.record_stop(shown, &msg, &author)?;
            write(repo.path(), "MERGE_HEAD", format!("{}\n", target.id()))?;
            let note = format!("{} # {}", item.arg, subject(repo, shown.id()));
            let trees = [&base, &head.tree()?, &target.tree()?];
            return Err(self.conflict(&index, shown, trees, &note)?);
        }
        let msg = if item.flag == Some('c') {
            self.reword(&msg)?
        } else {
            msg
        };
        let tree = index.write_tree_to(repo)?;
        let new = self.commit(&author, &msg, tree, &[&head, &target])?;
        if let Some(o) = &orig {
            self.rewritten(o.id(), new, None)?;
        }
        repo.checkout_tree(
            repo.find_tree(tree)?.as_object(),
            Some(CheckoutBuilder::new().safe()),
        )?;
        self.set_head(new, &format!("rebase (merge): {}", item.arg))?;
        Ok(None)
    }

    fn exec(&mut self, cmd: &str) -> Result<Option<String>, GitError> {
        let workdir = self.repo.workdir().unwrap_or(self.repo.path());
        self.out.push(format!("Executing: {cmd}"));
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::io::stderr())
            .status()?;
        crate::git_repo::sync_index(self.repo)?;
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(false);
        let dirty = self
            .repo
            .statuses(Some(&mut opts))?
            .iter()
            .any(|e| e.status() != git2::Status::CURRENT);
        let why = match (status.success(), dirty) {
            (true, false) => return Ok(None),
            (false, false) => format!("warning: execution failed: {cmd}"),
            (ok, true) => format!(
                "warning: execution {}: {cmd}\nand made changes to the index and/or the working \
                 tree; commit or stash them",
                if ok { "succeeded" } else { "failed" }
            ),
        };
        Err(GitError::Conflict(self.with_out(vec![
            why,
            "fix the problem, then run `rgit rebase --continue`".to_owned(),
        ])))
    }

    fn step(&mut self, item: &Item) -> Result<Option<String>, GitError> {
        match item.cmd {
            Cmd::Noop | Cmd::Drop => Ok(None),
            Cmd::Break => {
                let head = self.head()?;
                Ok(Some(self.with_out(vec![format!(
                    "Stopped at {}...  {}\nrun `rgit rebase --continue` to go on",
                    short7(head.id()),
                    head.summary().ok().flatten().unwrap_or("")
                )])))
            }
            Cmd::Exec => self.exec(&item.arg),
            Cmd::Label => {
                let head = self.head()?.id();
                let name = format!("refs/rewritten/{}", item.arg);
                self.repo.reference(&name, head, true, "rebase (label)")?;
                let mut refs = self.lines("refs-to-delete");
                refs.push(name);
                write(
                    &self.dir,
                    "refs-to-delete",
                    format!("{}\n", refs.join("\n")),
                )?;
                Ok(None)
            }
            Cmd::Reset => {
                let oid = self.resolve(&item.arg)?;
                let commit = self.repo.find_commit(oid)?;
                hard_reset(self.repo, &commit)?;
                self.set_head(oid, &format!("rebase (reset): {}", item.arg))?;
                Ok(None)
            }
            Cmd::UpdateRef => {
                let head = self.head()?.id();
                let mut refs = read_update_refs(&self.dir);
                if let Some(entry) = refs.iter_mut().find(|e| e.0 == item.arg) {
                    entry.2 = head;
                }
                write_update_refs(&self.dir, &refs)?;
                Ok(None)
            }
            Cmd::Merge => self.merge(item),
            _ => self.pick(item),
        }
    }

    fn run(mut self) -> Result<String, GitError> {
        loop {
            let todo = self.lines("git-rebase-todo");
            let mut found = None;
            for (i, line) in todo.iter().enumerate() {
                if let Some(item) = parse(self.repo, line)? {
                    found = Some((i, item));
                    break;
                }
            }
            let Some((i, item)) = found else {
                return self.finish();
            };
            let mut done = self.lines("done");
            done.push(todo[i].clone());
            write(&self.dir, "done", format!("{}\n", done.join("\n")))?;
            let rest = &todo[i + 1..];
            write(
                &self.dir,
                "git-rebase-todo",
                if rest.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", rest.join("\n"))
                },
            )?;
            let n: usize = read(&self.dir, "msgnum")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            write(&self.dir, "msgnum", format!("{}\n", n + 1))?;
            if let Some(stop) = self.step(&item)? {
                return Ok(stop);
            }
        }
    }

    fn finish(self) -> Result<String, GitError> {
        let repo = self.repo;
        let head = self.head()?.id();
        let head_name = read(&self.dir, "head-name").unwrap_or_default();
        let head_name = head_name.trim();
        let onto = read(&self.dir, "onto").unwrap_or_default();
        let mut out = self.out.clone();
        if head_name.starts_with("refs/") {
            repo.reference(
                head_name,
                head,
                true,
                &format!("rebase (finish): {head_name} onto {}", onto.trim()),
            )?;
            repo.set_head(head_name)?;
        }
        let updated: Vec<(String, Oid, Oid)> = read_update_refs(&self.dir)
            .into_iter()
            .filter(|e| !e.2.is_zero())
            .collect();
        for (name, _, to) in &updated {
            repo.reference(name, *to, true, "rebase (update-refs)")?;
        }
        let rewritten = read(&self.dir, "rewritten-list").unwrap_or_default();
        if !rewritten.is_empty() {
            let _ = run_hook(repo, "post-rewrite", &["rebase"], &rewritten);
            rewrite_notes(repo, &rewritten);
        }
        let autostash = read_oid(&self.dir, "autostash");
        let quiet = self.quiet;
        cleanup_state(repo)?;
        if !quiet {
            let name = if head_name.starts_with("refs/") {
                head_name
            } else {
                "detached HEAD"
            };
            out.push(format!("Successfully rebased and updated {name}."));
            if !updated.is_empty() {
                out.push("Updated the following refs with --update-refs:".to_owned());
                out.extend(updated.iter().map(|(n, _, _)| format!("\t{n}")));
            }
        }
        out.extend(apply_autostash(repo, autostash)?);
        Ok(out.join("\n"))
    }
}

fn commented_all(buf: &str) -> String {
    buf.lines()
        .map(|l| {
            if l.starts_with('#') || l.is_empty() {
                format!("{l}\n")
            } else {
                format!("# {l}\n")
            }
        })
        .collect()
}

type UpdateRef = (String, Oid, Oid);

fn read_update_refs(dir: &Path) -> Vec<UpdateRef> {
    let text = read(dir, "update-refs").unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines
        .chunks(3)
        .filter_map(|c| match c {
            [name, from, to] => Some((
                (*name).to_owned(),
                Oid::from_str(from).ok()?,
                Oid::from_str(to).ok()?,
            )),
            _ => None,
        })
        .collect()
}

fn write_update_refs(dir: &Path, refs: &[UpdateRef]) -> Result<(), GitError> {
    let text: String = refs
        .iter()
        .map(|(n, f, t)| format!("{n}\n{f}\n{t}\n"))
        .collect();
    write(dir, "update-refs", text)
}

/// Copy notes to the rewritten commits as notes.rewriteRef, notes.rewriteMode
/// and notes.rewrite.rebase say.
fn rewrite_notes(repo: &Repository, rewritten: &str) {
    let pairs: Vec<(Oid, Oid)> = rewritten
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(a, b)| Some((Oid::from_str(a).ok()?, Oid::from_str(b).ok()?)))
        .collect();
    let _ = crate::notes::copy_for_rewrite(repo, "rebase", &pairs);
}

/// Remove the rebase state: the directory, its labels and the stop files.
fn cleanup_state(repo: &Repository) -> Result<(), GitError> {
    if let Ok(refs) = repo.references_glob("refs/rewritten/*") {
        let names: Vec<String> = refs
            .flatten()
            .filter_map(|r| r.name().ok().map(str::to_owned))
            .collect();
        for name in names {
            if let Ok(mut r) = repo.find_reference(&name) {
                let _ = r.delete();
            }
        }
    }
    for f in OP_FILES {
        let _ = std::fs::remove_file(repo.path().join(f));
    }
    let dir = state_dir(repo);
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

fn apply_autostash(repo: &Repository, stash: Option<Oid>) -> Result<Vec<String>, GitError> {
    let Some(stash) = stash else {
        return Ok(Vec::new());
    };
    store_stash(repo, stash, "autostash")?;
    // stash_pop needs a mutable handle; a fresh one sees the same repository.
    let mut fresh = Repository::open(repo.path())?;
    Ok(vec![if fresh.stash_pop(0, None).is_ok() {
        "Applied autostash.".to_owned()
    } else {
        "Applying autostash resulted in conflicts.\nYour changes are safe in the stash.\nYou can \
         run `rgit stash pop` or `rgit stash drop` at any time."
            .to_owned()
    }])
}

/// `reset --hard` that keeps the rebase state, which libgit2's reset deletes.
fn hard_reset(repo: &Repository, commit: &Commit) -> Result<(), GitError> {
    repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().force()))?;
    let mut index = repo.index()?;
    index.read_tree(&commit.tree()?)?;
    index.write()?;
    repo.reference("HEAD", commit.id(), true, "rebase (reset)")?;
    Ok(())
}

fn clear_stop(dir: &Path, repo: &Repository) {
    for f in STOP_FILES {
        let _ = std::fs::remove_file(dir.join(f));
    }
    for f in OP_FILES {
        let _ = std::fs::remove_file(repo.path().join(f));
    }
}

fn tracked_changes(repo: &Repository) -> Result<bool, GitError> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(false).include_ignored(false);
    Ok(repo
        .statuses(Some(&mut opts))?
        .iter()
        .any(|e| e.status() != git2::Status::CURRENT))
}

fn patch_id(repo: &Repository, commit: &Commit) -> Option<Oid> {
    let parent = commit.parent(0).ok()?.tree().ok()?;
    let diff = repo
        .diff_tree_to_tree(Some(&parent), Some(&commit.tree().ok()?), None)
        .ok()?;
    diff.patchid(None).ok()
}

/// git's `merge-base --fork-point`: the newest commit of `refname`'s reflog
/// that `head` is built on.
fn fork_point(repo: &Repository, refname: &str, head: Oid) -> Option<Oid> {
    let reflog = repo.reflog(refname).ok()?;
    let mut tips: Vec<Oid> = reflog.iter().map(|e| e.id_new()).collect();
    tips.extend(repo.refname_to_id(refname));
    let bases: HashSet<Oid> = tips
        .iter()
        .filter_map(|t| repo.merge_base(head, *t).ok())
        .collect();
    let best: Vec<Oid> = bases
        .iter()
        .copied()
        .filter(|b| {
            !bases
                .iter()
                .any(|o| o != b && repo.graph_descendant_of(*o, *b).unwrap_or(false))
        })
        .collect();
    match best[..] {
        [one] if tips.contains(&one) => Some(one),
        _ => None,
    }
}

/// The commits to replay: those on `head` but not `hide`, oldest first.
fn walk(repo: &Repository, head: Oid, hide: &[Oid]) -> Result<Vec<Oid>, GitError> {
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
    walk.push(head)?;
    for h in hide {
        walk.hide(*h)?;
    }
    Ok(walk.collect::<Result<_, _>>()?)
}

/// Commits whose change `left` (the upstream side) already has, by patch id.
fn already_upstream(
    repo: &Repository,
    commits: &[Oid],
    left: Oid,
    head: Oid,
) -> Result<HashSet<Oid>, GitError> {
    let upstream = walk(repo, left, &[head])?;
    if upstream.is_empty() || commits.is_empty() {
        return Ok(HashSet::new());
    }
    let ids: HashSet<Oid> = upstream
        .iter()
        .filter_map(|o| repo.find_commit(*o).ok())
        .filter(|c| c.parent_count() == 1)
        .filter_map(|c| patch_id(repo, &c))
        .collect();
    Ok(commits
        .iter()
        .copied()
        .filter(|o| {
            repo.find_commit(*o).is_ok_and(|c| {
                c.parent_count() == 1 && patch_id(repo, &c).is_some_and(|p| ids.contains(&p))
            })
        })
        .collect())
}

/// git's `--rebase-merges` todo: each branch walked back from its tip, with
/// labels for merge parents and branch points.
fn merges_todo(
    repo: &Repository,
    commits: &[Oid],
    skip: &HashSet<Oid>,
    onto_label: Option<Oid>,
    cousins: bool,
    root_with_onto: bool,
) -> Result<Vec<Item>, GitError> {
    let interesting: HashSet<Oid> = commits.iter().copied().collect();
    let mut labels: HashMap<Oid, String> = HashMap::new();
    let mut used: HashSet<String> = HashSet::new();
    if let Some(o) = onto_label {
        labels.insert(o, "onto".into());
    }
    used.insert("onto".into());
    let mut label_oid = |oid: Oid, name: Option<&str>, labels: &mut HashMap<Oid, String>| {
        if let Some(l) = labels.get(&oid) {
            return l.clone();
        }
        let base = match name {
            Some(n) => n
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                        c
                    } else {
                        '-'
                    }
                })
                .collect(),
            None => short7(oid),
        };
        let mut label = base.clone();
        let mut n = 2;
        while !used.insert(label.to_lowercase()) {
            label = format!("{base}-{n}");
            n += 1;
        }
        labels.insert(oid, label.clone());
        label
    };
    let mut todo: HashMap<Oid, Item> = HashMap::new();
    let mut tips: Vec<Oid> = Vec::new();
    for &oid in commits {
        let c = repo.find_commit(oid)?;
        if c.parent_count() < 2 {
            if !skip.contains(&oid) {
                todo.insert(oid, Item::pick(Cmd::Pick, oid));
            }
            continue;
        }
        let oneline = c.summary().ok().flatten().unwrap_or("").to_owned();
        let name = oneline
            .strip_prefix("Merge ")
            .and_then(|r| {
                r.split('\'')
                    .nth(1)
                    .filter(|_| r.matches('\'').count() >= 2)
            })
            .map(str::to_owned)
            .or_else(|| {
                oneline
                    .strip_prefix("Merge pull request ")
                    .and_then(|r| r.split_once(" from "))
                    .map(|(_, b)| b.to_owned())
            })
            .unwrap_or_else(|| oneline.clone());
        let mut args = Vec::new();
        for p in c.parent_ids().skip(1) {
            if interesting.contains(&p) {
                tips.push(p);
                args.push(label_oid(p, Some(&name), &mut labels));
            } else {
                args.push(label_oid(p, None, &mut labels));
            }
        }
        todo.insert(
            oid,
            Item {
                cmd: Cmd::Merge,
                flag: Some('C'),
                oid: Some(oid),
                arg: args.join(" "),
                note: None,
            },
        );
    }
    let mut seen = HashSet::new();
    for &oid in commits {
        for p in repo.find_commit(oid)?.parent_ids() {
            if interesting.contains(&p) && !seen.insert(p) {
                label_oid(p, Some("branch-point"), &mut labels);
            }
        }
    }
    tips.extend(commits.last());
    let mut out = vec![Item::with_arg(Cmd::Label, "onto")];
    let mut shown = HashSet::new();
    for tip in tips {
        if shown.contains(&tip) {
            continue;
        }
        let mut list = Vec::new();
        let mut at = Some(tip);
        while let Some(c) = at.filter(|c| interesting.contains(c) && !shown.contains(c)) {
            list.push(c);
            at = repo.find_commit(c)?.parent_id(0).ok();
        }
        list.reverse();
        let to = match at {
            None => (if cousins || root_with_onto {
                "onto"
            } else {
                "[new root]"
            })
            .to_owned(),
            Some(base) => match labels.get(&base) {
                Some(l) => l.clone(),
                None if !cousins => label_oid(base, None, &mut labels),
                None => "onto".to_owned(),
            },
        };
        out.push(Item::with_arg(Cmd::Reset, &to));
        for c in list {
            if let Some(item) = todo.get(&c) {
                out.push(item.clone());
            }
            if let Some(l) = labels.get(&c) {
                out.push(Item::with_arg(Cmd::Label, l));
            }
            shown.insert(c);
        }
    }
    Ok(out)
}

/// git's `--autosquash`: each `fixup!`/`squash!`/`amend!` commit moves after
/// the commit its subject names.
fn rearrange_squash(repo: &Repository, items: Vec<Item>) -> Vec<Item> {
    let subjects: Vec<Option<String>> = items
        .iter()
        .map(|i| {
            i.oid
                .filter(|_| i.cmd.takes_commit())
                .map(|o| subject(repo, o))
        })
        .collect();
    let strip = |s: &str| -> Option<String> {
        ["fixup! ", "amend! ", "squash! "]
            .iter()
            .find_map(|p| s.strip_prefix(p))
            .map(str::to_owned)
    };
    let n = items.len();
    let mut by_subject: HashMap<String, usize> = HashMap::new();
    let mut next: Vec<Option<usize>> = vec![None; n];
    let mut tail: Vec<Option<usize>> = vec![None; n];
    let mut moved = vec![false; n];
    let mut items = items;
    for i in 0..n {
        let Some(subj) = subjects[i].clone() else {
            continue;
        };
        if items[i].cmd == Cmd::Pick
            && let Some(mut p) = strip(&subj)
        {
            while let Some(q) = strip(p.trim_start()) {
                p = q;
            }
            let p = p.trim_start().to_owned();
            let by_id = || {
                if p.contains(' ') {
                    return None;
                }
                let oid = repo.rev_single(&p).ok()?.peel_to_commit().ok()?.id();
                items.iter().position(|it| it.oid == Some(oid))
            };
            let target = by_subject.get(&p).copied().or_else(by_id).or_else(|| {
                (0..i).find(|&j| subjects[j].as_ref().is_some_and(|s| s.starts_with(&p)))
            });
            if let Some(t) = target.filter(|&t| t != i) {
                if subj.starts_with("fixup!") {
                    items[i].cmd = Cmd::Fixup;
                } else if subj.starts_with("amend!") {
                    items[i].cmd = Cmd::Fixup;
                    items[i].flag = Some('C');
                } else {
                    items[i].cmd = Cmd::Squash;
                }
                moved[i] = true;
                let end = tail[t].unwrap_or(t);
                next[i] = next[end];
                next[end] = Some(i);
                tail[t] = Some(i);
                continue;
            }
        }
        by_subject.entry(subj).or_insert(i);
    }
    let mut out = Vec::with_capacity(n);
    for (i, _) in moved.iter().enumerate().filter(|(_, m)| !**m) {
        let mut at = Some(i);
        while let Some(k) = at {
            out.push(items[k].clone());
            at = next[k];
        }
    }
    out
}

/// `exec` lines after each commit, or after its squash chain.
fn add_exec(items: Vec<Item>, cmds: &[String]) -> Vec<Item> {
    if cmds.is_empty() {
        return items;
    }
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        out.push(item.clone());
        let commit = item.cmd.takes_commit() && item.cmd != Cmd::Drop || item.cmd == Cmd::Merge;
        let chain_goes_on = items.get(i + 1).is_some_and(|n| n.cmd.is_fixup());
        if commit && !chain_goes_on {
            out.extend(cmds.iter().map(|c| Item::with_arg(Cmd::Exec, c)));
        }
    }
    out
}

/// `update-ref` lines after each commit (and its chain) that a branch other
/// than the one rebased points at.
fn add_update_refs(
    repo: &Repository,
    items: Vec<Item>,
    head_name: &str,
) -> Result<Vec<Item>, GitError> {
    let mut at: HashMap<Oid, Vec<String>> = HashMap::new();
    for b in repo.branches(Some(git2::BranchType::Local))? {
        let (b, _) = b?;
        let (Some(name), Some(oid)) = (b.get().name().ok().map(str::to_owned), b.get().target())
        else {
            continue;
        };
        if name == head_name || checked_out_at(repo, &name)?.is_some() {
            continue;
        }
        at.entry(oid).or_default().push(name);
    }
    let mut out = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        out.push(item.clone());
        if item.cmd != Cmd::Drop
            && let Some(names) = item.oid.and_then(|o| at.get(&o))
        {
            pending.extend(names.iter().cloned());
        }
        let group_goes_on = items
            .get(i + 1)
            .is_some_and(|n| n.cmd.is_fixup() || n.cmd == Cmd::Exec);
        if !group_goes_on {
            for name in pending.drain(..) {
                if !out
                    .iter()
                    .any(|it| it.cmd == Cmd::UpdateRef && it.arg == name)
                {
                    out.push(Item::with_arg(Cmd::UpdateRef, &name));
                }
            }
        }
    }
    Ok(out)
}

/// `rebase --onto <onto>`'s `A...B` form: their merge base.
fn resolve_onto(repo: &Repository, rev: &str) -> Result<Oid, GitError> {
    if let Some((a, b)) = rev.split_once("...") {
        let side = |r: &str| -> Result<Oid, GitError> {
            let r = if r.is_empty() { "HEAD" } else { r };
            Ok(repo.rev_single(r)?.peel_to_commit()?.id())
        };
        return repo
            .merge_base(side(a)?, side(b)?)
            .map_err(|_| GitError::Other(format!("'{rev}': need exactly one merge base")));
    }
    Ok(repo
        .rev_single(rev)
        .and_then(|o| o.peel_to_commit())
        .map_err(|_| GitError::Other(format!("does not point to a valid commit: '{rev}'")))?
        .id())
}

fn up_to_date(
    repo: &Repository,
    onto: Oid,
    upstream: Option<Oid>,
    bottom: Option<Oid>,
    head: Oid,
) -> bool {
    let Some(upstream) = upstream else {
        return false;
    };
    let base = |a: Oid| repo.merge_base(a, head).ok();
    base(onto) == Some(onto)
        && bottom.is_none_or(|b| b == upstream || Some(b) == base(onto))
        && base(upstream) == Some(onto)
}

/// Start a rebase of HEAD (or `o.branch`) onto `upstream` (the branch's
/// upstream when `None`). A stop leaves git's state for `--continue`.
pub(crate) fn start(
    repo: &mut Repository,
    upstream_arg: Option<&str>,
    o: &crate::RebaseOptions,
) -> Result<String, GitError> {
    let dir = state_dir(repo);
    if dir.exists() || repo.path().join("rebase-apply").exists() {
        return Err(GitError::Conflict(
            "a rebase is already in progress; run `rgit rebase --continue`, --skip, --abort or \
             --quit"
                .into(),
        ));
    }
    if !matches!(
        repo.state(),
        git2::RepositoryState::Clean | git2::RepositoryState::Bisect
    ) {
        return Err(GitError::Conflict(
            "an operation is already in progress; finish or abort it first".into(),
        ));
    }
    crate::git_repo::sync_index(repo)?;
    if o.keep_base && o.onto.is_some() {
        return Err(GitError::Other(
            "--keep-base and --onto cannot be used together".into(),
        ));
    }
    let (upstream_arg, branch) = if o.root {
        (None, o.branch.as_deref().or(upstream_arg))
    } else {
        (upstream_arg, o.branch.as_deref())
    };
    if let Some(b) = branch {
        checkout(repo, short_ref(b))?;
    }
    let config = repo.config()?.snapshot()?;
    let flag = |key: &str| config.get_bool(key).unwrap_or(false);
    let head_ref = repo.head()?;
    let head_name = match head_ref.is_branch() {
        true => head_ref.name().unwrap_or("HEAD").to_owned(),
        false => "detached HEAD".to_owned(),
    };
    let orig = head_ref.peel_to_commit()?.id();
    drop(head_ref);

    let (upstream, upstream_ref) = if o.root {
        (None, None)
    } else {
        let arg = upstream_arg.unwrap_or("@{upstream}");
        let (obj, r) = repo.revparse_ext(arg).map_err(|_| {
            GitError::Other(match upstream_arg {
                None => "there is no tracking information for the current branch; name the \
                         branch to rebase against"
                    .to_owned(),
                Some(a) => format!("invalid upstream '{a}'"),
            })
        })?;
        (
            Some(obj.peel_to_commit()?.id()),
            r.and_then(|r| r.name().ok().map(str::to_owned)),
        )
    };
    let onto = match (&o.onto, o.keep_base, upstream) {
        (Some(x), _, _) => Some(resolve_onto(repo, x)?),
        (None, true, Some(u)) => Some(repo.merge_base(u, orig)?),
        (None, _, u) => u,
    };
    let fork = o.fork_point.unwrap_or(
        upstream_arg.is_none()
            && !o.keep_base
            && config.get_bool("rebase.forkPoint").unwrap_or(true),
    );
    let bottom = match (&upstream_ref, fork) {
        (Some(r), true) => fork_point(repo, r, orig).or(upstream),
        _ => upstream,
    };
    let force = o.force || o.signoff || o.committer_date_is_author_date || o.reset_author_date;
    let autosquash =
        !o.no_autosquash && (o.autosquash || (o.interactive && flag("rebase.autoSquash")));
    let update_refs = o.update_refs || flag("rebase.updateRefs");
    let autostash = o.autostash || flag("rebase.autoStash");
    let dirty = tracked_changes(repo)?;
    if dirty && !autostash {
        return Err(GitError::Conflict(
            "cannot rebase: you have unstaged or uncommitted changes; commit or stash them (or \
             pass --autostash)"
                .into(),
        ));
    }
    if !o.no_verify {
        let mut args: Vec<&str> = match upstream_arg {
            Some(a) => vec![a],
            None if o.root => vec!["--root"],
            None => vec![],
        };
        args.extend(branch);
        if !run_hook(repo, "pre-rebase", &args, "")? {
            return Err(GitError::Hook(
                "The pre-rebase hook refused to rebase.".into(),
            ));
        }
    }
    let mut out = Vec::new();
    if o.verbose
        && let (Some(onto), Some(u)) = (onto, upstream)
        && let Ok(mb) = repo.merge_base(u, orig)
    {
        let diff = repo.diff_tree_to_tree(
            Some(&repo.find_commit(mb)?.tree()?),
            Some(&repo.find_commit(onto)?.tree()?),
            None,
        )?;
        let stats = diff.stats()?.to_buf(git2::DiffStatsFormat::FULL, 80)?;
        out.push(format!("Changes from {mb} to {onto}:"));
        out.push(stats.as_str().unwrap_or("").trim_end().to_owned());
    }
    let branch_label = short_ref(&head_name).to_owned();
    if !force
        && !o.interactive
        && o.exec.is_empty()
        && !autosquash
        && o.rebase_merges.is_none()
        && onto.is_some_and(|onto| up_to_date(repo, onto, upstream, bottom, orig))
    {
        if o.quiet {
            return Ok(String::new());
        }
        let name = if head_name.starts_with("refs/") {
            branch_label.as_str()
        } else {
            "HEAD"
        };
        out.push(format!("Current branch {name} is up to date."));
        return Ok(out.join("\n"));
    }

    // The commits to replay.
    let left = if o.root { onto } else { upstream };
    let hide: Vec<Oid> = [bottom, upstream, if o.root { onto } else { None }]
        .into_iter()
        .flatten()
        .collect();
    let commits = walk(repo, orig, &hide)?;
    let skip = match left {
        Some(l) if !(o.reapply_cherry_picks || o.keep_base) => {
            already_upstream(repo, &commits, l, orig)?
        }
        _ => HashSet::new(),
    };
    for c in commits.iter().filter(|c| skip.contains(c)) {
        out.push(format!(
            "warning: skipped previously applied commit {}",
            short7(*c)
        ));
    }
    if !skip.is_empty() {
        out.push("hint: use --reapply-cherry-picks to include skipped commits".to_owned());
    }
    let mut items = match o.rebase_merges {
        Some(cousins) => {
            let label = match o.root {
                true => onto.filter(|_| o.onto.is_some()),
                false => left.and_then(|l| repo.merge_base(l, orig).ok()),
            };
            merges_todo(
                repo,
                &commits,
                &skip,
                label,
                cousins,
                o.root && o.onto.is_some(),
            )?
        }
        None => commits
            .iter()
            .filter(|c| !skip.contains(c))
            .filter(|c| repo.find_commit(**c).is_ok_and(|c| c.parent_count() < 2))
            .map(|c| Item::pick(Cmd::Pick, *c))
            .collect(),
    };
    if autosquash {
        items = rearrange_squash(repo, items);
    }
    items = add_exec(items, &o.exec);
    if update_refs {
        items = add_update_refs(repo, items, &head_name)?;
    }
    let empty = match o.empty.as_deref() {
        Some("drop") => Empty::Drop,
        Some("keep") => Empty::Keep,
        Some(_) => Empty::Stop,
        None if o.interactive => Empty::Stop,
        None if !o.exec.is_empty() => Empty::Keep,
        None => Empty::Drop,
    };

    std::fs::create_dir_all(&dir)?;
    let result = (|| -> Result<(Oid, bool), GitError> {
        let root_onto = if onto.is_none() {
            let mut seq = Seq::load_partial(repo, &dir);
            Some(seq.new_root()?)
        } else {
            None
        };
        let onto = onto.or(root_onto).expect("an onto commit");
        write(&dir, "head-name", format!("{head_name}\n"))?;
        write(&dir, "onto", format!("{onto}\n"))?;
        write(&dir, "orig-head", format!("{orig}\n"))?;
        write(&dir, "interactive", "")?;
        write(&dir, "no-reschedule-failed-exec", "")?;
        if let Some(side) = &o.strategy_option {
            file_favor(side)?;
            write(&dir, "strategy", "ort\n")?;
            write(&dir, "strategy_opts", format!("\"{side}\"\n"))?;
        }
        for (on, file, text) in [
            (o.signoff, "signoff", "--signoff\n"),
            (o.committer_date_is_author_date, "cdate_is_adate", ""),
            (o.reset_author_date, "ignore_date", ""),
            (empty == Empty::Drop, "drop_redundant_commits", ""),
            (empty == Empty::Keep, "keep_redundant_commits", ""),
            (o.quiet, "quiet", ""),
            (o.verbose, "verbose", ""),
        ] {
            if on {
                write(&dir, file, text)?;
            }
        }
        if let Some(key) = crate::sign::commit_key(repo, o.sign.as_deref(), o.no_sign) {
            write(&dir, "gpg_sign_opt", format!("-S{key}"))?;
        }
        if let Some(auto) = o.rerere_autoupdate {
            let no = if auto { "" } else { "no-" };
            write(
                &dir,
                "allow_rerere_autoupdate",
                format!("--{no}rerere-autoupdate\n"),
            )?;
        }
        let style = Style::load(repo);
        write(
            &dir,
            "git-rebase-todo",
            todo_body(repo, &items, false, &style),
        )?;
        if o.interactive {
            let revs = match upstream {
                Some(u) => format!("{}..{}", abbrev(repo, u), abbrev(repo, orig)),
                None => abbrev(repo, orig),
            };
            let help = todo_help(repo, items.len(), Some((&revs, &abbrev(repo, onto))));
            let path = dir.join("git-rebase-todo");
            let body = todo_body(repo, &items, true, &style);
            std::fs::write(&path, format!("{body}{help}"))?;
            let body = todo_body(repo, &items, false, &style);
            write(&dir, "git-rebase-todo.backup", format!("{body}{help}"))?;
            edit_todo_file(repo, &path)?;
            let edited = parse_all(repo, &std::fs::read_to_string(&path)?)?;
            if edited.is_empty() {
                return Err(GitError::Other("nothing to do".into()));
            }
            // git stops on onto with the todo as edited, for --edit-todo or
            // --continue once the lost commits are back or dropped.
            if missing_commits(repo, &items, &edited, &style) {
                write(&dir, "dropped", "")?;
                write(&dir, "end", format!("{}\n", edited.len()))?;
                return Ok((onto, true));
            }
            items = edited;
            write(
                &dir,
                "git-rebase-todo",
                todo_body(repo, &items, false, &style),
            )?;
        }
        write(&dir, "end", format!("{}\n", items.len()))?;
        let refs: Vec<UpdateRef> = items
            .iter()
            .filter(|i| i.cmd == Cmd::UpdateRef)
            .filter_map(|i| {
                let from = repo.refname_to_id(&i.arg).ok()?;
                Some((i.arg.clone(), from, Oid::ZERO_SHA1))
            })
            .collect();
        if !refs.is_empty() {
            write_update_refs(&dir, &refs)?;
        }
        Ok((onto, false))
    })();
    let (onto, dropped) = match result {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    if dirty {
        let sig = repo.signature()?;
        let stash = repo.stash_save2(&sig, Some("autostash"), None)?;
        repo.stash_drop(0)?;
        write(&dir, "autostash", format!("{stash}\n"))?;
        out.push(format!("Created autostash: {}", short7(stash)));
    }
    let repo: &Repository = repo;
    repo.reference("ORIG_HEAD", orig, true, "rebase")?;
    let mut seq = Seq::load(repo)?;
    let target = upstream_arg.unwrap_or(&branch_label);
    if let Err(e) = seq.move_to(onto, &format!("rebase (start): checkout {target}")) {
        let stash = read_oid(&dir, "autostash");
        cleanup_state(repo)?;
        apply_autostash(repo, stash)?;
        return Err(e);
    }
    if dropped {
        return Err(GitError::Conflict(out.join("\n")));
    }
    seq.allow_ff &= !force;
    seq.out = out;
    seq.run()
}

impl<'r> Seq<'r> {
    /// A handle on a state directory still being written.
    fn load_partial(repo: &'r Repository, dir: &Path) -> Self {
        Seq {
            repo,
            dir: dir.to_owned(),
            strategy: None,
            signoff: false,
            cdate: false,
            ignore_date: false,
            allow_ff: true,
            empty: Empty::Drop,
            quiet: false,
            squash_onto: None,
            gpg: None,
            out: Vec::new(),
        }
    }
}

/// `rebase --continue` (or `--skip`): commit what the stop left staged and
/// go on with the todo list.
pub(crate) fn resume(repo: &Repository, skip: bool) -> Result<String, GitError> {
    let seq = Seq::load(repo)?;
    crate::git_repo::sync_index(repo)?;
    if seq.dir.join("dropped").exists() {
        let backup = parse_all(
            repo,
            &read(&seq.dir, "git-rebase-todo.backup").unwrap_or_default(),
        )?;
        let todo = parse_all(repo, &read(&seq.dir, "git-rebase-todo").unwrap_or_default())?;
        if !backup.is_empty() && missing_commits(repo, &backup, &todo, &Style::load(repo)) {
            return Err(GitError::Conflict(String::new()));
        }
        let _ = std::fs::remove_file(seq.dir.join("dropped"));
    }
    let head = seq.head()?;
    if skip {
        crate::rerere::clear(repo)?;
        hard_reset(repo, &head)?;
        clear_stop(&seq.dir, repo);
        let _ = std::fs::remove_file(seq.dir.join("current-fixups"));
        let _ = std::fs::remove_file(seq.dir.join("message-squash"));
        return seq.run();
    }
    let mut index = repo.index()?;
    if index.has_conflicts() {
        return Err(GitError::Conflict(
            "you must edit all merge conflicts and then mark them as resolved with `rgit add`"
                .into(),
        ));
    }
    let mut opts = git2::DiffOptions::new();
    if repo
        .diff_index_to_workdir(None, Some(&mut opts))?
        .deltas()
        .len()
        > 0
    {
        return Err(GitError::Conflict(
            "cannot continue: you have unstaged changes; stage them with `rgit add` or discard \
             them"
                .into(),
        ));
    }
    crate::rerere::say(repo, None);
    let tree = index.write_tree()?;
    let dir = seq.dir.clone();
    let stored = |f: &str| {
        read(&dir, f)
            .or_else(|| read(repo.path(), "MERGE_MSG"))
            .map(|m| cleanup(&m))
    };
    let stopped = read_oid(&dir, "stopped-sha")
        .or_else(|| read_oid(repo.path(), "REBASE_HEAD"))
        .and_then(|o| repo.find_commit(o).ok());
    let author = read(&dir, "author-script")
        .and_then(|s| read_author_script(&s))
        .or_else(|| stopped.as_ref().map(|c| c.author().to_owned()));
    let amend = read_oid(&dir, "amend");
    let chain = dir.join("current-fixups").exists();
    let subject = stopped
        .as_ref()
        .and_then(|c| c.summary().ok().flatten().map(str::to_owned))
        .unwrap_or_default();
    if let Some(merge_head) = read_oid(repo.path(), "MERGE_HEAD") {
        let target = repo.find_commit(merge_head)?;
        let msg = stored("message").unwrap_or_default();
        let author = author.unwrap_or(committer(repo)?);
        let new = seq.commit(&author, &msg, tree, &[&head, &target])?;
        if let Some(s) = &stopped {
            seq.rewritten(s.id(), new, None)?;
        }
        seq.set_head(new, "rebase (merge)")?;
    } else if tree != head.tree_id() {
        if let Some(amend) = amend {
            if amend != head.id() {
                return Err(GitError::Conflict(
                    "you have uncommitted changes in your working tree; commit them first and \
                     then run `rgit rebase --continue` again"
                        .into(),
                ));
            }
            // The chain's last fixup opens the editor on the whole squash
            // message when a squash or `fixup -c` asked for it, as git does.
            let msg = match chain {
                true if !seq.peek().is_some_and(Cmd::is_fixup) => {
                    match read(&dir, "message-squash") {
                        Some(buf) => seq.end_chain(&buf)?,
                        None => stored("message").unwrap_or_else(|| message(&head)),
                    }
                }
                true => stored("message").unwrap_or_else(|| message(&head)),
                false => message(&head),
            };
            let parents: Vec<Commit> = head.parents().collect();
            let parents: Vec<&Commit> = parents.iter().collect();
            let new = seq.commit(&head.author(), &msg, tree, &parents)?;
            if let Some(s) = &stopped {
                seq.rewritten(s.id(), new, Some(head.id()))?;
            }
            seq.set_head(new, &format!("rebase (continue): {subject}"))?;
        } else {
            let msg = stored("message").unwrap_or_default();
            if msg.is_empty() {
                return Err(GitError::Other(
                    "aborting commit due to empty commit message".into(),
                ));
            }
            let author = author.unwrap_or(committer(repo)?);
            let parents: Vec<&Commit> = if seq.squash_onto == Some(head.id()) {
                vec![]
            } else {
                vec![&head]
            };
            let new = seq.commit(&author, &msg, tree, &parents)?;
            if let Some(s) = &stopped {
                seq.rewritten(s.id(), new, None)?;
            }
            seq.set_head(new, &format!("rebase (continue): {subject}"))?;
        }
    }
    if chain && !seq.peek().is_some_and(Cmd::is_fixup) {
        let _ = std::fs::remove_file(dir.join("current-fixups"));
        let _ = std::fs::remove_file(dir.join("message-squash"));
    }
    clear_stop(&dir, repo);
    seq.run()
}

/// `rebase --abort`: back to the branch as it was before the rebase.
pub(crate) fn abort(repo: &Repository) -> Result<String, GitError> {
    let dir = state_dir(repo);
    let orig = read_oid(&dir, "orig-head")
        .ok_or_else(|| GitError::Other("no rebase in progress".into()))?;
    let head_name = read(&dir, "head-name").unwrap_or_default();
    let head_name = head_name.trim();
    let commit = repo.find_commit(orig)?;
    crate::rerere::clear(repo)?;
    hard_reset(repo, &commit)?;
    if head_name.starts_with("refs/") {
        repo.set_head(head_name)?;
    }
    let stash = read_oid(&dir, "autostash");
    cleanup_state(repo)?;
    Ok(apply_autostash(repo, stash)?.join("\n"))
}

/// `rebase --quit`: forget the rebase, leaving HEAD and the files as they are.
pub(crate) fn quit(repo: &Repository) -> Result<(), GitError> {
    let dir = state_dir(repo);
    if !dir.exists() {
        return Err(GitError::Other("no rebase in progress".into()));
    }
    if let Some(stash) = read_oid(&dir, "autostash") {
        store_stash(repo, stash, "autostash")?;
    }
    cleanup_state(repo)
}

/// `rebase --edit-todo`: the remaining todo in the sequence editor.
pub(crate) fn edit_todo(repo: &Repository) -> Result<(), GitError> {
    let seq = Seq::load(repo)?;
    let style = Style::load(repo);
    let path = seq.dir.join("git-rebase-todo");
    let text = read(&seq.dir, "git-rebase-todo").unwrap_or_default();
    let help = todo_help(repo, 0, None);
    let old = parse_all(repo, &text);
    let dropped = seq.dir.join("dropped");
    let incorrect = old.is_err() || dropped.exists();
    match &old {
        Ok(items) => {
            let body = todo_body(repo, items, true, &style);
            std::fs::write(&path, format!("{body}{help}"))?;
            if !incorrect {
                let body = todo_body(repo, items, false, &style);
                write(&seq.dir, "git-rebase-todo.backup", format!("{body}{help}"))?;
            }
        }
        Err(_) => std::fs::write(&path, format!("{text}{help}"))?,
    }
    edit_todo_file(repo, &path)?;
    let new = parse_all(repo, &std::fs::read_to_string(&path)?).inspect_err(|_| {
        eprint!("{EDIT_TODO_ADVICE}");
    })?;
    let lost = if incorrect {
        let backup = parse_all(
            repo,
            &read(&seq.dir, "git-rebase-todo.backup").unwrap_or_default(),
        )?;
        missing_commits(repo, &backup, &new, &style)
    } else {
        missing_commits(repo, &old.unwrap_or_default(), &new, &style)
    };
    if lost {
        write(&seq.dir, "dropped", "")?;
        return Err(GitError::Conflict(String::new()));
    }
    let _ = std::fs::remove_file(&dropped);
    write(
        &seq.dir,
        "git-rebase-todo",
        todo_body(repo, &new, false, &style),
    )?;
    let done = seq.lines("done").len();
    write(&seq.dir, "end", format!("{}\n", done + new.len()))
}

/// Rebase HEAD onto `upstream` (or `--onto onto upstream`), undoing the
/// whole rebase if a commit conflicts; `report` gets the output lines.
pub(crate) fn replay(
    repo: &mut Repository,
    upstream: &str,
    onto: Option<&str>,
    report: &dyn Fn(crate::OpProgress),
) -> Result<(), GitError> {
    let opts = crate::RebaseOptions {
        onto: onto.map(str::to_owned),
        fork_point: Some(false),
        ..Default::default()
    };
    match start(repo, Some(upstream), &opts) {
        Ok(out) => {
            for line in out.lines() {
                report(crate::OpProgress::Line(line.to_owned()));
            }
            Ok(())
        }
        Err(GitError::Conflict(why)) if state_dir(repo).exists() => {
            abort(repo)?;
            for line in why.lines().filter(|l| l.starts_with("CONFLICT")) {
                report(crate::OpProgress::Line(line.to_owned()));
            }
            Err(GitError::Conflict(
                "rebase hit a conflict and was aborted".into(),
            ))
        }
        Err(e) => Err(e),
    }
}
