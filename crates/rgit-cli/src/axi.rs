//! Structured results for the agent output modes. Each list or detail command
//! gets a small default schema, aggregate counts, a definitive empty state, and
//! next-step hints; every other command falls back to its text message.

use std::sync::Arc;

use rgit_git::{
    BlameLine, CommitDetails, FileDiff, GitBackend, LineOrigin, LogEntry, LogOptions, RefKind,
    RepoStatus, SmartlogEntry, StatusCode,
};

use crate::cli::{
    BranchCmd, Command, FlowCmd, IndexCmd, LanesCmd, NotesCmd, RemoteCmd, StackCmd, StashCmd,
    WorkspaceCmd, WorktreeCmd,
};
use crate::output::Output;
use crate::render;

const BLAME_LINES: usize = 200;

/// Run a subcommand for the agent output modes.
pub fn run(
    backend: &Arc<dyn GitBackend>,
    command: Command,
    interactive: bool,
) -> anyhow::Result<Output> {
    let done = done_message(&command);
    let next = next_steps(&command);
    let finish = |text: String| {
        Output::from(if text == "ok" {
            done.clone().unwrap_or(text)
        } else {
            text
        })
    };
    Ok(match command {
        Command::Status {
            untracked,
            ignored,
            paths,
            ..
        } => run_status(&crate::cli::status_view(
            backend,
            &paths,
            untracked.as_deref(),
            ignored,
        )?),
        Command::Log { format, .. } if !format.any() => {
            let opts: LogOptions = crate::cli::log_options(backend, &command)?;
            let filtered = opts.author.is_some()
                || opts.since.is_some()
                || opts.until.is_some()
                || !opts.paths.is_empty()
                || !opts.grep.is_empty()
                || opts.merges.is_some();
            let entries = backend.log(&opts)?;
            let total = if entries.len() < opts.limit {
                Some(entries.len())
            } else if filtered {
                None
            } else {
                let mut args = vec!["rev-list".to_owned(), "--count".to_owned()];
                if opts.first_parent {
                    args.push("--first-parent".to_owned());
                }
                if opts.all {
                    args.push("--all".to_owned());
                } else if opts.revs.is_empty() {
                    args.push("HEAD".to_owned());
                }
                args.extend(opts.revs.iter().cloned());
                backend.git(&args).ok().and_then(|n| n.trim().parse().ok())
            };
            let mut base = String::from("rgit log");
            if opts.all {
                base.push_str(" --all");
            }
            for rev in &opts.revs {
                base.push(' ');
                base.push_str(rev);
            }
            log(&entries, total, filtered, &base)
        }
        Command::Diff {
            format,
            ref revs,
            cached,
            ..
        } => {
            let (files, scope) = crate::cli::diff_files(backend, &command)?;
            let mut base = String::from("rgit diff");
            for rev in revs {
                base.push(' ');
                base.push_str(rev);
            }
            if cached {
                base.push_str(" --cached");
            }
            if format.name_status || format.numstat || (format.patch && format.stat) {
                Output::new(crate::cli::diff_out(&files, format))
            } else {
                diff(&files, &scope, &base, format.patch, format.name_only)
            }
        }
        Command::Show {
            ref revs,
            ref paths,
            format,
            no_patch,
        } if revs.len() <= 1
            && !revs.iter().any(|r| r.contains(':'))
            && !(format.name_status || format.numstat) =>
        {
            let mut c = backend.commit_details(revs.first().map_or("HEAD", String::as_str))?;
            if !paths.is_empty() {
                c.files
                    .retain(|f| rgit_git::pathspec_matches(paths, &f.path));
            }
            if no_patch {
                c.files.clear();
            }
            show(&c, format.patch && !no_patch, format.name_only && !no_patch)
        }
        Command::Blame { args, lines } => {
            let path = args.join(" ");
            let all = crate::cli::blame(backend, &args).map_err(|e| match e {
                rgit_git::GitError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                    anyhow::Error::new(crate::cli::CliError {
                        message: format!("no file {path} in this repository"),
                        help: Some("Run `rgit git ls-files` to list tracked files".to_owned()),
                        code: 1,
                    })
                }
                other => other.into(),
            })?;
            let (start, end) = match lines {
                Some(spec) => crate::cli::parse_line_range(&spec)?,
                None => (1, BLAME_LINES),
            };
            blame(&all, &path, start, end)
        }
        Command::Refs => {
            let refs = backend.refs()?;
            let rows = refs
                .iter()
                .map(|r| {
                    crate::obj! {
                        "name" => r.name,
                        "kind" => kind(r.kind),
                        "head" => r.is_head,
                    }
                })
                .collect();
            Output::new(render::refs(&refs)).list(
                "refs",
                rows,
                &["name", "kind", "head"],
                "0 refs in this repository",
            )
        }
        Command::Branch {
            cmd: None,
            all,
            remotes,
        } => {
            let current = backend.status().ok().and_then(|s| s.head.branch);
            let mut names = if remotes {
                Vec::new()
            } else {
                backend.local_branches()?
            };
            if all || remotes {
                names.extend(backend.remote_branches()?);
            }
            let rows = names
                .iter()
                .map(|n| crate::obj! { "name" => n, "current" => Some(n) == current.as_ref() })
                .collect();
            Output::new(render::branches(&names, current.as_deref()))
                .list(
                    "branches",
                    rows,
                    &["name", "current"],
                    if remotes {
                        "0 remote-tracking branches"
                    } else {
                        "0 branches in this repository"
                    },
                )
                .help("Run `rgit checkout <branch>` to switch branches")
                .help("Run `rgit branch create <name>` to add a branch")
        }
        Command::Branch {
            cmd: Some(BranchCmd::Create { name }),
            ..
        } if backend.local_branches()?.contains(&name) => {
            Output::message(format!("branch {name} already exists (no-op)"))
        }
        Command::Branch {
            cmd: Some(BranchCmd::Delete {
                name: Some(name), ..
            }),
            ..
        } if !backend.local_branches()?.contains(&name) => {
            Output::message(format!("branch {name} does not exist (no-op)"))
        }
        Command::Tag {
            name: None,
            delete: None,
            ..
        } => {
            let tags = backend.all_tags()?;
            let text = if tags.is_empty() {
                "no tags".to_owned()
            } else {
                tags.iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let rows = tags
                .iter()
                .map(|t| crate::obj! { "name" => t.name, "when" => t.when, "message" => t.message })
                .collect();
            Output::new(text)
                .list("tags", rows, &["name", "when"], "0 tags in this repository")
                .help("Run `rgit tag <name> -m \"<message>\"` to create a tag")
        }
        Command::Tag {
            name: Some(name),
            force: false,
            delete: None,
            ..
        } if tag_exists(backend, &name)? => {
            Output::message(format!("tag {name} already exists (no-op)"))
        }
        Command::Tag {
            delete: Some(name), ..
        } if !tag_exists(backend, &name)? => {
            Output::message(format!("tag {name} does not exist (no-op)"))
        }
        Command::Stash {
            cmd: Some(StashCmd::List),
        } => {
            let stashes = backend.status()?.stashes;
            let rows = stashes
                .iter()
                .map(|s| crate::obj! { "index" => s.index, "message" => s.message })
                .collect();
            Output::new(render::stashes(&stashes))
                .list("stashes", rows, &["index", "message"], "0 stashes")
                .help("Run `rgit stash pop <index>` to restore a stash")
        }
        Command::Remote { cmd: None } => {
            let remotes = backend.remotes()?;
            let rows = remotes
                .iter()
                .map(|r| crate::obj! { "name" => r.name, "url" => r.url })
                .collect();
            Output::new(render::remotes(&remotes))
                .list("remotes", rows, &["name", "url"], "0 remotes configured")
                .help("Run `rgit remote add <name> <url>` to add a remote")
        }
        Command::Remote {
            cmd: Some(RemoteCmd::Add { name, url }),
        } if backend.remotes()?.iter().any(|r| r.name == name) => {
            let existing = backend
                .remotes()?
                .into_iter()
                .find(|r| r.name == name)
                .expect("remote checked above");
            if existing.url == url {
                Output::message(format!("remote {name} already points at {url} (no-op)"))
            } else {
                return Err(anyhow::Error::new(crate::cli::CliError {
                    message: format!("remote {name} already exists with url {}", existing.url),
                    help: Some(format!(
                        "Run `rgit remote set-url {name} {url}` to change it"
                    )),
                    code: 1,
                }));
            }
        }
        Command::Remote {
            cmd: Some(RemoteCmd::Remove { name }),
        } if !backend.remotes()?.iter().any(|r| r.name == name) => {
            Output::message(format!("remote {name} does not exist (no-op)"))
        }
        Command::Worktree { cmd: None } => {
            let list = backend.worktrees()?;
            let rows = list
                .iter()
                .map(|w| {
                    crate::obj! {
                        "name" => w.name,
                        "branch" => w.branch.as_deref().unwrap_or("(detached)"),
                        "path" => w.path,
                        "head" => w.head,
                        "dirty" => w.dirty,
                        "locked" => w.locked,
                        "main" => w.is_main,
                    }
                })
                .collect();
            Output::new(render::worktrees(&list)).list(
                "worktrees",
                rows,
                &["name", "branch", "path"],
                "0 worktrees",
            )
        }
        Command::Worktree {
            cmd: Some(WorktreeCmd::Remove { name, .. }),
        } if !backend.worktrees()?.iter().any(|w| w.name == name) => {
            Output::message(format!("worktree {name} does not exist (no-op)"))
        }
        Command::Remote {
            cmd: Some(RemoteCmd::Rename { old, new }),
        } if !backend.remotes()?.iter().any(|r| r.name == old)
            && backend.remotes()?.iter().any(|r| r.name == new) =>
        {
            Output::message(format!("remote {old} is already named {new} (no-op)"))
        }
        Command::Branch {
            cmd: Some(BranchCmd::Rename { old, new }),
            ..
        } if !backend.local_branches()?.contains(&old)
            && backend.local_branches()?.contains(&new) =>
        {
            Output::message(format!("branch {old} is already named {new} (no-op)"))
        }
        Command::Branch {
            cmd: Some(BranchCmd::Checkout { name }),
            ..
        }
        | Command::Checkout {
            rev: Some(name),
            branch: None,
        } if backend.status()?.head.branch.as_deref() == Some(name.as_str()) => {
            Output::message(format!("already on {name} (no-op)"))
        }
        Command::Worktree {
            cmd: Some(WorktreeCmd::Add { name, .. }),
        } if backend.worktrees()?.iter().any(|w| w.name == name) => {
            Output::message(format!("worktree {name} already exists (no-op)"))
        }
        Command::Stack {
            cmd: Some(StackCmd::New { name }),
        } if backend.local_branches()?.contains(&name) => {
            return Err(anyhow::Error::new(crate::cli::CliError {
                message: format!("branch {name} already exists"),
                help: Some(format!("Run `rgit checkout {name}` to switch to it")),
                code: 1,
            }));
        }
        Command::Lanes {
            cmd: None | Some(LanesCmd::List),
        } => lanes(backend)?,
        Command::Lanes {
            cmd: Some(LanesCmd::New { name }),
        } if backend
            .lanes_state()
            .is_ok_and(|s| s.lanes.iter().any(|l| l.name == name)) =>
        {
            Output::message(format!("lane {name} already exists (no-op)"))
        }
        Command::Lanes {
            cmd: Some(LanesCmd::Delete { name }),
        } if backend
            .lanes_state()
            .is_ok_and(|s| !s.lanes.iter().any(|l| l.name == name)) =>
        {
            Output::message(format!("lane {name} does not exist (no-op)"))
        }
        Command::Workspace {
            cmd: None | Some(WorkspaceCmd::List),
        } => {
            let list = rgit_git::workspace::entries(backend.as_ref());
            let rows = list
                .iter()
                .map(|w| {
                    crate::obj! {
                        "name" => w.name,
                        "branch" => w.branch,
                        "path" => w.path.display().to_string(),
                    }
                })
                .collect();
            let text = rgit_git::workspace::list(backend.as_ref())?;
            Output::new(text)
                .list(
                    "workspaces",
                    rows,
                    &["name", "branch", "path"],
                    "0 workspaces",
                )
                .help("Run `rgit workspace new <name>` to add a workspace")
        }
        Command::Workspace {
            cmd: Some(WorkspaceCmd::New { name }),
        } if rgit_git::workspace::entries(backend.as_ref())
            .iter()
            .any(|w| w.name == name) =>
        {
            Output::message(format!("workspace {name} already exists (no-op)"))
        }
        Command::Workspace {
            cmd: Some(WorkspaceCmd::Remove { name }),
        } if !rgit_git::workspace::entries(backend.as_ref())
            .iter()
            .any(|w| w.name == name) =>
        {
            Output::message(format!("workspace {name} does not exist (no-op)"))
        }
        Command::Flow {
            cmd: FlowCmd::Status,
        } => {
            let fields = rgit_git::workflow::describe(backend.as_ref()).map_err(|e| {
                anyhow::Error::new(crate::cli::CliError {
                    message: e.to_string(),
                    help: Some(
                        "Run `rgit flow init <preset>` (gitflow, github, gitlab, trunk, release-flow)"
                            .to_owned(),
                    ),
                    code: 1,
                })
            })?;
            let text = rgit_git::workflow::status(backend.as_ref())?;
            let mut out = Output::new(text);
            for (key, value) in fields {
                out = out.with(key, value);
            }
            out
        }
        Command::Index {
            action: IndexCmd::Search { query, limit, root },
        } => {
            let (hits, multi) = crate::cli::semantic_hits(backend, root.as_deref(), &query, limit)?;
            if hits.is_empty()
                && root.is_none()
                && !rgit_index::index_path(backend.workdir()).exists()
            {
                return Err(anyhow::Error::new(crate::cli::CliError {
                    message: "no semantic index for this repository".to_owned(),
                    help: Some("Run `rgit index build` to create it".to_owned()),
                    code: 1,
                }));
            }
            let text = crate::cli::semantic_search(backend, root.as_deref(), &query, limit)?;
            let rows = hits
                .iter()
                .map(|(score, repo, h)| {
                    let path = if multi {
                        format!("{repo}/{}", h.path)
                    } else {
                        h.path.clone()
                    };
                    crate::obj! {
                        "path" => path,
                        "lines" => format!("{}-{}", h.start_line, h.end_line),
                        "score" => (score * 1000.0).round() / 1000.0,
                        "preview" => h.preview,
                    }
                })
                .collect();
            Output::new(text).list(
                "hits",
                rows,
                &["path", "lines", "score"],
                format!("0 matches for {query:?}"),
            )
        }
        Command::Index {
            action: IndexCmd::Code { query, limit, root },
        } => {
            let (hits, multi) = crate::cli::code_hits(backend, root.as_deref(), &query, limit)?;
            let text = crate::cli::code_search(backend, root.as_deref(), &query, limit)?;
            let rows = hits
                .iter()
                .map(|h| {
                    let path = if multi {
                        format!("{}/{}", h.repo, h.path)
                    } else {
                        h.path.clone()
                    };
                    crate::obj! {
                        "path" => path,
                        "line" => h.line,
                        "source" => h.tag,
                        "score" => (h.score * 10000.0).round() / 10000.0,
                    }
                })
                .collect();
            Output::new(text).list(
                "hits",
                rows,
                &["path", "line", "source"],
                format!("0 matches for {query:?}"),
            )
        }
        Command::Index {
            action: IndexCmd::Status,
        } => {
            let path = rgit_index::index_path(backend.workdir());
            match rgit_index::load(&path) {
                Some(index) => Output::new(format!(
                    "indexed: {} chunks ({})",
                    index.len(),
                    path.display()
                ))
                .with("chunks", index.len())
                .with("path", path.display().to_string()),
                None => Output::new(format!("no index ({})", path.display()))
                    .with("index", "none")
                    .with("path", path.display().to_string())
                    .help("Run `rgit index build` to create it"),
            }
        }
        Command::Oplog => {
            let ops = backend.oplog()?;
            let rows = ops
                .iter()
                .map(|o| {
                    crate::obj! {
                        "id" => o.short_id,
                        "label" => o.label,
                        "head" => o.head,
                        "when" => o.when,
                    }
                })
                .collect();
            Output::new(render::oplog(&ops))
                .list(
                    "ops",
                    rows,
                    &["id", "label", "head", "when"],
                    "0 operations recorded",
                )
                .help("Run `rgit undo` to undo the newest operation")
        }
        Command::Smartlog => smartlog(&backend.smartlog()?),
        Command::Stack {
            cmd: None | Some(StackCmd::List),
        } => stack(backend)?,
        other => {
            let mut out = finish(crate::cli::run(backend, other, interactive)?);
            out.help.extend(next);
            out
        }
    })
}

