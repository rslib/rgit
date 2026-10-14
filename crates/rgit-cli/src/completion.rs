//! Value candidates for Tab completion, read from the repository in the
//! current directory. Every failure, such as no repository, gives no
//! candidates: a completion must never print an error into the shell.

use std::ffi::OsStr;

use clap_complete::engine::{
    ArgValueCandidates, ArgValueCompleter, CompletionCandidate, PathCompleter, ValueCompleter,
};
use rgit_git::{Git2Backend, GitBackend, RefKind};

fn backend() -> Option<Git2Backend> {
    Git2Backend::discover(".").ok()
}

fn matching(
    current: &OsStr,
    values: impl IntoIterator<Item = (String, Option<String>)>,
) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .filter(|(value, _)| value.starts_with(prefix.as_ref()) && seen.insert(value.clone()))
        .map(|(value, help)| CompletionCandidate::new(value).help(help.map(Into::into)))
        .collect()
}

fn ref_names() -> Vec<(String, Option<String>)> {
    let Some(backend) = backend() else {
        return Vec::new();
    };
    let Ok(refs) = backend.refs() else {
        return Vec::new();
    };
    let mut names = vec![("HEAD".to_owned(), None)];
    for entry in refs {
        let help = match entry.kind {
            RefKind::Local => "branch",
            RefKind::Remote if entry.name.ends_with("/HEAD") => continue,
            RefKind::Remote => "remote branch",
            RefKind::Tag => "tag",
        };
        names.push((entry.name, Some(help.to_owned())));
    }
    names
}

/// Local branches, tags, remote-tracking branches and `HEAD`.
pub fn revs(current: &OsStr) -> Vec<CompletionCandidate> {
    matching(current, ref_names())
}

/// A revision or a file: for `log`, `diff` and other commands that take both.
pub fn revs_or_paths(current: &OsStr) -> Vec<CompletionCandidate> {
    let mut out = revs(current);
    out.extend(files(current));
    out
}

/// Plain files under the current directory.
pub fn files(current: &OsStr) -> Vec<CompletionCandidate> {
    PathCompleter::file().complete(current)
}

pub fn remotes(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(backend) = backend() else {
        return Vec::new();
    };
    let Ok(remotes) = backend.remotes() else {
        return Vec::new();
    };
    matching(current, remotes.into_iter().map(|r| (r.name, Some(r.url))))
}

/// Paths with an unstaged change or untracked, else plain files.
pub fn changed(current: &OsStr) -> Vec<CompletionCandidate> {
    status_paths(current, |e| e.is_unstaged() || e.is_untracked())
}

pub fn staged(current: &OsStr) -> Vec<CompletionCandidate> {
    status_paths(current, rgit_git::StatusEntry::is_staged)
}

fn status_paths(
    current: &OsStr,
    wanted: impl Fn(&rgit_git::StatusEntry) -> bool,
) -> Vec<CompletionCandidate> {
    let Some(backend) = backend() else {
        return Vec::new();
    };
    let Ok(status) = backend.status() else {
        return Vec::new();
    };
    let prefix = cwd_in_repo(backend.workdir());
    let up = prefix
        .as_deref()
        .map_or(0, |p| p.split('/').filter(|c| !c.is_empty()).count());
    let outside_ok = current.to_string_lossy().starts_with("../");
    let paths = status
        .entries
        .into_iter()
        .filter(|e| wanted(e))
        .filter_map(|e| {
            let prefix = prefix.as_deref()?;
            match e.path.strip_prefix(prefix) {
                Some(rest) => Some(rest.to_owned()),
                None if outside_ok => Some(format!("{}{}", "../".repeat(up), e.path)),
                None => None,
            }
        });
    let found = matching(current, paths.map(|p| (p, None)));
    if found.is_empty() {
        files(current)
    } else {
        found
    }
}

