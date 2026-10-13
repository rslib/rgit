//! Structured results for the agent output modes. Each list or detail command
//! gets a small default schema, aggregate counts, a definitive empty state, and
//! next-step hints; every other command falls back to its text message.

use std::sync::Arc;

use rgit_git::{
    BlameLine, CommitDetails, FileDiff, GitBackend, LineOrigin, LogEntry, LogOptions, RefKind,
    RepoStatus, SmartlogEntry, StatusCode,
};

use crate::cli::{
    ApplyArgs, BranchCmd, BundleCmd, Command, CommitGraphCmd, ConfigArgs, DiffFormat, FlowCmd,
    HookCmd, IndexCmd, LanesCmd, MaintenanceCmd, MidxCmd, NotesCmd, RemoteCmd, StackCmd, StashCmd,
    SubmoduleCmd, WorkspaceCmd, WorktreeCmd,
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
    let color = crate::render::color_on();
    crate::cli::set_word_diff(backend, &command);
    crate::render::set_color(color);
    crate::cli::apply_diff_opts(backend, &command)?;
    let done = done_message(&command);
    let next = next_steps(&command);
    let restores = matches!(&command, Command::Checkout { pathspec, paths, .. }
        if !pathspec.is_empty() || !paths.is_empty());
    let finish = |text: String| {
        Output::from(if text == "ok" {
            done.clone().unwrap_or(text)
        } else {
            text
        })
    };
    Ok(match command {
        Command::Hook {
            cmd:
                HookCmd::Run {
                    ignore_missing,
                    to_stdin,
                    name,
                    args,
                },
        } => {
            let code = rgit_git::hook_run(
                &backend.git_dir(),
                &name,
                &args,
                to_stdin.as_deref().map(std::path::Path::new),
                ignore_missing,
            )?;
            anyhow::ensure!(code == 0, "the {name} hook exited with status {code}");
            Output::from(format!("ran the {name} hook"))
        }
        Command::Status {
            fmt,
            untracked,
            ignored,
            paths,
        } if fmt.any() => {
            let mut opts = fmt.opts(untracked.as_deref(), ignored.as_deref(), &paths);
            crate::cli::status_env(backend, &mut opts, true, false);
            let report = backend.status_text(&opts)?;
            Output::from(String::from_utf8_lossy(&report.text).into_owned())
        }
        Command::Status {
            untracked,
            ignored,
            paths,
            ..
        } => run_status(&crate::cli::status_view(
            backend,
            &paths,
            untracked.as_deref(),
            ignored.as_deref(),
        )?),
        Command::Log {
            format,
            ref pretty,
            ref line_ranges,
            walk_reflogs,
            ..
        } if (format.patch || format.stat)
            && !pretty.any()
            && !walk_reflogs
            && line_ranges.is_empty() =>
        {
            log_structured(backend, &command, format)?
        }
        Command::Log {
            format,
            ref pretty,
            ref line_ranges,
            ..
        } if !format.any() && !pretty.any() && line_ranges.is_empty() => {
            let opts: LogOptions = crate::cli::log_options(backend, &command)?;
            let filtered = opts.author.is_some()
                || opts.committer.is_some()
                || opts.occurrences.is_some()
                || opts.changes_matching.is_some()
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
                backend
                    .rev_walk(&LogOptions {
                        limit: usize::MAX,
                        offset: 0,
                        ..opts.clone()
                    })
                    .ok()
                    .map(|c| c.len())
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
        Command::Diff { format, .. } => {
            let (files, scope) = crate::cli::diff_files(backend, &command)?;
            if format.name_status || format.numstat || format.shortstat {
                diff_form(&files, format, &format!("0 {scope} changes"))
            } else {
                diff(&files, &scope, format)
            }
        }
        Command::Show {
            ref revs,
            ref paths,
            format,
            ref pretty,
            no_patch,
            ..
        } if !pretty.any() && !revs.iter().any(|r| r.contains(':')) => {
            show(backend, revs, paths, format, no_patch)?
        }
        Command::Blame {
            args,
            lines,
            format,
            opts,
        } if !(format.porcelain || format.line_porcelain) => {
            let path = args.join(" ");
            // Plain `START,END` ranges page through the whole blame.
            let paged = match &lines[..] {
                [] => Some((1, BLAME_LINES)),
                [spec] => crate::cli::parse_line_range(spec).ok(),
                _ => None,
            };
            let ranges = if paged.is_some() { &[][..] } else { &lines[..] };
            let all = crate::cli::blame(backend, &args, ranges, &opts).map_err(|e| match e {
                rgit_git::GitError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                    anyhow::Error::new(crate::cli::CliError {
                        message: format!("no file {path} in this repository"),
                        help: Some("Run `rgit ls-files` to list tracked files".to_owned()),
                        code: 1,
                    })
                }
                other => other.into(),
            })?;
            let (start, end) = paged.unwrap_or((1, usize::MAX));
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            let whens = blame_whens(&all.lines, start, end, |id| {
                backend
                    .read_object(id)
                    .map(|o| commit_when(&o.data, now))
                    .unwrap_or_default()
            });
            blame(&all.lines, &whens, &path, start, end)
        }
        Command::Plumbing(c) => crate::plumbing::run(backend, c, false)?,
        Command::Extra(c) => crate::extra::run(backend, c, false)?,
        command @ Command::LsRemote { .. } => {
            crate::plumbing::ls_remote(Some(backend.workdir()), command, false)?
        }
        Command::Submodule {
            cmd: cmd @ (None | Some(SubmoduleCmd::Status { quiet: false, .. })),
        } => {
            let (cached, recursive, paths) = match cmd {
                Some(SubmoduleCmd::Status {
                    cached,
                    recursive,
                    paths,
                    ..
                }) => (cached, recursive, paths),
                _ => (false, false, Vec::new()),
            };
            let list = crate::cli::chosen_submodules(backend, recursive, &paths)?;
            let rows = list
                .iter()
                .map(|s| {
                    let commit = match cached {
                        true => s.recorded.as_deref(),
                        false => s.checked_out.as_deref().or(s.recorded.as_deref()),
                    };
                    crate::obj! {
                        "path" => s.path,
                        "commit" => commit.unwrap_or(""),
                        "state" => s.state.to_string(),
                        "name" => s.name,
                        "url" => s.url.as_deref().unwrap_or(""),
                        "branch" => s.branch.as_deref().unwrap_or(""),
                        "describe" => s.describe.as_deref().unwrap_or(""),
                    }
                })
                .collect();
            Output::new(crate::cli::submodule_status(&list, cached))
                .list(
                    "submodules",
                    rows,
                    &["path", "commit", "state"],
                    "0 submodules",
                )
                .help("Run `rgit submodule update --init --recursive` to check them out")
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
        Command::Branch { cmd: None, opts } if opts.is_listing() && opts.format.is_none() => {
            let list = crate::cli::branch_rows(backend, &opts)?;
            let names: Vec<String> = list.iter().map(|r| r.name.clone()).collect();
            let current = list.iter().find(|r| r.current).map(|r| r.name.as_str());
            let rows = list
                .iter()
                .map(|r| {
                    let (upstream, ahead, behind) = match &r.upstream {
                        Some((u, a, b)) => (Some(u.as_str()), *a, *b),
                        None => (None, 0, 0),
                    };
                    crate::obj! {
                        "name" => r.name,
                        "current" => r.current,
                        "id" => r.id,
                        "summary" => r.summary,
                        "upstream" => upstream,
                        "ahead" => ahead,
                        "behind" => behind,
                    }
                })
                .collect();
            let columns: &[&str] = match opts.verbose {
                0 => &["name", "current"],
                1 => &["name", "current", "id", "summary"],
                _ => &[
                    "name", "current", "id", "upstream", "ahead", "behind", "summary",
                ],
            };
            Output::new(render::branches(&names, current))
                .list(
                    "branches",
                    rows,
                    columns,
                    if opts.remotes {
                        "0 remote-tracking branches"
                    } else {
                        "0 branches in this repository"
                    },
                )
                .help("Run `rgit checkout <branch>` to switch branches")
                .help("Run `rgit branch create <name>` to add a branch")
        }
        Command::Branch {
            cmd: Some(BranchCmd::Create { name, .. }),
            ..
        } if backend.local_branches()?.contains(&name) => {
            Output::message(format!("branch {name} already exists (no-op)"))
        }
        Command::Branch { cmd: None, opts }
            if !opts.is_listing()
                && !opts.force
                && opts.set_upstream_to.is_none()
                && ![
                    opts.show_current,
                    opts.delete,
                    opts.force_delete,
                    opts.rename,
                    opts.force_rename,
                    opts.copy,
                    opts.force_copy,
                    opts.unset_upstream,
                ]
                .contains(&true)
                && opts.args.len() == 1
                && backend.local_branches()?.contains(&opts.args[0]) =>
        {
            Output::message(format!("branch {} already exists (no-op)", opts.args[0]))
        }
        Command::Branch {
            cmd: Some(BranchCmd::Delete { names, .. }),
            ..
        } if !names.is_empty() && !any_branch_exists(backend, &names)? => Output::message(format!(
            "branch {} does not exist (no-op)",
            names.join(", ")
        )),
        Command::Branch { cmd: None, opts }
            if (opts.delete || opts.force_delete)
                && !opts.remotes
                && !opts.args.is_empty()
                && !any_branch_exists(backend, &opts.args)? =>
        {
            Output::message(format!(
                "branch {} does not exist (no-op)",
                opts.args.join(", ")
            ))
        }
        Command::Tag { names, opts } if opts.is_listing(&names) && opts.format.is_none() => {
            let tags = crate::cli::tag_list(backend, &names, &opts)?;
            let info: std::collections::HashMap<String, rgit_git::TagInfo> = backend
                .all_tags()?
                .into_iter()
                .map(|t| (t.name.clone(), t))
                .collect();
            let names: Vec<&str> = tags
                .iter()
                .map(|t| t.name.strip_prefix("refs/tags/").unwrap_or(&t.name))
                .collect();
            let text = if names.is_empty() {
                "no tags".to_owned()
            } else {
                names.join("\n")
            };
            let rows = names
                .iter()
                .map(|n| {
                    let t = info.get(*n);
                    crate::obj! {
                        "name" => n,
                        "when" => t.map(|t| t.when.as_str()).unwrap_or_default(),
                        "message" => t.map(|t| t.message.as_str()).unwrap_or_default(),
                    }
                })
                .collect();
            Output::new(text)
                .list(
                    "tags",
                    rows,
                    if opts.lines.is_some() {
                        &["name", "when", "message"]
                    } else {
                        &["name", "when"]
                    },
                    "0 tags in this repository",
                )
                .help("Run `rgit tag <name> -m \"<message>\"` to create a tag")
        }
        Command::Tag { names, opts }
            if !opts.force
                && !opts.delete
                && !opts.verify
                && names.len() == 1
                && tag_exists(backend, &names[0])? =>
        {
            Output::message(format!("tag {} already exists (no-op)", names[0]))
        }
        Command::Tag { names, opts }
            if opts.delete && !names.is_empty() && !any_tag_exists(backend, &names)? =>
        {
            Output::message(format!("tag {} does not exist (no-op)", names.join(", ")))
        }
        Command::Stash {
            cmd:
                Some(StashCmd::Show {
                    index,
                    patch,
                    name_only,
                    name_status,
                    numstat,
                    stat,
                    patch_with_stat,
                    include_untracked,
                    only_untracked,
                    ..
                }),
            ..
        } => {
            let i = index.unwrap_or(0);
            let bool_config = |k: &str, default: bool| {
                backend.config_get(k).ok().flatten().map_or(default, |v| {
                    matches!(v.to_lowercase().as_str(), "true" | "yes" | "on" | "1")
                })
            };
            let untracked = include_untracked
                || !only_untracked && bool_config("stash.showIncludeUntracked", false);
            let files = crate::cli::stash_diff(
                backend,
                i,
                (!only_untracked, untracked || only_untracked),
            )?;
            let mut format = DiffFormat {
                patch: patch || patch_with_stat,
                stat: stat || patch_with_stat,
                name_only,
                name_status,
                numstat,
                ..DiffFormat::default()
            };
            if !format.any() {
                format.stat = bool_config("stash.showStat", true);
                format.patch = bool_config("stash.showPatch", false);
            }
            if name_status || numstat {
                diff_form(&files, format, "0 files changed")
            } else {
                diff(&files, &format!("stash@{{{i}}}"), format)
            }
        }
        Command::Stash {
            cmd: Some(StashCmd::List {
                pretty, max_count, ..
            }),
            ..
        } if !pretty.any() && pretty.pretty.is_none() => {
            let mut stashes = backend.status()?.stashes;
            stashes.truncate(max_count.unwrap_or(usize::MAX));
            let rows = stashes
                .iter()
                .map(|s| crate::obj! { "index" => s.index, "message" => s.message })
                .collect();
            Output::new(render::stashes(&stashes))
                .list("stashes", rows, &["index", "message"], "0 stashes")
                .help("Run `rgit stash pop <index>` to restore a stash")
        }
        Command::Remote { cmd: None, verbose } => {
            let remotes = backend.remotes()?;
            let mut rows = Vec::new();
            for r in &remotes {
                let push = backend.remote_urls(&r.name, true)?.join(" ");
                rows.push(crate::obj! { "name" => r.name, "url" => r.url, "push" => push });
            }
            let columns: &[&str] = if verbose {
                &["name", "url", "push"]
            } else {
                &["name", "url"]
            };
            Output::new(render::remotes(&remotes))
                .list("remotes", rows, columns, "0 remotes configured")
                .help("Run `rgit remote add <name> <url>` to add a remote")
        }
        Command::Remote {
            cmd: Some(RemoteCmd::Add { name, url, .. }),
            ..
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
            ..
        } if !backend.remotes()?.iter().any(|r| r.name == name) => {
            Output::message(format!("remote {name} does not exist (no-op)"))
        }
        Command::Worktree {
            cmd:
                None
                | Some(WorktreeCmd::List {
                    porcelain: false, ..
                }),
        } => {
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
                &["name", "branch", "path", "head"],
                "0 worktrees",
            )
        }
        Command::Worktree {
            cmd: Some(WorktreeCmd::Remove { name, .. }),
        } if !backend
            .worktrees()?
            .iter()
            .any(|w| w.name == name || same_path(&w.path, &name)) =>
        {
            Output::message(format!("worktree {name} does not exist (no-op)"))
        }
        Command::Remote {
            cmd: Some(RemoteCmd::Rename { old, new }),
            ..
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
            force_branch: None,
            detach: false,
            ..
        }
        | Command::Switch {
            rev: Some(name),
            create: None,
            force_create: None,
            detach: false,
            ..
        } if !restores && backend.status()?.head.branch.as_deref() == Some(name.as_str()) => {
            Output::message(format!("already on {name} (no-op)"))
        }
        Command::Worktree {
            cmd: Some(WorktreeCmd::Add { path, .. }),
        } if backend
            .worktrees()?
            .iter()
            .any(|w| same_path(&w.path, &path)) =>
        {
            Output::message(format!("worktree {path} already exists (no-op)"))
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
        Command::Bisect { cmd } => match crate::cli::bisect(backend, cmd)? {
            (text, Some(left)) => {
                let rows = left
                    .iter()
                    .map(|e| crate::obj! { "id" => e.short_id, "summary" => e.summary })
                    .collect();
                Output::new(text).with("remaining", left.len()).list(
                    "commits",
                    rows,
                    &["id", "summary"],
                    "0 commits left to test",
                )
            }
            (text, None) => bisect_step(text),
        },
        Command::Stack {
            cmd: None | Some(StackCmd::List),
        } => stack(backend)?,
        c @ Command::Fsck { .. } => {
            let report = crate::cli::fsck(backend, c)?;
            fsck(report.stdout + &report.stderr)
        }
        Command::RequestPull {
            start,
            url,
            end,
            patch,
        } => {
            let (text, warnings) = backend.request_pull(&start, &url, end.as_deref(), patch)?;
            let mut out = Output::new(text.clone()).long("request", text);
            if !warnings.is_empty() {
                out = out.with("warnings", warnings);
            }
            out.help(format!(
                "Run `rgit push {url}` first if the branch is not there yet"
            ))
        }
        Command::Cherry {
            upstream,
            head,
            limit,
            ..
        } => {
            let upstream = upstream.unwrap_or_else(|| "@{upstream}".to_owned());
            let list = backend.cherry(
                &upstream,
                head.as_deref().unwrap_or("HEAD"),
                limit.as_deref(),
            )?;
            let missing = list.iter().filter(|c| !c.upstream_has_it).count();
            let rows = list
                .iter()
                .map(|c| {
                    crate::obj! {
                        "upstream" => if c.upstream_has_it { "has it" } else { "missing" },
                        "id" => &c.id[..c.id.len().min(12)],
                        "subject" => c.subject,
                    }
                })
                .collect();
            Output::new(String::new())
                .with("missing_upstream", missing)
                .list(
                    "commits",
                    rows,
                    &["upstream", "id", "subject"],
                    "0 commits: everything is upstream",
                )
                .help(format!(
                    "Run `rgit log {upstream}..HEAD` for the full history"
                ))
        }
        c @ (Command::Gc { .. } | Command::Repack { .. }) => {
            crate::cli::run(backend, c, interactive)?;
            let n = backend.count_objects()?;
            let msg = done.unwrap_or_default();
            Output::new(msg.clone())
                .with("result", msg)
                .with("loose_objects", n.count)
                .with("packs", n.packs)
                .with("pack_kib", n.size_pack as usize)
                .help("Run `rgit fsck` to check the object database")
        }
        other => {
            let mut out = finish(crate::cli::run(backend, other, interactive)?);
            out.help.extend(next);
            out
        }
    })
}

/// `git fsck`'s report as a table: `<kind> <type> <id> [(<name>)]` lines
/// (dangling, unreachable, missing, root, tagged), anything else kept whole.
fn fsck(text: String) -> Output {
    let mut rows = Vec::new();
    {
        for line in text.lines() {
            let mut words = line.splitn(3, ' ');
            let (kind, ty, rest) = (words.next(), words.next(), words.next());
            match (kind, ty, rest) {
                (Some(k), Some(t), Some(rest))
                    if matches!(
                        k,
                        "dangling" | "unreachable" | "missing" | "root" | "tagged"
                    ) =>
                {
                    let (id, name) = rest.split_once(' ').unwrap_or((rest, ""));
                    let name = name.trim_start_matches('(').trim_end_matches(')');
                    rows.push(crate::obj! { "kind" => k, "type" => t, "id" => id, "name" => name });
                }
                _ => rows.push(
                    crate::obj! { "kind" => "error", "type" => "", "id" => "", "name" => line },
                ),
            }
        }
    }
    Output::new(text.clone()).list(
        "problems",
        rows,
        &["kind", "type", "id", "name"],
        "no problems found",
    )
}

/// A bisect step from git's report: the commits left and roughly how many
/// steps, the commit to test now, or the first bad commit once found.
fn bisect_step(text: String) -> Output {
    let mut out = Output::new(text.clone());
    let mut testing = false;
    if !text.lines().any(|l| {
        l.starts_with("Bisecting: ") || l.contains(" is the first ") || l.starts_with("status: ")
    }) {
        return Output::message(text);
    }
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Bisecting: ") {
            let mut nums = rest
                .split(|c: char| !c.is_ascii_digit())
                .filter_map(|n| n.parse::<usize>().ok());
            out = out
                .with("remaining", nums.next().unwrap_or(0))
                .with("steps", nums.next().unwrap_or(0));
        } else if let Some((oid, subject)) = line.strip_prefix('[').and_then(|l| l.split_once("] "))
        {
            testing = true;
            out = out.with("current", format!("{} {subject}", &oid[..oid.len().min(7)]));
        } else if let Some((oid, _)) = line.split_once(" is the first ")
            && oid.len() == 40
        {
            out = out
                .with("first_bad", &oid[..7])
                .help("Run `rgit show <id>` for the commit's details")
                .help("Run `rgit bisect reset` to end the bisect");
        } else if let Some(status) = line.strip_prefix("status: ") {
            out = out.with("status", status);
        }
    }
    if testing {
        out = out
            .help("Run `rgit bisect good` if this commit does not have the change")
            .help("Run `rgit bisect bad` if it does")
            .help("Run `rgit bisect skip` if this commit cannot be tested");
    }
    out
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
    any_tag_exists(backend, &[name.to_owned()])
}