fn kind(k: RefKind) -> &'static str {
    match k {
        RefKind::Local => "local",
        RefKind::Remote => "remote",
        RefKind::Tag => "tag",
    }
}

fn code(c: StatusCode) -> &'static str {
    match c {
        StatusCode::Unmodified => "-",
        other => other.letter(),
    }
}

fn tag_exists(backend: &Arc<dyn GitBackend>, name: &str) -> anyhow::Result<bool> {
    Ok(backend.all_tags()?.iter().any(|t| t.name == name))
}

pub(crate) fn line_counts(f: &FileDiff) -> (usize, usize) {
    let (mut add, mut del) = (0, 0);
    for l in f.hunks.iter().flat_map(|h| &h.lines) {
        match l.origin {
            LineOrigin::Added => add += 1,
            LineOrigin::Removed => del += 1,
            _ => {}
        }
    }
    (add, del)
}

fn stat_rows(files: &[FileDiff]) -> (Vec<crate::toon::Obj>, String) {
    let (mut add, mut del) = (0, 0);
    let rows = files
        .iter()
        .map(|f| {
            let (a, d) = line_counts(f);
            add += a;
            del += d;
            crate::obj! { "path" => f.path, "added" => a, "removed" => d, "binary" => f.binary }
        })
        .collect();
    (rows, format!("{} files, +{add} -{del}", files.len()))
}