/// The current directory below the worktree root, as `sub/dir/` (empty at the
/// root), the prefix git status paths carry.
fn cwd_in_repo(workdir: &std::path::Path) -> Option<String> {
    let root = workdir.canonicalize().ok()?;
    let cwd = std::env::current_dir().ok()?.canonicalize().ok()?;
    let rel = cwd.strip_prefix(root).ok()?;
    let mut prefix = rel.to_string_lossy().replace('\\', "/");
    if !prefix.is_empty() {
        prefix.push('/');
    }
    Some(prefix)
}

/// `stash@{N}` with the stash message as help, from the stash reflog.
pub fn stashes(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(backend) = backend() else {
        return Vec::new();
    };
    let Ok(log) = std::fs::read_to_string(backend.common_dir().join("logs/refs/stash")) else {
        return Vec::new();
    };
    let messages: Vec<&str> = log
        .lines()
        .map(|l| l.split_once('\t').map_or("", |(_, message)| message))
        .collect();
    matching(
        current,
        messages
            .iter()
            .rev()
            .enumerate()
            .map(|(i, m)| (format!("stash@{{{i}}}"), Some((*m).to_owned()))),
    )
}

/// Tags only.
pub fn tags(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(backend) = backend() else {
        return Vec::new();
    };
    let Ok(refs) = backend.refs() else {
        return Vec::new();
    };
    matching(
        current,
        refs.into_iter()
            .filter(|r| r.kind == RefKind::Tag)
            .map(|r| (r.name, None)),
    )
}

/// Local branches only.
pub fn branches(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(backend) = backend() else {
        return Vec::new();
    };
    let Ok(refs) = backend.refs() else {
        return Vec::new();
    };
    matching(
        current,
        refs.into_iter()
            .filter(|r| r.kind == RefKind::Local)
            .map(|r| (r.name, None)),
    )
}

/// The words of the command line being completed, the program name first.
fn line_words() -> Vec<String> {
    std::env::args_os()
        .skip_while(|a| a != "--")
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// Whether one of `flags` is on the command line being completed.
fn line_has(flags: &[&str]) -> bool {
    line_words().iter().any(|w| flags.contains(&w.as_str()))
}

/// How many values the command `name` already has before the word being
/// completed. The engine's own index does not tell the second value from the
/// first.
fn values_before(name: &str) -> usize {
    let words = line_words();
    let Some(at) = words.iter().position(|w| w == name) else {
        return 0;
    };
    let current = words.len().saturating_sub(1);
    words
        .iter()
        .enumerate()
        .skip(at + 1)
        .filter(|(i, w)| *i < current && !w.starts_with('-'))
        .count()
}

/// `tag <name> [<rev>]`: tags to delete or verify, else a name, then a rev.
struct TagNames;

impl ValueCompleter for TagNames {
    fn complete(&self, current: &OsStr) -> Vec<CompletionCandidate> {
        if values_before("tag") == 0 || line_has(&["-d", "--delete", "-v", "--verify"]) {
            tags(current)
        } else {
            revs(current)
        }
    }
}

/// `branch <name> [<start>]`: branches to delete, move or copy, else a name,
/// then a start point.
struct BranchNames;

impl ValueCompleter for BranchNames {
    fn complete(&self, current: &OsStr) -> Vec<CompletionCandidate> {
        let acts = [
            "-d", "-D", "-m", "-M", "-c", "-C", "--delete", "--move", "--copy",
        ];
        // The value of `--contains` and the like is that option's to complete.
        let words = line_words();
        let previous = words.len().checked_sub(2).map(|i| words[i].as_str());
        if matches!(
            previous,
            Some("--contains" | "--no-contains" | "--merged" | "--no-merged" | "--points-at")
        ) {
            return Vec::new();
        }
        if values_before("branch") == 0 || line_has(&acts) {
            branches(current)
        } else {
            revs(current)
        }
    }
}

/// Directories under the current directory.
pub fn dirs(current: &OsStr) -> Vec<CompletionCandidate> {
    PathCompleter::dir().complete(current)
}

/// How an argument gets its candidates.
enum Kind {
    Values(fn(&OsStr) -> Vec<CompletionCandidate>),
    /// A fixed set that is offered but not enforced, so parsing is unchanged.
    Fixed(&'static [&'static str]),
    Tags,
    Branches,
}