/// Whether two paths name the same existing folder.
fn same_path(a: &str, b: &str) -> bool {
    let canon = |p: &str| std::fs::canonicalize(p).ok();
    canon(a).is_some() && canon(a) == canon(b)
}

fn any_branch_exists(backend: &Arc<dyn GitBackend>, names: &[String]) -> anyhow::Result<bool> {
    Ok(backend.local_branches()?.iter().any(|b| names.contains(b)))
}

fn any_tag_exists(backend: &Arc<dyn GitBackend>, names: &[String]) -> anyhow::Result<bool> {
    Ok(backend.all_tags()?.iter().any(|t| names.contains(&t.name)))
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

/// One file of a diff: counts, its rename source and binary mark, and the
/// hunks when the patch is shown.
fn file_row(f: &FileDiff, hunks: bool) -> crate::toon::Obj {
    let (added, removed) = line_counts(f);
    let mut row = crate::obj! {
        "path" => f.path,
        "status" => f.status.letter(),
        "added" => added,
        "removed" => removed,
    };
    if let Some(old) = &f.old_path {
        row.push(("old_path".to_owned(), old.clone().into()));
    }
    if f.binary {
        row.push(("binary".to_owned(), true.into()));
    }
    if hunks {
        row.push((
            "hunks".to_owned(),
            crate::toon::Node::List(f.hunks.iter().map(hunk_row).collect()),
        ));
    }
    row
}

/// A hunk: its ranges, git's function context, and each diff line with its
/// `+`, `-`, space or `\` prefix kept.
fn hunk_row(h: &rgit_git::Hunk) -> crate::toon::Node {
    let body = h.header.strip_prefix("@@ ").unwrap_or(&h.header);
    let (ranges, context) = match body.find(" @@") {
        Some(i) => (&body[..i], body[i + 3..].trim()),
        None => (body, ""),
    };
    let mut row = crate::obj! { "at" => ranges };
    if !context.is_empty() {
        row.push(("context".to_owned(), context.to_owned().into()));
    }
    row.push((
        "lines".to_owned(),
        crate::toon::Node::List(
            h.lines
                .iter()
                .map(|l| {
                    let mark = match l.origin {
                        LineOrigin::Context => " ",
                        LineOrigin::Added => "+",
                        LineOrigin::Removed => "-",
                        LineOrigin::Meta => "",
                    };
                    crate::toon::Node::Str(format!("{mark}{}", l.text))
                })
                .collect(),
        ),
    ));
    crate::toon::Node::Obj(row)
}

fn file_rows(files: &[FileDiff], hunks: bool) -> (Vec<crate::toon::Obj>, String) {
    let (mut add, mut del) = (0, 0);
    let rows = files
        .iter()
        .map(|f| {
            let (a, d) = line_counts(f);
            add += a;
            del += d;
            file_row(f, hunks)
        })
        .collect();
    (rows, format!("{} files, +{add} -{del}", files.len()))
}

const FILE_COLS: &[&str] = &["path", "status", "old_path", "added", "removed", "binary", "hunks"];

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
    if let Some(r) = &s.rebase {
        out = out
            .with("onto", r.onto.as_str())
            .with(
                "step",
                format!("{}/{}", r.done.len(), r.done.len() + r.todo.len()),
            )
            .with("current", r.done.last().map(String::as_str).unwrap_or(""))
            .with("remaining", r.todo.len());
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

/// git's list forms of a diff as structured data: `--name-status` and
/// `--numstat` as a file table, `--shortstat` as the summary alone.
fn diff_form(files: &[FileDiff], format: DiffFormat, empty: &str) -> Output {
    let text = crate::cli::diff_out(files, format);
    if format.shortstat {
        let (_, total) = file_rows(files, false);
        return Output::new(text).with("total", total);
    }
    let rows: Vec<crate::toon::Obj> = files
        .iter()
        .map(|f| {
            let mut row = if format.numstat {
                let (added, removed) = line_counts(f);
                crate::obj! { "path" => f.path, "added" => added, "removed" => removed }
            } else {
                crate::obj! { "path" => f.path, "status" => f.status.letter() }
            };
            if let Some(old) = &f.old_path {
                row.push(("old_path".to_owned(), old.clone().into()));
            }
            if f.binary {
                row.push(("binary".to_owned(), true.into()));
            }
            row
        })
        .collect();
    let cols: &[&str] = if format.numstat {
        &["path", "added", "removed"]
    } else {
        &["path", "status"]
    };
    Output::new(text).list("files", rows, cols, empty)
}

fn diff(files: &[FileDiff], scope: &str, format: DiffFormat) -> Output {
    let empty = format!("0 {scope} changes");
    if format.name_only {
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        let out = Output::new(paths.join("\n"));
        return if paths.is_empty() {
            out.with("files", empty)
        } else {
            out.with("files", paths)
        };
    }
    let (rows, total) = file_rows(files, !format.stat || format.patch);
    let mut out = Output::new(render::diffstat(files)).list("files", rows, FILE_COLS, empty);
    if !files.is_empty() {
        out = out.with("total", total);
    }
    if !files.is_empty() && scope == "unstaged" {
        out = out.help("Run `rgit stage <path>` to stage a file");
    } else if scope == "unstaged" {
        out = out.help("Run `rgit diff --cached` to see staged changes");
    }
    out
}

/// One commit's header and diff as structured fields: the full id, the author
/// and committer as `Name <email>`, strict ISO dates, the message, and the
/// files for the format in use.
fn commit_fields(c: &CommitDetails, format: DiffFormat, no_patch: bool) -> crate::toon::Obj {
    let mut lines = c.message.trim_end().lines();
    let subject = lines.next().unwrap_or_default().to_owned();
    let body = lines
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    let author = format!("{} <{}>", c.author, c.email);
    let mut out = crate::obj! {
        "id" => c.full_id.clone(),
        "author" => author.clone(),
        "date" => iso_date(&c.author_date),
        "when" => c.when.clone(),
        "subject" => subject,
    };
    if !body.is_empty() {
        out.push(("body".to_owned(), body.into()));
    }
    let committer = format!("{} <{}>", c.committer, c.committer_email);
    if committer != author {
        out.push(("committer".to_owned(), committer.into()));
    }
    if c.parents.len() > 1 {
        out.push((
            "parents".to_owned(),
            crate::toon::Node::List(
                c.parents
                    .iter()
                    .map(|(id, _)| id.clone().into())
                    .collect(),
            ),
        ));
        if let Some((first, _)) = c.parents.first() {
            out.push(("diff".to_owned(), format!("first parent {first}").into()));
        }
    }
    if !c.refs.is_empty() {
        out.push((
            "refs".to_owned(),
            crate::toon::Node::List(
                c.refs.iter().map(|r| r.clone().into()).collect(),
            ),
        ));
    }
    if !no_patch {
        let (rows, total) = file_rows(&c.files, !format.stat || format.patch);
        out.push(("total".to_owned(), total.into()));
        if !format.shortstat {
            out.push((
                "files".to_owned(),
                crate::toon::Node::List(rows.into_iter().map(crate::toon::Node::Obj).collect()),
            ));
        }
    }
    out
}

/// One commit as `show` prints it: the header fields, with `body` cut unless
/// `--full` and the file rows registered for `--fields`.
fn commit_out(c: &CommitDetails, format: DiffFormat, no_patch: bool) -> Output {
    if !no_patch && (format.name_status || format.numstat) {
        return diff_form(&c.files, format, "0 files changed");
    }
    if format.name_only && !no_patch {
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        return Output::new(paths.join("\n")).with("files", paths);
    }
    let mut out = Output::new(render::commit_details(c));
    for (key, value) in commit_fields(c, format, no_patch) {
        match (key.as_str(), value) {
            ("body", crate::toon::Node::Str(body)) => out = out.long("body", body),
            ("files", crate::toon::Node::List(rows)) => {
                let objs = rows
                    .into_iter()
                    .map(|r| match r {
                        crate::toon::Node::Obj(o) => o,
                        _ => crate::toon::Obj::new(),
                    })
                    .collect();
                out = out.list("files", objs, FILE_COLS, "0 files changed");
            }
            (key, value) => out.data.push((key.to_owned(), value)),
        }
    }
    out
}

/// One commit of a `commits` list, with the file form `--name-only`,
/// `--name-status` or `--numstat` asks for.
fn commit_item(c: &CommitDetails, format: DiffFormat, no_patch: bool) -> crate::toon::Obj {
    let wants_form = format.name_only || format.name_status || format.numstat;
    if no_patch || !wants_form {
        return commit_fields(c, format, no_patch);
    }
    let mut item = commit_fields(c, format, true);
    let form = if format.name_only {
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        Output::new("").with("files", paths)
    } else {
        diff_form(&c.files, format, "0 files changed")
    };
    item.extend(form.data);
    item
}

/// What `show` prints: one commit, or a `commits` list for several.
fn show(
    backend: &Arc<dyn GitBackend>,
    revs: &[String],
    paths: &[String],
    format: DiffFormat,
    no_patch: bool,
) -> anyhow::Result<Output> {
    let trim = |mut c: CommitDetails| {
        if !paths.is_empty() {
            c.files
                .retain(|f| rgit_git::pathspec_matches(paths, &f.path));
        }
        if no_patch {
            c.files.clear();
        }
        c
    };
    if revs.len() > 1 && !revs.iter().any(|r| r.contains(':')) {
        let mut items = Vec::new();
        let mut text = String::new();
        for rev in revs {
            let c = trim(backend.commit_details(rev)?);
            items.push(commit_item(&c, format, no_patch));
            text.push_str(&render::commit_details(&c));
            text.push_str("\n\n");
        }
        text.pop();
        text.pop();
        return Ok(Output::new(text).list("commits", items, COMMIT_COLS, "0 commits"));
    }
    let rev = revs.first().map_or("HEAD", String::as_str);
    let c = trim(backend.commit_details(rev)?);
    let mut out = commit_out(&c, format, no_patch);
    if let Ok(obj) = backend.read_object(rev)
        && obj.kind == "tag"
        && let Some(tag) = tag_fields(&obj.data)
    {
        out.data
            .insert(0, ("tag".to_owned(), crate::toon::Node::Obj(tag)));
    }
    Ok(out)
}

const COMMIT_COLS: &[&str] = &[
    "id", "author", "date", "when", "subject", "body", "committer", "parents", "diff", "refs",
    "total", "files",
];

/// What `log -p` and `log --stat` print: a `commits` list, each item the show
/// header and its files, with hunks for `-p` and without for `--stat`.
fn log_structured(
    backend: &Arc<dyn GitBackend>,
    command: &Command,
    format: DiffFormat,
) -> anyhow::Result<Output> {
    let opts: LogOptions = crate::cli::log_options(backend, command)?;
    let entries = backend.log(&opts)?;
    let mut rows = Vec::new();
    for e in &entries {
        let mut c = backend.commit_details(&e.oid)?;
        if !opts.paths.is_empty() {
            c.files
                .retain(|f| rgit_git::pathspec_matches(&opts.paths, &f.path));
        }
        rows.push(commit_fields(&c, format, false));
    }
    Ok(
        Output::new(render::log(&entries)).list(
            "commits",
            rows,
            COMMIT_COLS,
            "0 commits on this branch",
        ),
    )
}

/// An annotated tag's own fields: its name, tagger, strict ISO date,
/// message and signature kind, from the tag object's text.
fn tag_fields(data: &[u8]) -> Option<crate::toon::Obj> {
    let text = String::from_utf8_lossy(data);
    let mut name = None;
    let mut target = None;
    let mut tagger = None;
    let mut lines = text.lines();
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("tag ") {
            name = Some(v.to_owned());
        } else if let Some(v) = line.strip_prefix("object ") {
            target = Some(v.to_owned());
        } else if let Some(v) = line.strip_prefix("tagger ") {
            tagger = Some(v.to_owned());
        }
    }
    let mut message = Vec::new();
    let mut signature = None;
    for line in lines {
        signature = match line {
            "-----BEGIN PGP SIGNATURE-----" => Some("gpg"),
            "-----BEGIN SSH SIGNATURE-----" => Some("ssh"),
            "-----BEGIN SIGNED MESSAGE-----" => Some("x509"),
            _ => None,
        };
        if signature.is_some() {
            break;
        }
        message.push(line);
    }
    let message = message.join("\n").trim().to_owned();
    let mut row = crate::obj! { "name" => name? };
    if let Some(target) = target {
        row.push(("object".to_owned(), target.into()));
    }
    if let Some(t) = tagger {
        let (who, epoch, zone) = parse_signature(&t);
        row.push(("tagger".to_owned(), who.into()));
        row.push(("date".to_owned(), epoch_iso(epoch, &zone).into()));
    }
    row.push(("message".to_owned(), message.into()));
    if let Some(kind) = signature {
        row.push(("signature".to_owned(), kind.into()));
    }
    Some(row)
}

/// A git signature line `Name <email> epoch zone` as who, epoch and zone.
fn parse_signature(line: &str) -> (String, i64, String) {
    let (who, rest) = match line.split_once(" <") {
        Some((who, rest)) => (who.to_owned(), rest),
        None => return (line.to_owned(), 0, "+0000".to_owned()),
    };
    let (email, rest) = match rest.split_once("> ") {
        Some((email, rest)) => (email.to_owned(), rest),
        None => return (who, 0, "+0000".to_owned()),
    };
    let mut words = rest.split_whitespace();
    let epoch = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
    let zone = words.next().unwrap_or("+0000").to_owned();
    (format!("{who} <{email}>"), epoch, zone)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// git's default date form (`Thu Sep 24 23:24:06 2026 -0500`) as strict
/// ISO 8601.
fn iso_date(d: &str) -> String {
    let w: Vec<&str> = d.split_whitespace().collect();
    let [_, month, day, time, year, zone] = w.as_slice() else {
        return d.to_owned();
    };
    let m = MONTHS
        .iter()
        .position(|m| *m == *month)
        .map_or(0, |m| m + 1);
    format!(
        "{year}-{m:02}-{day:0>2}T{time}{}",
        zone_colon(zone)
    )
}

/// Unix time and a `+/-HHMM` zone as strict ISO 8601.
fn epoch_iso(epoch: i64, zone: &str) -> String {
    let offset = zone_offset(zone);
    let local = epoch + i64::from(offset) * 60;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    format!(
        "{y:04}-{month:02}-{day:02}T{:<02}:{:<02}:{:<02}{}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
        zone_colon(zone)
    )
}

/// A `+/-HHMM` zone as `+/-HH:MM`.
fn zone_colon(zone: &str) -> String {
    let bytes = zone.as_bytes();
    if bytes.len() == 5 && (bytes[0] == b'+' || bytes[0] == b'-') {
        format!("{}{}:{}", zone[..1].to_owned(), &zone[1..3], &zone[3..5])
    } else {
        zone.to_owned()
    }
}

/// A `+/-HHMM` zone's offset in minutes.
fn zone_offset(zone: &str) -> i32 {
    let sign = match zone.as_bytes().first() {
        Some(b'-') => -1,
        _ => 1,
    };
    let digits: String = zone.chars().filter(|c| c.is_ascii_digit()).collect();
    digits
        .parse::<i32>()
        .map(|n| sign * (n / 100 * 60 + n % 100))
        .unwrap_or(0)
}

/// The `when` ages for the blame rows on screen: only the shown rows'
/// commits are read, so paging a big file costs one page of object reads.
fn blame_whens(
    all: &[BlameLine],
    start: usize,
    end: usize,
    mut read: impl FnMut(&str) -> String,
) -> std::collections::HashMap<String, String> {
    let lo = start.saturating_sub(1).min(all.len());
    let hi = end.min(all.len()).max(lo);
    let mut whens = std::collections::HashMap::new();
    for line in &all[lo..hi] {
        if line.id.is_empty() {
            continue;
        }
        whens
            .entry(line.id.clone())
            .or_insert_with(|| read(&line.id));
    }
    whens
}

fn blame(
    all: &[BlameLine],
    whens: &std::collections::HashMap<String, String>,
    path: &str,
    start: usize,
    end: usize,
) -> Output {
    let lo = start.saturating_sub(1).min(all.len());
    let hi = end.min(all.len()).max(lo);
    let shown = &all[lo..hi];
    let rows = shown
        .iter()
        .map(|b| {
            crate::obj! {
                "line" => b.final_line,
                "id" => b.short_id,
                "author" => b.author,
                "when" => whens.get(&b.id).map(String::as_str).unwrap_or(""),
                "text" => b.line,
            }
        })
        .collect();
    let mut out = Output::new(render::blame(shown, true));
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
        &["line", "id", "author", "when", "text"],
        format!("0 lines in {path} for that range"),
    )
}

/// A commit's relative age from its raw object text: the author line's unix
/// time, as `2h` or `3d`.
fn commit_when(data: &[u8], now: i64) -> String {
    let text = String::from_utf8_lossy(data);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("author ") {
            let (_, epoch, _) = parse_signature(rest);
            return match (now - epoch).max(0) {
                d if d < 60 => format!("{d}s"),
                d if d < 3600 => format!("{}m", d / 60),
                d if d < 86_400 => format!("{}h", d / 3600),
                d => format!("{}d", d / 86_400),
            };
        }
    }
    String::new()
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
        Command::Add { paths, update, .. } if paths.is_empty() => {
            if *update {
                "staged changes to tracked files".to_owned()
            } else {
                "staged all changes".to_owned()
            }
        }
        Command::Add { paths, .. } => format!("staged {}", paths.join(" ")),
        Command::Restore {
            paths,
            staged: true,
            worktree: false,
            ..
        } => format!("unstaged {}", paths.join(" ")),
        Command::Restore { paths, .. } => format!("restored {}", paths.join(" ")),
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
        }
        | Command::Checkout {
            force_branch: Some(new),
            ..
        }
        | Command::Switch {
            create: Some(new), ..
        }
        | Command::Switch {
            force_create: Some(new),
            ..
        } => format!("created and checked out {new}"),
        Command::Checkout {
            detach: true, rev, ..
        }
        | Command::Switch {
            detach: true, rev, ..
        } => format!("detached HEAD at {}", rev.as_deref().unwrap_or("HEAD")),
        Command::Checkout { rev: Some(rev), .. } | Command::Switch { rev: Some(rev), .. } => {
            format!("checked out {rev}")
        }
        Command::Merge { abort: true, .. } => "merge aborted".to_owned(),
        Command::Merge { cont: true, .. } => "merge committed".to_owned(),
        Command::Merge { quit: true, .. } => {
            "merge forgotten; index and working tree kept".to_owned()
        }
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
        Command::Rebase { quit: true, .. } => "rebase stopped; HEAD left where it is".to_owned(),
        Command::Rebase {
            edit_todo: true, ..
        } => "rebase todo list edited".to_owned(),
        Command::Rebase {
            onto: Some(onto), ..
        } => format!("rebased onto {onto}"),
        Command::Rebase { .. } => "rebased".to_owned(),
        Command::Reset {
            rev: Some(rev),
            soft,
            hard,
            keep,
            merge,
            ..
        } => {
            let mode = match (soft, hard, keep, merge) {
                (true, ..) => "soft",
                (_, true, ..) => "hard",
                (_, _, true, _) => "keep",
                (.., true) => "merge",
                _ => "mixed",
            };
            format!("reset to {rev} ({mode})")
        }
        Command::CherryPick { abort: true, .. } => "cherry-pick aborted".to_owned(),
        Command::CherryPick { cont: true, .. } => "cherry-pick continued".to_owned(),
        Command::CherryPick { skip: true, .. } | Command::Revert { skip: true, .. } => {
            "skipped the current commit".to_owned()
        }
        Command::CherryPick { quit: true, .. } | Command::Revert { quit: true, .. } => {
            "sequence forgotten; HEAD, index and working tree kept".to_owned()
        }
        Command::CherryPick { revs, .. } if !revs.is_empty() => {
            format!("cherry-picked {}", revs.join(" "))
        }
        Command::Revert { abort: true, .. } => "revert aborted".to_owned(),
        Command::Revert { cont: true, .. } => "revert continued".to_owned(),
        Command::Revert { revs, .. } if !revs.is_empty() => format!("reverted {}", revs.join(" ")),
        Command::Branch { cmd: Some(cmd), .. } => match cmd {
            BranchCmd::Create { name, .. } => format!("created and checked out branch {name}"),
            BranchCmd::Checkout { name } => format!("checked out {name}"),
            BranchCmd::Rename { old, new } => format!("renamed branch {old} to {new}"),
            _ => return None,
        },
        Command::Stash { cmd: Some(cmd), .. } => match cmd {
            StashCmd::Pop { index, .. } => stash("popped", index),
            StashCmd::Apply { index, .. } => stash("applied", index),
            StashCmd::Drop { index, .. } => stash("dropped", index),
            StashCmd::Branch { name, index } => {
                format!("{} onto new branch {name}", stash("popped", index))
            }
            StashCmd::Clear => "dropped every stash".to_owned(),
            StashCmd::Store { commit, .. } => format!("stored {commit} as stash@{{0}}"),
            _ => return None,
        },
        Command::Tag { names, opts } if opts.delete => {
            format!("deleted tag {}", names.join(", "))
        }
        Command::Tag { names, .. } if !names.is_empty() => format!("created tag {}", names[0]),
        Command::Remote { cmd: Some(cmd), .. } => match cmd {
            RemoteCmd::Add { name, url, .. } => format!("added remote {name} -> {url}"),
            RemoteCmd::Remove { name } => format!("removed remote {name}"),
            RemoteCmd::SetUrl { name, url, .. } => format!("set remote {name} -> {url}"),
            RemoteCmd::Rename { old, new } => format!("renamed remote {old} to {new}"),
            _ => return None,
        },
        Command::Worktree { cmd: Some(cmd) } => match cmd {
            WorktreeCmd::Add { path, .. } => format!("added worktree {path}"),
            WorktreeCmd::Remove { name, .. } => format!("removed worktree {name}"),
            WorktreeCmd::Lock { name, .. } => format!("locked worktree {name}"),
            WorktreeCmd::Unlock { name } => format!("unlocked worktree {name}"),
            WorktreeCmd::Move { name, new_path } => format!("moved worktree {name} to {new_path}"),
            _ => return None,
        },
        Command::Fetch { .. } => "fetched".to_owned(),
        Command::Pull { .. } => "pulled".to_owned(),
        Command::Push { .. } => "pushed".to_owned(),
        Command::Config(ConfigArgs {
            key: Some(key),
            unset,
            unset_all,
            ..
        }) if *unset || *unset_all => format!("unset {key}"),
        Command::Config(ConfigArgs {
            key: Some(key),
            rename_section: true,
            value: Some(value),
            ..
        }) => format!("renamed section {key} to {value}"),
        Command::Config(ConfigArgs {
            key: Some(key),
            remove_section: true,
            ..
        }) => format!("removed section {key}"),
        Command::Config(ConfigArgs {
            key: Some(key),
            value: Some(value),
            get: false,
            get_all: false,
            get_regexp: false,
            ..
        }) => format!("set {key} = {value}"),
        Command::Apply(ApplyArgs { check: true, .. }) => "the patch applies cleanly".to_owned(),
        Command::Apply(ApplyArgs {
            stat: false,
            numstat: false,
            summary: false,
            ..
        }) => "applied the patch".to_owned(),
        Command::Notes { cmd: Some(cmd), .. } => match cmd {
            NotesCmd::Add { rev, .. } | NotesCmd::Append { rev, .. } => {
                format!("noted {}", rev.as_deref().unwrap_or("HEAD"))
            }
            NotesCmd::Remove { revs, .. } if revs.is_empty() => {
                "removed the note of HEAD".to_owned()
            }
            NotesCmd::Remove { revs, .. } => format!("removed the notes of {}", revs.join(" ")),
            NotesCmd::Copy { from, to, .. } => {
                format!(
                    "copied the note of {from} to {}",
                    to.as_deref().unwrap_or("HEAD")
                )
            }
            NotesCmd::Edit { rev, .. } => format!("noted {}", rev.as_deref().unwrap_or("HEAD")),
            _ => return None,
        },
        Command::UpdateRef {
            name: Some(name),
            delete: true,
            ..
        } => format!("deleted {name}"),
        Command::UpdateRef {
            name: Some(name), ..
        } => format!("updated {name}"),
        Command::Gc { .. } => "packed the object database".to_owned(),
        Command::Repack { .. } => "repacked the object database".to_owned(),
        Command::Bundle {
            cmd: BundleCmd::Create { file, .. },
        } => format!("wrote {file}"),
        Command::PackRefs { .. } => "packed the refs".to_owned(),
        Command::CommitGraph {
            cmd: CommitGraphCmd::Write { .. },
        } => "wrote the commit-graph".to_owned(),
        Command::MultiPackIndex { cmd } => match cmd {
            MidxCmd::Write { .. } => "wrote the multi-pack-index".to_owned(),
            MidxCmd::Expire { .. } => "expired unused packs".to_owned(),
            MidxCmd::Repack { .. } => "repacked a batch of packs".to_owned(),
            _ => return None,
        },
        Command::Maintenance { cmd } => match cmd {
            MaintenanceCmd::Run { .. } => "ran the maintenance tasks".to_owned(),
            MaintenanceCmd::Start { .. } => "registered and scheduled maintenance".to_owned(),
            MaintenanceCmd::Stop => "stopped scheduled maintenance".to_owned(),
            MaintenanceCmd::Register => "registered the repository for maintenance".to_owned(),
            MaintenanceCmd::Unregister { .. } => "unregistered the repository".to_owned(),
        },
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
        Command::Stage { .. } | Command::StageAll | Command::Add { .. } => {
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
            cmd: Some(BranchCmd::Create { name, .. }),
            ..
        } => vec![format!(
            "Run `rgit push --set-upstream` to publish {name}"
        )],
        Command::Tag { names, opts } if !opts.delete && !opts.is_listing(names) => {
            vec!["Run `rgit push --tags` to publish tags".into()]
        }
        Command::Stash {
            cmd: None | Some(StashCmd::Push { .. }),
            ..
        } => vec![
            "Run `rgit stash list` to see stashes".into(),
            "Run `rgit stash pop` to restore the newest one".into(),
        ],
        Command::Remote {
            cmd: Some(RemoteCmd::Add { name, .. }),
            ..
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

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str) -> BlameLine {
        BlameLine {
            short_id: String::new(),
            author: String::new(),
            line: String::new(),
            id: id.to_owned(),
            email: String::new(),
            orig_line: 0,
            orig_path: String::new(),
            boundary: false,
            final_line: 0,
            ignored: false,
            unblamable: false,
            previous: None,
        }
    }

    #[test]
    fn blame_whens_reads_only_shown_rows() {
        let all = vec![line("a"), line(""), line("b"), line("b"), line("c")];
        let mut read = Vec::new();
        let whens = blame_whens(&all, 3, 4, |id| {
            read.push(id.to_owned());
            id.to_owned()
        });
        assert_eq!(read, vec!["b".to_owned()]);
        assert_eq!(whens.len(), 1);
        assert_eq!(whens["b"], "b");
    }
}