/// The structured working-tree status, also used by the home view.
pub fn run_status(s: &RepoStatus) -> Output {
    let mut out = Output::new(render::status(s)).with("branch", s.head.describe());
    if let Some(upstream) = &s.head.upstream {
        out = out
            .with("upstream", upstream.as_str())
            .with("ahead", s.head.ahead)
            .with("behind", s.head.behind);
    }
    if let Some(state) = s.state.label() {
        out = out.with("state", state);
    }
    let staged = s.entries.iter().filter(|e| e.is_staged()).count();
    let untracked = s.entries.iter().filter(|e| e.is_untracked()).count();
    let unstaged = s
        .entries
        .iter()
        .filter(|e| e.is_unstaged() && !e.is_untracked())
        .count();
    let conflicted = s
        .entries
        .iter()
        .any(|e| e.index == StatusCode::Unmerged || e.worktree == StatusCode::Unmerged);
    if !s.entries.is_empty() {
        out = out.with(
            "changes",
            format!("{staged} staged, {unstaged} unstaged, {untracked} untracked"),
        );
    }
    if !s.stashes.is_empty() {
        out = out.with("stashes", s.stashes.len());
    }
    let rows = s
        .entries
        .iter()
        .map(|e| {
            let staged = match e.index {
                StatusCode::Untracked => "-",
                other => code(other),
            };
            let unstaged = if e.is_untracked() {
                "?"
            } else {
                code(e.worktree)
            };
            crate::obj! {
                "path" => e.path,
                "staged" => staged,
                "unstaged" => unstaged,
                "from" => e.orig_path,
            }
        })
        .collect();
    out = out.list(
        "files",
        rows,
        &["path", "staged", "unstaged"],
        "0 changes; working tree clean",
    );
    match s.state.label() {
        Some("rebasing") => {
            out = out
                .help("Run `rgit rebase --continue` after resolving conflicts")
                .help("Run `rgit rebase --abort` to give up the rebase");
        }
        Some("merging") => {
            out = out
                .help("Run `rgit merge --continue` after resolving conflicts")
                .help("Run `rgit merge --abort` to give up the merge");
        }
        Some(state @ ("cherry-picking" | "reverting")) => {
            let verb = if state == "reverting" {
                "revert"
            } else {
                "cherry-pick"
            };
            out = out
                .help(format!(
                    "Run `rgit {verb} --continue` after resolving conflicts"
                ))
                .help(format!("Run `rgit {verb} --abort` to give up the {verb}"));
        }
        _ => {}
    }
    if conflicted {
        out = out.help("Run `rgit resolve <path> --ours|--theirs` to settle a conflict");
    }
    if unstaged + untracked > 0 {
        out = out
            .help("Run `rgit diff` to see unstaged changes")
            .help("Run `rgit stage <path>` to stage a file");
    }
    if staged > 0 {
        out = out.help("Run `rgit commit -m \"<message>\"` to commit staged changes");
    }
    if s.entries.is_empty() && s.head.ahead > 0 {
        out = out.help("Run `rgit push` to publish local commits");
    }
    if s.head.behind > 0 {
        out = out.help("Run `rgit pull` to integrate upstream commits");
    }
    out
}