impl Kind {
    fn apply(self, arg: clap::Arg) -> clap::Arg {
        match self {
            Kind::Values(f) => arg.add(ArgValueCompleter::new(f)),
            Kind::Tags => arg.add(ArgValueCompleter::new(TagNames)),
            Kind::Branches => arg.add(ArgValueCompleter::new(BranchNames)),
            Kind::Fixed(set) => arg.add(ArgValueCandidates::new(move || {
                set.iter().map(CompletionCandidate::new).collect::<Vec<_>>()
            })),
        }
    }
}

/// Paths of the linked worktrees.
pub fn worktrees(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(backend) = backend() else {
        return Vec::new();
    };
    let Ok(worktrees) = backend.worktrees() else {
        return Vec::new();
    };
    matching(
        current,
        worktrees
            .into_iter()
            .filter(|w| !w.is_main)
            .map(|w| (w.path, w.branch)),
    )
}

/// Lane names.
pub fn lanes(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(backend) = backend() else {
        return Vec::new();
    };
    let Ok(state) = backend.lanes_state() else {
        return Vec::new();
    };
    matching(current, state.lanes.into_iter().map(|l| (l.name, None)))
}

/// The value name help shows for an argument, else its id in capitals.
fn value_name(arg: &clap::Arg) -> String {
    arg.get_value_names()
        .and_then(|names| names.first())
        .map_or_else(|| arg.get_id().as_str().to_uppercase(), |n| n.to_string())
}