fn log(entries: &[LogEntry], total: Option<usize>, filtered: bool, base: &str) -> Output {
    let rows = entries
        .iter()
        .map(|e| {
            crate::obj! {
                "id" => e.short_id,
                "summary" => e.summary,
                "author" => e.author,
                "when" => e.when,
                "oid" => e.oid,
                "parents" => e.parents.join(" "),
                "refs" => e.refs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(" "),
                "unpushed" => e.unpushed,
            }
        })
        .collect();
    let empty = if filtered {
        "0 commits match these filters"
    } else {
        "0 commits on this branch"
    };
    let mut out = Output::new(render::log(entries));
    match total {
        Some(total) if total > entries.len() => {
            out = out
                .with("count", format!("{} of {total} total", entries.len()))
                .help(format!(
                    "Run `{base} --limit {total}` to see all {total} commits"
                ));
        }
        None => {
            out = out
                .with("count", format!("{} shown; more exist", entries.len()))
                .help("Run the same command with a larger `--limit <n>` to see more");
        }
        Some(_) => {}
    }
    out.list("commits", rows, &["id", "summary", "author", "when"], empty)
        .help("Run `rgit show <id>` for a commit's details")
}

fn diff(files: &[FileDiff], scope: &str, base: &str, patch: bool, name_only: bool) -> Output {
    let empty = format!("0 {scope} changes");
    if name_only {
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        let out = Output::new(paths.join("\n"));
        return if paths.is_empty() {
            out.with("files", empty)
        } else {
            out.with("files", paths)
        };
    }
    let (rows, total) = stat_rows(files);
    if patch {
        let text = render::patch(files);
        return if files.is_empty() {
            Output::new(text).with("patch", empty)
        } else {
            Output::new(text.clone())
                .with("total", total)
                .long("patch", text)
        };
    }
    let mut out = Output::new(render::diffstat(files));
    if !files.is_empty() {
        out = out.with("total", total);
    }
    out = out.list("files", rows, &["path", "added", "removed"], empty);
    if !files.is_empty() {
        out = out.help(format!("Run `{base} --patch` for the full patch"));
        if scope == "unstaged" {
            out = out.help("Run `rgit stage <path>` to stage a file");
        }
    } else if scope == "unstaged" {
        out = out.help("Run `rgit diff --cached` to see staged changes");
    }
    out
}

fn show(c: &CommitDetails, patch: bool, name_only: bool) -> Output {
    if name_only {
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        return Output::new(paths.join("\n")).with("files", paths);
    }
    if patch {
        let text = render::patch(&c.files);
        return Output::new(text.clone())
            .with("id", c.id.as_str())
            .long("patch", text);
    }
    let mut lines = c.message.trim_end().lines();
    let subject = lines.next().unwrap_or_default();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    let (rows, total) = stat_rows(&c.files);
    let mut out = Output::new(render::commit_details(c))
        .with("id", c.id.as_str())
        .with("author", c.author.as_str())
        .with("when", c.when.as_str())
        .with("subject", subject);
    if !body.is_empty() {
        out = out.long("body", body);
    }
    out.with("total", total).list(
        "files",
        rows,
        &["path", "added", "removed"],
        "0 files changed",
    )
}

fn blame(all: &[BlameLine], path: &str, start: usize, end: usize) -> Output {
    let lo = start.saturating_sub(1).min(all.len());
    let hi = end.min(all.len()).max(lo);
    let shown = &all[lo..hi];
    let rows = shown
        .iter()
        .enumerate()
        .map(|(i, b)| {
            crate::obj! {
                "line" => lo + i + 1,
                "id" => b.short_id,
                "author" => b.author,
                "text" => b.line,
            }
        })
        .collect();
    let mut out = Output::new(render::blame(shown));
    if shown.len() < all.len() {
        out = out.with(
            "count",
            format!("lines {}-{hi} of {} total", lo + 1, all.len()),
        );
        if hi < all.len() {
            let next_end = (hi + BLAME_LINES).min(all.len());
            out = out.help(format!(
                "Run `rgit blame {path} -L {},{next_end}` for the next lines",
                hi + 1
            ));
        }
    }
    out.list(
        "lines",
        rows,
        &["line", "id", "author", "text"],
        format!("0 lines in {path} for that range"),
    )
}