/// The completion for one argument of the command at `path` (`"bisect good"`):
/// by command and id where the same placeholder means different things, else
/// by the placeholder its help shows. `None` leaves it free text, which the
/// test in this file makes `FREE_TEXT` name.
fn kind(path: &str, id: &str, name: &str) -> Option<Kind> {
    use Kind::{Fixed, Values};
    let by_command = match (path, id) {
        ("commit" | "discard" | "stash" | "stash push", "paths") => Some(Values(changed)),
        ("remote set-branches", "branches") => Some(Values(revs)),
        ("remote set-branches", "name") => Some(Values(remotes)),
        ("worktree lock" | "worktree move" | "worktree remove" | "worktree unlock", "name") => {
            Some(Values(worktrees))
        }
        ("update-ref" | "symbolic-ref", "name" | "old" | "new") => Some(Values(revs)),
        ("hook run", "name") => Some(Fixed(&[
            "applypatch-msg",
            "commit-msg",
            "fsmonitor-watchman",
            "post-checkout",
            "post-commit",
            "post-merge",
            "post-rewrite",
            "pre-applypatch",
            "pre-commit",
            "pre-merge-commit",
            "pre-push",
            "pre-rebase",
            "prepare-commit-msg",
            "push-to-checkout",
            "reference-transaction",
        ])),
        ("var", "name") => Some(Fixed(&[
            "GIT_AUTHOR_IDENT",
            "GIT_COMMITTER_IDENT",
            "GIT_DEFAULT_BRANCH",
            "GIT_EDITOR",
            "GIT_PAGER",
            "GIT_SHELL_PATH",
        ])),
        ("archive", "format") => Some(Fixed(&["tar", "zip", "tar.gz", "tgz"])),
        ("scalar run", "task") => Some(Fixed(&[
            "all",
            "config",
            "commit-graph",
            "fetch",
            "loose-objects",
            "pack-files",
        ])),
        (
            "remote get-url" | "remote remove" | "remote rename" | "remote set-url"
            | "remote set-head" | "remote prune" | "remote show" | "remote update",
            "name" | "names" | "old",
        ) => Some(Values(remotes)),
        ("tag", "names") => Some(Kind::Tags),
        ("verify-tag", "tags") => Some(Values(tags)),
        ("branch", "args") => Some(Kind::Branches),
        ("branch delete", "names") | ("branch checkout", "name") | ("branch rename", "old") => {
            Some(Values(branches))
        }
        ("lanes assign" | "lanes commit" | "lanes push" | "lanes pr", "lane")
        | ("lanes delete", "name")
        | ("lanes rename", "old")
        | ("lanes stack", "on") => Some(Values(lanes)),
        (
            "blame" | "reflog" | "cat-file" | "rev-parse" | "grep" | "merge-tree" | "diff-tree"
            | "diff-index",
            "args",
        ) => Some(Values(revs_or_paths)),
        ("init" | "worktree add" | "worktree move", "path" | "new_path")
        | ("bugreport" | "diagnose", "output_directory")
        | ("clone", "reference" | "reference_if_able") => Some(Values(dirs)),
        ("merge-file", "base" | "current" | "other") | ("mailinfo", "msg" | "patch") => {
            Some(Values(files))
        }
        ("archive", "remote") => Some(Values(remotes)),
        (
            "remote add"
            | "clone"
            | "scalar clone"
            | "submodule add"
            | "submodule set-branch"
            | "pull"
            | "forge branch delete",
            "branch" | "master" | "track",
        ) => return None,
        ("maintenance run", "task") => Some(Fixed(&[
            "prefetch",
            "loose-objects",
            "incremental-repack",
            "gc",
            "commit-graph",
            "pack-refs",
            "reflog-expire",
            "worktree-prune",
            "rerere-gc",
        ])),
        ("diagnose", "mode") => Some(Fixed(&["basic", "all"])),
        (p, "action") if p.starts_with("credential") => Some(Fixed(&["get", "store", "erase"])),
        _ => None,
    };
    if by_command.is_some() {
        return by_command;
    }
    Some(match (id, name) {
        ("relative", _) => Values(dirs),
        ("color", _) => Fixed(&["always", "never", "auto"]),
        ("date", "STYLE") => Fixed(&[
            "default",
            "relative",
            "local",
            "iso",
            "iso-strict",
            "rfc",
            "short",
            "raw",
            "unix",
            "human",
        ]),
        ("decorate", _) => Fixed(&["short", "full", "auto", "no"]),
        ("no_walk", _) => Fixed(&["sorted", "unsorted"]),
        ("color_moved", _) => Fixed(&["no", "default", "plain", "blocks", "zebra", "dimmed-zebra"]),
        ("diff_algorithm", _) => Fixed(&["myers", "minimal", "patience", "histogram"]),
        ("ws_error_highlight", _) => Fixed(&["context", "old", "new", "all", "none"]),
        ("diff_merges", _) => Fixed(&[
            "off",
            "first-parent",
            "separate",
            "combined",
            "dense-combined",
            "remerge",
        ]),
        ("recurse_submodules", _) => Fixed(&["check", "on-demand", "only", "no"]),
        ("missing", _) => Fixed(&["error", "allow-any", "allow-promisor", "print"]),
        ("whitespace", _) => Fixed(&["nowarn", "warn", "fix", "strip", "error", "error-all"]),
        ("cleanup", _) => Fixed(&["strip", "whitespace", "verbatim", "scissors", "default"]),
        ("object_format", _) => Fixed(&["sha1"]),
        ("kind", _) => Fixed(&["blob", "tree", "commit", "tag"]),
        ("schedule", _) => Fixed(&["hourly", "daily", "weekly"]),
        ("scheduler", _) => Fixed(&["auto", "launchctl", "systemd-timer", "crontab"]),
        ("maintenance", _) => Fixed(&["enable", "disable", "keep"]),
        ("ref_action", _) => Fixed(&["update", "print"]),
        ("mirror", _) => Fixed(&["fetch", "push"]),
        ("track", "direct|inherit") => Fixed(&["direct", "inherit"]),
        ("disk_usage", _) => Fixed(&["human"]),
        ("diagnose", _) => Fixed(&["basic", "all"]),
        (
            _,
            "FILE" | "PATH" | "PATHS" | "BODY_FILE" | "OUTPUT" | "INDEX" | "PACK" | "PACKS"
            | "SOCKET" | "MBOX" | "MBOXES" | "PATCHES" | "ATTR_OR_PATH",
        ) => Values(files),
        (_, "DIR" | "ROOT" | "GIT_DIR" | "ENLISTMENT") => Values(dirs),
        (_, "REMOTE" | "REPOSITORY") => Values(remotes),
        (
            _,
            "REV" | "REVS" | "COMMIT" | "COMMITS" | "TREE-ISH" | "TREE" | "TREES" | "REVISION"
            | "REVISION-RANGE" | "OBJECT" | "OBJECTS" | "BLOB" | "HEAD" | "UPSTREAM" | "LIMIT"
            | "START" | "END" | "A" | "B" | "REFS" | "REFNAMES" | "REFSPECS" | "BRANCH"
            | "BRANCHES" | "BASE" | "ON" | "AFTER" | "BEFORE" | "FROM" | "TO" | "REF" | "NOTES_REF"
            | "NEWBASE" | "PARENT" | "TYPE_OR_OBJECT",
        ) => Values(revs),
        _ => return None,
    })
}