fn smartlog(entries: &[SmartlogEntry]) -> Output {
    let rows = entries
        .iter()
        .map(|e| {
            let mark = if e.is_head {
                "head"
            } else if e.is_trunk {
                "trunk"
            } else {
                "draft"
            };
            crate::obj! {
                "id" => e.short_id,
                "mark" => mark,
                "summary" => e.summary,
                "refs" => e.refs.join(" "),
                "when" => e.when,
                "author" => e.author,
                "change" => e.change_id,
            }
        })
        .collect();
    Output::new(render::smartlog(entries))
        .list(
            "commits",
            rows,
            &["id", "mark", "summary", "refs"],
            "0 draft commits",
        )
        .help("Run `rgit show <id>` for a commit's details")
        .help("Run `rgit stack new <name>` to stack a new branch")
}

fn stack(backend: &Arc<dyn GitBackend>) -> anyhow::Result<Output> {
    let text = crate::stack::list(backend)?;
    let parents = backend.stack_parents()?;
    let current = backend.status()?.head.branch;
    let parent_of = |b: &str| {
        parents
            .iter()
            .find(|(name, _)| name == b)
            .and_then(|(_, p)| p.clone())
    };
    let mut rows = Vec::new();
    let mut cursor = current.clone();
    while let Some(branch) = cursor {
        let parent = parent_of(&branch);
        rows.push(crate::obj! {
            "branch" => branch,
            "parent" => parent,
            "current" => Some(&branch) == current.as_ref(),
        });
        cursor = parent;
    }
    if rows.len() <= 1 && parents.iter().all(|(_, p)| p.is_none()) {
        rows.clear();
    }
    Ok(Output::new(text)
        .list(
            "stack",
            rows,
            &["branch", "parent", "current"],
            "0 stacked branches",
        )
        .help("Run `rgit stack new <name>` to stack a branch on this one"))
}

fn lanes(backend: &Arc<dyn GitBackend>) -> anyhow::Result<Output> {
    let Ok(state) = backend.lanes_state() else {
        return Ok(Output::new("lanes are off")
            .with("lanes", "0 lanes; lanes are off in this repository")
            .help("Run `rgit lanes init` to start using lanes"));
    };
    let text = crate::lanes::list(backend)?;
    let rows = state
        .lanes
        .iter()
        .map(|l| {
            crate::obj! {
                "name" => l.name,
                "branch" => l.branch,
                "files" => l.paths.len() + l.hunks.len(),
                "commits" => l.commits.len(),
                "parent" => l.parent,
            }
        })
        .collect();
    Ok(Output::new(text)
        .list(
            "lanes",
            rows,
            &["name", "branch", "files", "commits"],
            "0 lanes; run `rgit lanes init` to start",
        )
        .help("Run `rgit lanes assign <lane> <path>` to move a file into a lane")
        .help("Run `rgit lanes commit <lane> -m \"<message>\"` to commit a lane"))
}