fn takes_value(arg: &clap::Arg) -> bool {
    arg.get_num_args().is_some_and(|n| n.takes_values()) && !arg.is_hide_set()
}

/// Whether the value is text: numbers and other typed values have nothing to
/// complete.
fn is_text(arg: &clap::Arg) -> bool {
    use clap::builder::ValueParser;
    let id = arg.get_value_parser().type_id();
    id == ValueParser::string().type_id()
        || id == ValueParser::os_string().type_id()
        || id == ValueParser::path_buf().type_id()
}

fn has_custom(arg: &clap::Arg) -> bool {
    arg.get::<ArgValueCompleter>().is_some() || arg.get::<ArgValueCandidates>().is_some()
}

fn has_completion(arg: &clap::Arg) -> bool {
    has_custom(arg)
        || !arg.get_possible_values().is_empty()
        || arg.get_value_hint() != clap::ValueHint::Unknown
}

/// Give every value argument without a completion the one `kind` names for
/// it, in the command and all its subcommands.
pub fn attach(mut cmd: clap::Command) -> clap::Command {
    // Built, so every argument has its final value count.
    cmd.build();
    walk(cmd, "")
}

fn walk(mut cmd: clap::Command, path: &str) -> clap::Command {
    let ids: Vec<clap::Id> = cmd
        .get_arguments()
        .filter(|a| takes_value(a) && is_text(a) && !has_completion(a))
        .map(|a| a.get_id().clone())
        .collect();
    for id in ids {
        let name = cmd
            .get_arguments()
            .find(|a| a.get_id() == &id)
            .map(value_name)
            .unwrap_or_default();
        if let Some(kind) = kind(path, id.as_str(), &name) {
            cmd = cmd.mut_arg(id, |a| kind.apply(a));
        }
    }
    let subs: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_owned())
        .collect();
    for sub in subs {
        let below = match path {
            "" => sub.clone(),
            _ => format!("{path} {sub}"),
        };
        cmd = cmd.mut_subcommand(&sub, |c| walk(c, &below));
    }
    cmd
}

/// Arguments whose value is free text, by the placeholder their help
/// shows (`MESSAGE`) or, where one placeholder means more than one
/// thing, `command:id`. Numeric arguments are free by their type.
#[cfg(test)]
const FREE_TEXT: &[(&str, &str)] = &[
    (
        "branch create:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "check-ref-format:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    ("clone:branch", "a branch on another repository"),
    (
        "column:command",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "fast-export:args",
        "arguments passed through to git or a program",
    ),
    (
        "fast-import:args",
        "arguments passed through to git or a program",
    ),
    (
        "flow start:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "fmt-merge-msg:into_name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "for-each-repo:args",
        "arguments passed through to git or a program",
    ),
    (
        "forge branch delete:branch",
        "a branch on another repository",
    ),
    (
        "forge branch delete:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge branch list:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge browse:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge issue browse:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge issue close:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge issue comment:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge issue create:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge issue edit:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge issue list:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge issue reopen:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge issue view:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr checkout:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr checks:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr close:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr comment:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr create:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr draft:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr edit:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr list:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr merge:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr ready:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr reopen:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr review:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge pr view:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge repo create:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "forge repo delete:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "forge repo view:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    ("git:args", "arguments passed through to git or a program"),
    (
        "hook run:args",
        "arguments passed through to git or a program",
    ),
    (
        "interpret-trailers:args",
        "arguments passed through to git or a program",
    ),
    (
        "lanes new:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "lanes stack:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "merge-index:args",
        "arguments passed through to git or a program",
    ),
    (
        "merge-one-file:args",
        "arguments passed through to git or a program",
    ),
    ("pull:branch", "a branch on another repository"),
    ("remote add:master", "a branch on another repository"),
    (
        "remote add:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    ("remote add:track", "a branch on another repository"),
    (
        "replace:args",
        "arguments passed through to git or a program",
    ),
    (
        "rerere:args",
        "arguments passed through to git or a program",
    ),
    ("scalar clone:branch", "a branch on another repository"),
    (
        "sparse-checkout:args",
        "arguments passed through to git or a program",
    ),
    (
        "stack new:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "stash branch:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    ("submodule add:branch", "a branch on another repository"),
    (
        "submodule add:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "submodule set-branch:branch",
        "a branch on another repository",
    ),
    (
        "submodule summary:args",
        "arguments passed through to git or a program",
    ),
    (
        "symbolic-ref:target",
        "a forge repository, issue or pull request, by number, branch or URL",
    ),
    (
        "tool:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "update-index:args",
        "arguments passed through to git or a program",
    ),
    (
        "workspace new:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "workspace remove:name",
        "a name that does not exist yet, or an identifier of another system",
    ),
    (
        "FIELD,...",
        "comma-separated column names of the agent table",
    ),
    ("REPO", "OWNER/REPO on a forge"),
    ("PROVIDER", "a forge provider name"),
    ("PROFILE", "a forge profile name from config"),
    ("HOST", "a host name or address"),
    ("ACCOUNT", "a forge account name"),
    ("ASSIGNEE", "a forge user name"),
    ("LABEL", "a forge label or a conflict marker label"),
    ("TITLE", "a forge issue or pull request title"),
    ("BODY", "a forge comment or description text"),
    ("WHAT", "a pull request number, branch or URL"),
    ("ORGANIZATION", "a forge organization name"),
    ("DESCRIPTION", "a forge repository description"),
    ("FORMAT", "a format string such as `%H %s`"),
    ("N", "a number"),
    ("N[/M]", "one number or two"),
    ("START,END", "a line range"),
    ("N[,BASE]", "a number with an optional base"),
    ("WIDTH", "a number"),
    ("NEW", "a new name"),
    ("NEW_BRANCH", "the name of a branch to create"),
    ("INITIAL_BRANCH", "the name of a branch to create"),
    ("PREFIX", "text put before each path or line"),
    ("MESSAGE", "a commit, tag or stash message"),
    ("REASON", "a reason text"),
    ("TEXT", "free text"),
    ("STRING", "free text"),
    ("REGEX", "a regular expression"),
    ("PATTERN", "a glob or regular expression"),
    ("PATTERNS", "ref or name patterns"),
    ("GLOB", "a glob"),
    ("VALUE", "a config value"),
    ("VALUE_PATTERN", "a regular expression on a config value"),
    ("KEY", "a sort key, config key or similar name"),
    ("KEY=VALUE", "a config setting"),
    ("KEYID", "a signing key id"),
    ("KEY-ID", "a signing key id"),
    ("DATE", "a date such as `2 weeks ago`"),
    ("UNTIL", "a date"),
    ("SINCE", "a date"),
    ("TIME", "a time"),
    ("URL", "a URL"),
    ("URI", "a URL"),
    ("PARAM,...", "dirstat parameters"),
    ("MODES", "a list of whitespace modes"),
    ("OPTIONS", "a list of column options"),
    ("OPTION", "an option passed to another tool"),
    ("SPEC", "a partial-clone filter spec"),
    ("COMMAND", "a shell command"),
    ("CMD", "a shell command"),
    ("PROGRAM", "a program name"),
    ("TOOL", "the name of a configured diff or merge tool"),
    ("TERM", "a bisect term"),
    ("MARK", "text appended to a description"),
    ("AUTHOR", "a name or pattern"),
    ("COMMITTER", "a name or pattern"),
    ("NAME <EMAIL>", "an author identity"),
    ("IDENT", "an identity"),
    ("ADDR", "an email address"),
    ("ID", "a message id"),
    ("HEADER", "a mail header"),
    ("BOUNDARY", "a MIME boundary"),
    ("SFX", "a file name suffix"),
    ("REFSPEC", "a refspec"),
    ("RANGE:FILE", "a line range and a file"),
    ("PATH:CONTENT", "a path and the file content"),
    ("PERMISSIONS", "a permission mode"),
    ("ORIGIN", "the name of the origin remote to create"),
    ("MSG", "a text"),
    ("MODE", "a layout mode"),
    ("ENCODING", "a character encoding name"),
    ("CONTACTS", "name or email contacts"),
    ("CLONE_BASE", "a base URL"),
    ("BASE_NAME", "a file name prefix"),
    ("ACDMRT", "diff filter letters"),
    ("QUERY", "a search query"),
    ("VERSION", "a release version"),
    ("PRESET", "a workflow preset name"),
    ("TOKEN[(=|:)VALUE]", "a trailer token and value"),
];

#[cfg(test)]
fn is_free(path: &str, arg: &clap::Arg) -> bool {
    let numeric = !is_text(arg);
    let name = value_name(arg);
    let qualified = format!("{path}:{}", arg.get_id());
    numeric
        || FREE_TEXT
            .iter()
            .any(|(key, _)| *key == name || *key == qualified)
}

#[cfg(test)]
fn walk_args(cmd: &clap::Command, path: &str, visit: &mut dyn FnMut(&str, &clap::Arg)) {
    for arg in cmd.get_arguments().filter(|a| takes_value(a)) {
        visit(path, arg);
    }
    for sub in cmd.get_subcommands() {
        let below = match path {
            "" => sub.get_name().to_owned(),
            _ => format!("{path} {}", sub.get_name()),
        };
        walk_args(sub, &below, visit);
    }
}

#[cfg(test)]
fn tree() -> clap::Command {
    attach(<crate::cli::Cli as clap::CommandFactory>::command())
}

#[test]
fn coverage_every_value_argument_completes() {
    let mut gaps = Vec::new();
    walk_args(&tree(), "", &mut |path, arg| {
        if !has_completion(arg) && !is_free(path, arg) {
            gaps.push(format!(
                "rgit {path}, {} [{}]",
                arg.get_id(),
                value_name(arg)
            ));
        }
    });
    // `attach` must leave numbers alone; a derive-time completer is fine.
    let mut own = Vec::new();
    let mut raw = <crate::cli::Cli as clap::CommandFactory>::command();
    raw.build();
    walk_args(&raw, "", &mut |path, arg| {
        if has_custom(arg) {
            own.push(format!("{path}:{}", arg.get_id()));
        }
    });
    walk_args(&tree(), "", &mut |path, arg| {
        let key = format!("{path}:{}", arg.get_id());
        if !is_text(arg) && has_custom(arg) && !own.contains(&key) {
            gaps.push(format!(
                "rgit {path}, {} is numeric but attach gave it a completion",
                arg.get_id()
            ));
        }
    });
    gaps.sort();
    gaps.dedup();
    assert!(
        gaps.is_empty(),
        "{} value arguments have no completion and are not free text:\n{}",
        gaps.len(),
        gaps.join("\n")
    );
}

#[test]
fn coverage_free_text_list_has_no_stale_keys() {
    let mut seen = Vec::new();
    walk_args(&tree(), "", &mut |path, arg| {
        seen.push((value_name(arg), format!("{path}:{}", arg.get_id())));
    });
    for (key, reason) in FREE_TEXT {
        assert!(!reason.is_empty(), "{key} has no reason");
        assert!(
            seen.iter()
                .any(|(name, qualified)| key == name || key == qualified),
            "free-text key {key} is not in the command tree"
        );
    }
}