/// A specific success line for commands whose backend call only says "ok".
fn done_message(c: &Command) -> Option<String> {
    let stash = |verb: &str, index: &Option<usize>| match index {
        Some(i) => format!("{verb} stash@{{{i}}}"),
        None => format!("{verb} the newest stash"),
    };
    Some(match c {
        Command::Stage { paths, hunk, .. } if hunk.is_empty() => {
            format!("staged {}", paths.join(" "))
        }
        Command::Stage { paths, .. } => format!("staged part of {}", paths.join(" ")),
        Command::Unstage { paths, hunk, .. } if hunk.is_empty() => {
            format!("unstaged {}", paths.join(" "))
        }
        Command::Unstage { paths, .. } => format!("unstaged part of {}", paths.join(" ")),
        Command::StageAll => "staged all changes".to_owned(),
        Command::UnstageAll => "unstaged all changes".to_owned(),
        Command::Discard { paths, .. } if !paths.is_empty() => {
            format!("discarded unstaged changes to {}", paths.join(" "))
        }
        Command::Resolve { path, ours, .. } => {
            format!(
                "resolved {path} with {}",
                if *ours { "ours" } else { "theirs" }
            )
        }
        Command::Checkout {
            branch: Some(new), ..
        } => format!("created and checked out {new}"),
        Command::Checkout { rev: Some(rev), .. } => format!("checked out {rev}"),
        Command::Merge { abort: true, .. } => "merge aborted".to_owned(),
        Command::Merge { cont: true, .. } => "merge committed".to_owned(),
        Command::Merge {
            squash: true, revs, ..
        } => {
            format!(
                "squashed {} into the index; commit to finish",
                revs.join(" ")
            )
        }
        Command::Merge {
            no_commit: true,
            revs,
            ..
        } => format!("merged {} without committing", revs.join(" ")),
        Command::Merge { revs, .. } if !revs.is_empty() => format!("merged {}", revs.join(" ")),
        Command::Rebase { abort: true, .. } => "rebase aborted".to_owned(),
        Command::Rebase { cont: true, .. } => "rebase continued".to_owned(),
        Command::Rebase { skip: true, .. } => "skipped the current commit".to_owned(),
        Command::Rebase {
            onto: Some(onto), ..
        } => format!("rebased onto {onto}"),
        Command::Rebase { .. } => "rebased".to_owned(),
        Command::Reset {
            rev: Some(rev),
            soft,
            hard,
            ..
        } => {
            let mode = match (soft, hard) {
                (true, _) => "soft",
                (_, true) => "hard",
                _ => "mixed",
            };
            format!("reset to {rev} ({mode})")
        }
        Command::CherryPick { abort: true, .. } => "cherry-pick aborted".to_owned(),
        Command::CherryPick { cont: true, .. } => "cherry-pick continued".to_owned(),
        Command::CherryPick { skip: true, .. } | Command::Revert { skip: true, .. } => {
            "skipped the current commit".to_owned()
        }
        Command::CherryPick { revs, .. } if !revs.is_empty() => {
            format!("cherry-picked {}", revs.join(" "))
        }
        Command::Revert { abort: true, .. } => "revert aborted".to_owned(),
        Command::Revert { cont: true, .. } => "revert continued".to_owned(),
        Command::Revert { revs, .. } if !revs.is_empty() => format!("reverted {}", revs.join(" ")),
        Command::Branch { cmd: Some(cmd), .. } => match cmd {
            BranchCmd::Create { name } => format!("created and checked out branch {name}"),
            BranchCmd::Checkout { name } => format!("checked out {name}"),
            BranchCmd::Delete {
                name: Some(name), ..
            } => format!("deleted branch {name}"),
            BranchCmd::Rename { old, new } => format!("renamed branch {old} to {new}"),
            _ => return None,
        },
        Command::Stash { cmd: Some(cmd) } => match cmd {
            StashCmd::Pop { index } => stash("popped", index),
            StashCmd::Apply { index } => stash("applied", index),
            StashCmd::Drop { index } => stash("dropped", index),
            _ => return None,
        },
        Command::Tag {
            delete: Some(name), ..
        } => format!("deleted tag {name}"),
        Command::Tag {
            name: Some(name), ..
        } => format!("created tag {name}"),
        Command::Remote { cmd: Some(cmd) } => match cmd {
            RemoteCmd::Add { name, url } => format!("added remote {name} -> {url}"),
            RemoteCmd::Remove { name } => format!("removed remote {name}"),
            RemoteCmd::SetUrl { name, url } => format!("set remote {name} -> {url}"),
            RemoteCmd::Rename { old, new } => format!("renamed remote {old} to {new}"),
        },
        Command::Worktree { cmd: Some(cmd) } => match cmd {
            WorktreeCmd::Add { name, path } => format!("added worktree {name} at {path}"),
            WorktreeCmd::Remove { name, .. } => format!("removed worktree {name}"),
            _ => return None,
        },
        Command::Fetch { .. } => "fetched".to_owned(),
        Command::Pull { .. } => "pulled".to_owned(),
        Command::Push { .. } => "pushed".to_owned(),
        Command::Config {
            key: Some(key),
            unset,
            unset_all,
            ..
        } if *unset || *unset_all => format!("unset {key}"),
        Command::Config {
            key: Some(key),
            value: Some(value),
            get: false,
            get_all: false,
            ..
        } => format!("set {key} = {value}"),
        Command::Apply { check: true, .. } => "the patch applies cleanly".to_owned(),
        Command::Apply { stat: false, .. } => "applied the patch".to_owned(),
        Command::Notes { cmd: Some(cmd), .. } => match cmd {
            NotesCmd::Add { rev, .. } | NotesCmd::Append { rev, .. } => {
                format!("noted {}", rev.as_deref().unwrap_or("HEAD"))
            }
            NotesCmd::Remove { rev } => {
                format!("removed the note of {}", rev.as_deref().unwrap_or("HEAD"))
            }
            _ => return None,
        },
        Command::UpdateRef {
            name, delete: true, ..
        } => format!("deleted {name}"),
        Command::UpdateRef { name, .. } => format!("updated {name}"),
        Command::Gc { .. } => "packed the object database".to_owned(),
        Command::Clean { dry_run: false, .. } => "removed untracked files".to_owned(),
        Command::Rm { paths, .. } if !paths.is_empty() => {
            format!("removed {}", paths.join(" "))
        }
        Command::Mv { paths, .. } => match paths.split_last() {
            Some((to, from)) => format!("moved {} to {to}", from.join(" ")),
            None => return None,
        },
        _ => return None,
    })
}

/// Next steps after a change, where the follow-up is not obvious from the
/// result. Concrete names are used when the command supplied them.
fn next_steps(c: &Command) -> Vec<String> {
    match c {
        Command::Stage { .. } | Command::StageAll => {
            vec!["Run `rgit commit -m \"<message>\"` to commit staged changes".into()]
        }
        Command::Commit { .. } | Command::Extend | Command::Reword { .. } => {
            vec!["Run `rgit push` to publish the branch".into()]
        }
        Command::Uncommit { .. } => {
            vec!["Run `rgit commit -m \"<message>\"` to commit the staged changes again".into()]
        }
        Command::Undo => vec!["Run `rgit redo` to reverse the undo".into()],
        Command::Fetch { .. } => vec![
            "Run `rgit pull` to integrate the current branch's upstream".into(),
            "Run `rgit sync` to update and restack the whole stack".into(),
        ],
        Command::Push { delete: false, tags: false, dry_run: false, .. } => vec![
            "Run `rgit forge pr create --title \"<title>\" --head <branch> --base <branch>` to open a pull request".into(),
        ],
        Command::Branch {
            cmd: Some(BranchCmd::Create { name }),
            ..
        } => vec![format!(
            "Run `rgit push --set-upstream` to publish {name}"
        )],
        Command::Tag {
            name: Some(_),
            delete: None,
            ..
        } => vec!["Run `rgit push --tags` to publish tags".into()],
        Command::Stash {
            cmd: None | Some(StashCmd::Push { .. }),
        } => vec![
            "Run `rgit stash list` to see stashes".into(),
            "Run `rgit stash pop` to restore the newest one".into(),
        ],
        Command::Remote {
            cmd: Some(RemoteCmd::Add { name, .. }),
        } => vec![format!("Run `rgit fetch --remote {name}` to download its refs")],
        Command::Worktree {
            cmd: Some(WorktreeCmd::Add { .. }),
        } => vec!["Run `rgit worktree` to list worktrees".into()],
        Command::Stack {
            cmd: Some(StackCmd::New { .. }),
        } => vec![
            "Run `rgit commit -m \"<message>\"` to add work to the new branch".into(),
            "Run `rgit submit` to push the stack and open pull requests".into(),
        ],
        Command::Lanes {
            cmd: Some(LanesCmd::New { name } | LanesCmd::Stack { name, .. }),
        } => vec![format!(
            "Run `rgit lanes assign {name} <path>` to move a file into it"
        )],
        Command::Lanes {
            cmd: Some(LanesCmd::Commit { lane, .. }),
        } => vec![format!("Run `rgit lanes push {lane}` to publish the lane")],
        Command::Flow {
            cmd: FlowCmd::Start { .. },
        } => vec!["Run `rgit flow finish` when the feature is done".into()],
        Command::Flow {
            cmd: FlowCmd::Init { .. },
        } => vec!["Run `rgit flow start <name>` to begin a feature".into()],
        Command::Workspace {
            cmd: Some(WorkspaceCmd::New { .. }),
        } => vec!["Run `rgit workspace` to list workspaces and their paths".into()],
        _ => Vec::new(),
    }
}
