use std::io::IsTerminal;
use std::process::exit;
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches};
use rgit_git::{Git2Backend, GitBackend};
use serde_json::Value;

use crate::cli::{Cli, CliError, Command, HooksCmd, OutputMode};
use crate::output::Output;
use crate::toon::{Node, Obj};

mod add_interactive;
mod add_patch;
mod axi;
mod clean;
mod cli;
mod credential;
mod creds;
mod date;
mod diffcolor;
mod diffopts;
mod examples;
mod extra;
mod forge;
mod globals;
mod graph;
mod interactive;
mod lanes;
mod logging;
mod maintenance;
mod mcp;
mod output;
mod plumbing;
mod pretty;
mod prompt;
mod render;
mod scalar;
mod setup;
mod stack;
mod toon;

/// How results are printed for this invocation.
struct Emit {
    mode: OutputMode,
    fields: Vec<String>,
    full: bool,
    rerun: String,
    command: Option<String>,
}

fn main() -> ! {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag] = args.as_slice()
        && flag == "-V"
    {
        println!("{}", env!("CARGO_PKG_VERSION"));
        exit(0);
    }
    let (args, mut paginate) = globals::apply(args);
    let args = whatchanged(help_and_version(globals::dispatch(args, &mut paginate)));
    logging::init();
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let parsed = examples::apply(Cli::command())
        .try_get_matches_from(
            std::iter::once("rgit".to_owned()).chain(sticky_sign(diff_scores(blame_scores(
                diff_shorthand(count_shorthand(&plumbing::grep_tokens(&args))),
            )))),
        )
        .and_then(|m| Cli::from_arg_matches(&m));
    let cli = match parsed {
        Ok(cli) => cli,
        Err(e) => usage_exit(e, &args),
    };
    let output_mode = cli.output_mode();
    let structured_output = output_mode != OutputMode::Text;
    let mut cli = cli;
    if !structured_output && cli.command.is_none() && !stdout_is_terminal {
        // No terminal for the TUI: a human gets the status instead.
        cli.command = Some(Command::Status {
            fmt: Default::default(),
            untracked: None,
            ignored: None,
            paths: Vec::new(),
        });
    }
    if !structured_output && !cli.compact {
        cli.command = cli.command.map(|c| cli::git_defaults(c, compact_config));
    }
    if !structured_output
        && !matches!(
            cli.command,
            None | Some(Command::Mcp | Command::Serve { .. })
        )
    {
        rgit_git::stream_hooks();
    }
    let can_prompt = interactive::enabled(cli.no_input || structured_output);
    if can_prompt {
        // The in-process ssh transport falls back to a password prompt when key
        // auth fails and no ControlMaster socket exists to reuse.
        rgit_git::set_password_provider(creds::ssh_password);
    } else {
        // SAFETY: still single-threaded; keeps spawned git from prompting.
        unsafe { std::env::set_var("GIT_TERMINAL_PROMPT", "0") };
    }
    let emit = Emit {
        mode: output_mode,
        fields: cli.fields.clone(),
        full: cli.full,
        rerun: rerun(&args),
        command: globals::command_name(&args),
    };

    match cli.command {
        // The MCP server takes over stdio for the process lifetime.
        Some(Command::Mcp) => exit(mcp::serve(discover_or_exit())),
        Some(Command::Skills { cmd }) => finish(cli::run_skills(cmd), &emit),
        Some(Command::ForEachRepo {
            config,
            keep_going,
            args,
        }) => finish(
            maintenance::for_each_repo(&config, keep_going, &args, !structured_output),
            &emit,
        ),
        Some(
            cmd @ (Command::Credential { .. }
            | Command::CredentialStore { .. }
            | Command::CredentialCache { .. }
            | Command::CredentialCacheDaemon { .. }),
        ) => exit(credential::run(cmd)),
        Some(Command::Scalar { cmd }) => finish(scalar::run(cmd), &emit),
        Some(Command::Hooks { cmd }) => finish(
            match cmd {
                HooksCmd::Install { user, app } => setup::install(app, user),
                HooksCmd::Status => setup::status(),
            },
            &emit,
        ),
        // Repo creation runs before discovery (there is no repo yet); both use
        // libgit2 directly rather than shelling out.
        Some(Command::Init {
            path,
            initial_branch,
            bare,
            template,
            shared,
            separate_git_dir,
            object_format,
            quiet,
        }) => {
            if object_format.as_deref().is_some_and(|f| f != "sha1") {
                die(
                    CliError::usage("only --object-format=sha1 is supported"),
                    &emit,
                );
            }
            let path = path.unwrap_or_else(|| ".".to_owned());
            let args = rgit_git::InitArgs {
                initial_branch,
                bare,
                template,
                shared,
                separate_git_dir,
            };
            finish(
                rgit_git::init(std::path::Path::new(&path), &args)
                    .map(|msg| if quiet { String::new() } else { msg })
                    .map_err(anyhow::Error::from),
                &emit,
            );
        }
        Some(Command::Clone {
            url,
            dir,
            branch,
            depth,
            bare,
            origin,
            recurse_submodules,
            single_branch,
            no_single_branch,
            no_checkout,
            mirror,
            no_tags,
            reference,
            reference_if_able,
            bundle_uri: _,
            dissociate,
            shared,
            filter,
            sparse,
            template,
            shallow_since,
            separate_git_dir,
            config,
            quiet,
            verbose: _,
            jobs: _,
        }) => {
            // Default the target directory to the repo name, as git does.
            let dir = dir.unwrap_or_else(|| {
                url.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("repo")
                    .trim_end_matches(".git")
                    .to_owned()
                    + if bare || mirror { ".git" } else { "" }
            });
            let args = rgit_git::CloneArgs {
                branch,
                depth,
                bare,
                origin,
                recurse_submodules,
                single_branch,
                no_checkout,
                mirror,
                no_tags,
                config,
                no_single_branch,
                reference,
                reference_if_able,
                dissociate,
                shared,
                filter,
                sparse,
                template,
                shallow_since,
                separate_git_dir,
            };
            let checkout = !(no_checkout || bare || mirror) && args.filter.is_none();
            let result = rgit_git::clone(&url, std::path::Path::new(&dir), &args, &|p| {
                if let rgit_git::OpProgress::Line(l) = p
                    && (l.starts_with("warning:") || l.starts_with("info:"))
                {
                    eprintln!("{l}");
                }
            });
            if result.is_ok()
                && checkout
                && let Ok(repo) = rgit_git::Git2Backend::discover(&dir)
            {
                let repo: std::sync::Arc<dyn rgit_git::GitBackend> = std::sync::Arc::new(repo);
                let zero = "0".repeat(40);
                let head = repo.rev_parse("HEAD").unwrap_or_else(|_| zero.clone());
                let _ =
                    cli::post_hook(&repo, repo.workdir(), "post-checkout", &[&zero, &head, "1"]);
            }
            finish(
                result
                    .map(|()| {
                        if quiet {
                            String::new()
                        } else {
                            format!("cloned into {dir}")
                        }
                    })
                    .map_err(anyhow::Error::from),
                &emit,
            );
        }
        // The web viewer takes over the process, like the TUI, so it runs on
        // its own runtime rather than through the one-shot dispatch below.
        Some(Command::Serve {
            host,
            port,
            root,
            clone_base,
        }) => {
            let addr = match format!("{host}:{port}").parse::<std::net::SocketAddr>() {
                Ok(a) => a,
                Err(e) => die(CliError::usage(format!("bad --host/--port ({e})")), &emit),
            };
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("rgit: {e}");
                    exit(1);
                }
            };
            // The /agent page renders this catalog; register it before serving.
            rgit_web::set_mcp_tools(mcp::tool_catalog());
            let served = match root {
                Some(dir) => {
                    let root_path = std::path::PathBuf::from(&dir);
                    // The HTTP MCP endpoint resolves `repo` by name under the root.
                    let mcp = mcp::http_router_rooted(root_path.clone());
                    runtime.block_on(rgit_web::serve_root(root_path, addr, clone_base, Some(mcp)))
                }
                None => {
                    let backend = discover_or_exit();
                    let repo = rgit_web::repo_name(backend.as_ref());
                    let mcp = mcp::http_router(repo.clone(), backend.clone());
                    runtime.block_on(rgit_web::serve(backend, repo, addr, clone_base, Some(mcp)))
                }
            };
            match served {
                Ok(()) => exit(0),
                Err(e) => die(anyhow::anyhow!("{e}"), &emit),
            }
        }
        // The `git` escape hatch is a transparent passthrough: inherit stdin,
        // stdout, and stderr so stdin-reading subcommands (commit-tree,
        // hash-object --stdin, apply) and interactive ones behave exactly like
        // git, and exit with git's own status.
        Some(Command::Git { args }) => {
            let backend = match discover_or_init(false) {
                Ok(backend) => backend,
                Err(error) => die(error, &emit),
            };
            let mut git = std::process::Command::new("git");
            git.args(&args).current_dir(backend.workdir());
            if emit.mode == OutputMode::Text {
                match git.status() {
                    Ok(s) => exit(s.code().unwrap_or(if s.success() { 0 } else { 1 })),
                    Err(e) => die(anyhow::anyhow!("could not run git: {e}"), &emit),
                }
            }
            match git.output() {
                Ok(out) if out.status.success() => finish(
                    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_owned()),
                    &emit,
                ),
                Ok(out) => die(
                    rgit_git::GitError::Cli(String::from_utf8_lossy(&out.stderr).into_owned())
                        .into(),
                    &emit,
                ),
                Err(e) => die(anyhow::anyhow!("could not run git: {e}"), &emit),
            }
        }
        // git's own status formats are for scripts: print them raw in every mode.
        Some(command @ Command::Status { .. }) if matches!(&command, Command::Status { fmt, .. } if fmt.any()) =>
        {
            let backend = match discover_or_init(false) {
                Ok(backend) => backend,
                Err(error) => die(error, &emit),
            };
            let Command::Status {
                fmt,
                untracked,
                ignored,
                paths,
            } = cli::from_cwd(command, &backend)
            else {
                unreachable!()
            };
            let mut opts = fmt.opts(untracked.as_deref(), ignored.as_deref(), &paths);
            if emit.mode == OutputMode::Text {
                globals::start_pager(Some("status"), paginate);
            }
            cli::status_env(&backend, &mut opts, cli.no_color, globals::color_tty());
            match backend.status_text(&opts) {
                Ok(report) => {
                    use std::io::Write;
                    let mut out = std::io::stdout().lock();
                    exit(i32::from(
                        out.write_all(&report.text)
                            .and_then(|()| out.flush())
                            .is_err(),
                    ));
                }
                Err(error) => die(
                    cli::CliError {
                        message: error.to_string(),
                        help: None,
                        code: 128,
                    }
                    .into(),
                    &emit,
                ),
            }
        }
        // An archive with no output file is raw bytes on stdout, as in git.
        Some(Command::Archive(a)) if a.output.is_none() && !a.list => {
            let backend = match discover_or_init(false) {
                Ok(backend) => Some(backend),
                Err(_) if a.remote.is_some() => None,
                Err(error) => die(error, &emit),
            };
            let a = match &backend {
                Some(b) => match cli::from_cwd(Command::Archive(a), b) {
                    Command::Archive(a) => a,
                    _ => unreachable!(),
                },
                None => a,
            };
            match cli::archive(backend.as_ref(), &a) {
                Ok(bytes) => {
                    use std::io::Write;
                    let mut out = std::io::stdout().lock();
                    exit(i32::from(
                        out.write_all(&bytes).and_then(|()| out.flush()).is_err(),
                    ));
                }
                Err(error) => die(error, &emit),
            }
        }
        // Every other subcommand runs one operation and prints compact output.
        Some(Command::Forge {
            profile,
            account,
            host,
            cmd,
        }) => {
            let mut base = String::from("rgit forge");
            for (flag, value) in [
                ("--profile", &profile),
                ("--account", &account),
                ("--host", &host),
            ] {
                if let Some(v) = value {
                    base.push_str(&format!(" {flag} {v}"));
                }
            }
            let (hints, list_cmd) = forge_hints(&cmd, &base);
            finish(
                forge::run(
                    cmd,
                    forge::ForgeContext {
                        profile,
                        provider: None,
                        account,
                        host,
                    },
                )
                .map(|text| {
                    let mut out = forge_output(text, list_cmd.as_deref());
                    out.help.extend(hints);
                    out
                }),
                &emit,
            )
        }
        Some(command) => {
            if output_mode == OutputMode::Text {
                globals::start_pager(globals::command_name(&args).as_deref(), paginate);
            }
            // Color and prompts only on a real terminal.
            render::init_color(
                output_mode == OutputMode::Text,
                if cli.no_color {
                    Some("never")
                } else {
                    cli.color.as_deref()
                },
            );
            let repoless = command.runs_without_repo();
            let backend = match discover_or_init(can_prompt && !repoless) {
                Ok(backend) => backend,
                Err(_) if repoless => {
                    finish(cli::run_without_repo(command, !structured_output), &emit)
                }
                Err(error) => die(error, &emit),
            };
            let command = cli::from_cwd(command, &backend);
            let since = backend.index_second();
            let watch = rgit_git::watch_hooks(&backend.git_dir());
            let widened = command
                .moves_worktree()
                .then(|| rgit_git::sparse_widen(&backend.git_dir()).ok().flatten())
                .flatten();
            let result = if structured_output {
                axi::run(&backend, command, can_prompt)
            } else {
                cli::run(&backend, command, can_prompt).map(Output::from)
            };
            if let Some(w) = widened {
                let _ = rgit_git::sparse_narrow(&backend.git_dir(), w);
            }
            let _ = backend.smudge_racy(since);
            let result = match watch.map(|w| w.finish(|| index_flags(&args))) {
                Some(Err(e)) if result.is_ok() => Err(ref_hook_error(e)),
                _ => result,
            };
            finish(result, &emit)
        }
        None if structured_output => finish(Ok(home()), &emit),
        // No subcommand: launch the TUI.
        None => {
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "rgit starting");
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("rgit: {e}");
                    exit(1);
                }
            };
            // Exit rather than drop the runtime: crossterm's EventStream parks a
            // blocking stdin read that a graceful shutdown would wait on forever.
            exit(runtime.block_on(run_tui(cli.no_preview, can_prompt)));
        }
    }
}

async fn run_tui(no_preview: bool, can_prompt: bool) -> i32 {
    match rgit_tui::run(discover_or_init_or_exit(can_prompt), no_preview).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("rgit: {e}");
            1
        }
    }
}

fn finish(result: anyhow::Result<impl Into<Output>>, emit: &Emit) -> ! {
    let output = match result {
        Ok(output) => output.into(),
        Err(error) => die(error, emit),
    };
    if emit.mode == OutputMode::Text {
        // git prints nothing for most successful changes.
        if output.text == "ok" {
            exit(cli::exit_code());
        }
        // Plumbing text ends with its own newline, or is empty, as in git.
        if output.text.is_empty() || output.text.ends_with(['\n', '\0']) || cli::text_as_is() {
            print!("{}", output.text);
            let _ = std::io::Write::flush(&mut std::io::stdout());
        } else {
            println!("{}", output.text);
        }
        exit(cli::exit_code());
    }
    let mut value = match output.finalize(&emit.fields, emit.full, &emit.rerun) {
        Ok(value) => value,
        Err(error) => die(error, emit),
    };
    if let Some((_, Node::List(hints))) = value.iter_mut().find(|(k, _)| k == "help") {
        for hint in hints {
            if let Node::Str(h) = hint {
                *h = keep_mode(h, emit.mode);
            }
        }
    }
    if emit.mode == OutputMode::Json {
        value.push(("ok".to_owned(), Node::Bool(true)));
        println!("{}", json(value));
    } else {
        println!("{}", toon::encode(&value));
    }
    exit(cli::exit_code());
}

fn die(error: anyhow::Error, emit: &Emit) -> ! {
    if emit.mode == OutputMode::Text {
        let (text, code) = output::human(&error, emit.command.as_deref());
        if !text.is_empty() {
            eprintln!("{text}");
        }
        exit(code);
    }
    let (message, help, code) = output::translate(&error);
    fail(message, help, code, emit.mode)
}

/// Report an error on stdout in the output format, or on stderr for humans.
fn fail(message: String, help: Vec<String>, code: i32, mode: OutputMode) -> ! {
    let help: Vec<String> = help.iter().map(|h| keep_mode(h, mode)).collect();
    match mode {
        OutputMode::Text => eprintln!("error: {message}"),
        OutputMode::Json => {
            println!(
                "{}",
                json(obj! { "ok" => false, "error" => message, "help" => help })
            );
        }
        OutputMode::Porcelain => {
            let mut value = obj! { "error" => sanitize(message) };
            if !help.is_empty() {
                value.push(("help".to_owned(), help.into()));
            }
            println!("{}", toon::encode(&value));
        }
    }
    exit(code);
}

/// A hint's `rgit ...` commands with the output flag in use, so an agent that
/// follows them keeps structured output.
fn keep_mode(hint: &str, mode: OutputMode) -> String {
    let flag = match mode {
        OutputMode::Text => return hint.to_owned(),
        OutputMode::Porcelain => "--toon",
        OutputMode::Json => "--json",
    };
    let mut out = String::new();
    let mut rest = hint;
    while let Some(i) = rest.find("`rgit") {
        let tail = &rest[i + 5..];
        let command = tail.split('`').next().unwrap_or_default();
        let has_mode = command
            .split(' ')
            .any(|w| matches!(w, "--toon" | "--axi" | "--json"));
        out.push_str(&rest[..i + 5]);
        // `valid flags for `rgit diff`` names a command rather than running it.
        if (tail.starts_with(' ') || tail.starts_with('`'))
            && !has_mode
            && !rest[..i].ends_with("for ")
        {
            out.push(' ');
            out.push_str(flag);
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// `whatchanged` is git's `log` with raw diffs and no merges (unless asked
/// for another diff format, or for merges' diffs).
fn whatchanged(mut args: Vec<String>) -> Vec<String> {
    let Some(at) = args.iter().position(|a| a == "whatchanged") else {
        return args;
    };
    let rest = &args[at + 1..];
    let end = rest.iter().position(|a| a == "--").unwrap_or(rest.len());
    let given = |flags: &[&str]| {
        rest[..end].iter().any(|a| {
            flags
                .iter()
                .any(|f| a == f || a.starts_with(&format!("{f}=")))
        })
    };
    let mut extra = Vec::new();
    if !given(&["-m", "-c", "--cc", "--diff-merges", "--dd"]) {
        extra.push("--no-merges".to_owned());
    }
    let formats = [
        "-p",
        "-u",
        "--patch",
        "--stat",
        "--numstat",
        "--shortstat",
        "--name-only",
        "--name-status",
        "--summary",
        "-s",
        "--no-patch",
        "--raw",
        "--patch-with-stat",
        "--patch-with-raw",
        "--dirstat",
        "--compact-summary",
    ];
    if !given(&formats) {
        extra.push("--raw".to_owned());
    }
    if !given(&["--oneline", "--format", "--pretty"]) {
        extra.push("--pretty=medium".to_owned());
    }
    args[at] = "log".to_owned();
    args.splice(at + 1..at + 1, extra);
    args
}

/// A refused ref update, as git reports it.
pub(crate) fn ref_hook_error(e: rgit_git::GitError) -> anyhow::Error {
    CliError {
        message: e.to_string(),
        help: None,
        code: 128,
    }
    .into()
}

/// post-index-change's flags for the command in `args`: whether it updated
/// the working tree, and whether it may have changed skip-worktree bits, as
/// git sets them.
pub(crate) fn index_flags(args: &[String]) -> (bool, bool) {
    let has = |flags: &[&str]| args.iter().any(|a| flags.contains(&a.as_str()));
    match globals::command_name(args).as_deref() {
        Some("checkout") => (!has(&["--"]), false),
        Some("reset") if has(&["--hard", "--merge", "--keep"]) => (true, false),
        Some("reset") => (false, !has(&["--soft"])),
        Some(
            "switch" | "merge" | "pull" | "rebase" | "cherry-pick" | "revert" | "stash" | "am"
            | "sparse-checkout",
        ) => (true, false),
        Some("read-tree") => (has(&["-u"]), false),
        _ => (false, false),
    }
}

/// git's `-<n>` count for `log`, `rev-list` and `stash list`: `rgit log -3` is
/// `rgit log -n 3`.
fn count_shorthand(args: &[String]) -> Vec<String> {
    let log = args
        .iter()
        .position(|a| a == "log" || a == "rev-list")
        .or_else(|| {
            args.windows(2)
                .position(|w| w[0] == "stash" && w[1] == "list")
                .map(|i| i + 1)
        });
    let Some(log) = log else {
        return args.to_vec();
    };
    let mut out = args[..=log].to_vec();
    let mut rest = args[log + 1..].iter();
    for a in rest.by_ref() {
        if a == "--" {
            out.push(a.clone());
            break;
        }
        match a.strip_prefix('-') {
            Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                out.extend(["-n".to_owned(), n.to_owned()]);
            }
            _ => out.push(a.clone()),
        }
    }
    out.extend(rest.cloned());
    out
}

/// git's sticky diff options: `-B[n][/m]`, `-X[param]`, `-l<n>` and
/// `--stat=<width>[,<name-width>[,<count>]]` in `diff`, `log` and `show`.
fn diff_shorthand(args: Vec<String>) -> Vec<String> {
    let Some(at) = args
        .iter()
        .position(|a| matches!(a.as_str(), "diff" | "log" | "show" | "whatchanged"))
    else {
        return args;
    };
    let log = matches!(args[at].as_str(), "log" | "whatchanged");
    let mut out = args[..=at].to_vec();
    let mut rest = args[at + 1..].iter();
    for a in rest.by_ref() {
        if a == "--" {
            out.push(a.clone());
            break;
        }
        if let Some(v) = a.strip_prefix("-B") {
            out.push(if v.is_empty() {
                "--break-rewrites".to_owned()
            } else {
                format!("--break-rewrites={v}")
            });
        } else if let Some(v) = a.strip_prefix("-X") {
            out.push(format!("--dirstat={v}"));
        } else if let Some(n) = a
            .strip_prefix("-l")
            .filter(|n| !log && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            out.push(format!("--rename-limit={n}"));
        } else if let Some(v) = a.strip_prefix("--stat=") {
            out.push("--stat".to_owned());
            let mut parts = v.split(',');
            for flag in ["--stat-width", "--stat-name-width", "--stat-count"] {
                if let Some(n) = parts.next().filter(|n| !n.is_empty()) {
                    out.push(format!("{flag}={n}"));
                }
            }
        } else {
            out.push(a.clone());
        }
    }
    out.extend(rest.cloned());
    // --binary asks for a patch too.
    let flags = &out[at + 1..out.iter().position(|a| a == "--").unwrap_or(out.len())];
    if flags.iter().any(|a| a == "--binary") && !flags.iter().any(|a| a == "-p" || a == "--patch") {
        out.insert(at + 1, "--patch".to_owned());
    }
    out
}

/// git's sticky `-S[<keyid>]`: `rgit cherry-pick -S topic` signs and picks
/// topic, so `-S` never takes the next word.
fn sticky_sign(mut args: Vec<String>) -> Vec<String> {
    let signs = ["commit", "merge", "cherry-pick", "revert", "am", "rebase"];
    let Some(at) = args.iter().position(|a| signs.contains(&a.as_str())) else {
        return args;
    };
    for a in &mut args[at + 1..] {
        if a == "--" {
            break;
        }
        if let Some(key) = a.strip_prefix("-S") {
            *a = format!("--gpg-sign={}", key.strip_prefix('=').unwrap_or(key));
        }
    }
    args
}

/// git's attached scores for `blame`: `-M30` is `-M --move-score=30`, and
/// `-C50` is `-C --copy-score=50`.
fn blame_scores(args: Vec<String>) -> Vec<String> {
    let Some(at) = args.iter().position(|a| a == "blame" || a == "annotate") else {
        return args;
    };
    let mut out = args[..=at].to_vec();
    let mut rest = args[at + 1..].iter();
    for a in rest.by_ref() {
        if a == "--" {
            out.push(a.clone());
            break;
        }
        let score = |flag: &str| {
            a.strip_prefix(flag)
                .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        };
        match (score("-M"), score("-C")) {
            (Some(n), _) => out.extend(["-M".to_owned(), format!("--move-score={n}")]),
            (_, Some(n)) => out.extend(["-C".to_owned(), format!("--copy-score={n}")]),
            _ => out.push(a.clone()),
        }
    }
    out.extend(rest.cloned());
    out
}

/// git's sticky `-M[<n>]` and `-C[<n>]` for the diff plumbing: `-M50%` is
/// `--find-renames=50%`, so a bare `-M` never takes the next word.
fn diff_scores(mut args: Vec<String>) -> Vec<String> {
    let diffs = ["diff-tree", "diff-index", "diff-files"];
    let Some(at) = args.iter().position(|a| diffs.contains(&a.as_str())) else {
        return args;
    };
    for a in &mut args[at + 1..] {
        if a == "--" {
            break;
        }
        let long = match a.get(..2) {
            Some("-M") => "--find-renames",
            Some("-C") => "--find-copies",
            _ => continue,
        };
        *a = match &a[2..] {
            "" => long.to_owned(),
            n => format!("{long}={n}"),
        };
    }
    args
}

/// The current invocation as a copy-pasteable command, for `--full` hints.
fn rerun(args: &[String]) -> String {
    let mut out = String::from("rgit");
    for a in args {
        out.push(' ');
        if a.is_empty() || a.contains(|c: char| c.is_whitespace() || "\"'`$".contains(c)) {
            out.push_str(&format!("'{}'", a.replace('\'', "'\\''")));
        } else {
            out.push_str(a);
        }
    }
    out
}

fn objects(items: Vec<Value>) -> Vec<Obj> {
    items
        .into_iter()
        .filter_map(|v| match Node::from(v) {
            Node::Obj(fields) => Some(fields),
            _ => None,
        })
        .collect()
}

/// ` <target> <repo>` as given, so hints address the same repository.
fn repo_args(target: &Option<String>, repo: &Option<String>) -> String {
    [target, repo]
        .into_iter()
        .flatten()
        .map(|v| format!(" {v}"))
        .collect()
}

/// Next steps after a forge command, keeping the profile/account/host flags
/// and the repository arguments. Also returns the list command a `--page`
/// hint should extend.
fn forge_hints(cmd: &cli::ForgeCmd, base: &str) -> (Vec<String>, Option<String>) {
    use cli::{ForgeBranchCmd, ForgeCmd, PrCmd, RepoCmd};
    match cmd {
        ForgeCmd::Login { provider, .. } => (
            vec![format!(
                "Run `{base} whoami {provider}` to check the account"
            )],
            None,
        ),
        ForgeCmd::Logout { .. } => (
            vec![format!("Run `{base} auth list` to see remaining accounts")],
            None,
        ),
        ForgeCmd::Auth { .. } => (
            vec![format!("Run `{base} login <provider>` to add an account")],
            None,
        ),
        ForgeCmd::Whoami { .. } => (Vec::new(), None),
        ForgeCmd::Repo { cmd } => match cmd {
            RepoCmd::View { target, repo } => {
                let at = repo_args(target, repo);
                (
                    vec![
                        format!("Run `{base} pr list{at}` for open pull requests"),
                        format!("Run `{base} branch list{at}` for its branches"),
                    ],
                    None,
                )
            }
            RepoCmd::Create { .. } => (
                vec![
                    "Run `rgit remote add origin <clone_url>` to point this repo at it".to_owned(),
                    "Run `rgit push --set-upstream` to publish the current branch".to_owned(),
                ],
                None,
            ),
            RepoCmd::Delete { .. } => (Vec::new(), None),
        },
        ForgeCmd::Branch { cmd } => match cmd {
            ForgeBranchCmd::List { target, repo, .. } => {
                let at = repo_args(target, repo);
                (
                    vec![format!(
                        "Run `{base} branch delete <branch>{at} --yes` to delete one"
                    )],
                    Some(format!("{base} branch list{at}")),
                )
            }
            ForgeBranchCmd::Delete { target, repo, .. } => (
                vec![format!(
                    "Run `{base} branch list{}` to see the remaining branches",
                    repo_args(target, repo)
                )],
                None,
            ),
        },
        ForgeCmd::Pr { cmd } => match cmd {
            PrCmd::List { target, repo, .. } => {
                let at = repo_args(target, repo);
                (
                    vec![
                        format!(
                            "Run `{base} pr create{at} --title \"<title>\" --head <branch> --base <branch>` to open one"
                        ),
                        format!("Run `{base} pr close <number>{at} --yes` to close one"),
                    ],
                    Some(format!("{base} pr list{at}")),
                )
            }
            PrCmd::Create { target, repo, .. } => {
                let at = repo_args(target, repo);
                (
                    vec![
                        format!("Run `{base} pr list{at}` to see open pull requests"),
                        format!("Run `{base} pr close <number>{at} --yes` to close it"),
                    ],
                    None,
                )
            }
            PrCmd::Close { target, repo, .. } => (
                vec![format!(
                    "Run `{base} pr list{}` to see the remaining pull requests",
                    repo_args(target, repo)
                )],
                None,
            ),
        },
    }
}

/// Forge results arrive as JSON; tables get a small default schema. A paged
/// listing (`items`, `page`, `more`, `total`) gets a count and, when more
/// pages follow, a hint that re-runs `list_cmd` with the next `--page`.
fn forge_output(text: String, list_cmd: Option<&str>) -> Output {
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(mut map)) if map.get("items").is_some_and(Value::is_array) => {
            let Some(Value::Array(items)) = map.remove("items") else {
                unreachable!("checked above")
            };
            let page = map.get("page").and_then(Value::as_u64).unwrap_or(1);
            let more = map.get("more").and_then(Value::as_bool).unwrap_or(false);
            let total = map.get("total").and_then(Value::as_u64);
            let rows = objects(items);
            let is_pr = list_cmd.is_some_and(|c| c.contains(" pr list"));
            let (key, defaults, noun): (&str, &'static [&'static str], &str) = if is_pr {
                (
                    "pull_requests",
                    &["number", "title", "state", "head_branch"],
                    "open pull requests",
                )
            } else {
                ("branches", &["name", "sha", "protected"], "remote branches")
            };
            let shown = rows.len() as u64;
            let first = (page - 1) * u64::from(rgit_forge::PAGE_SIZE) + 1;
            let mut out = Output::new(text);
            match total {
                Some(total) if total > shown && shown > 0 => {
                    out = out.with(
                        "count",
                        format!("{first}-{} of {total} total", first + shown - 1),
                    );
                }
                None if more => {
                    out = out.with("count", format!("{shown} shown on page {page}; more exist"));
                }
                _ => {}
            }
            let empty = if page > 1 {
                format!("0 {noun} on page {page}")
            } else {
                format!("0 {noun}")
            };
            out = out.list(key, rows, defaults, empty);
            if more && let Some(cmd) = list_cmd {
                out = out.help(format!("Run `{cmd} --page {}` for the next page", page + 1));
            }
            out
        }
        Ok(Value::Object(mut map)) => {
            map.retain(|_, v| !v.is_null());
            match map.remove("accounts") {
                Some(Value::Array(items)) => {
                    let rows = objects(items);
                    Output::from_json(text, map).list(
                        "accounts",
                        rows,
                        &["provider", "account", "authenticated", "source"],
                        "0 forge accounts",
                    )
                }
                Some(other) => {
                    map.insert("accounts".to_owned(), other);
                    Output::from_json(text, map)
                }
                None => Output::from_json(text, map),
            }
        }
        _ => Output::message(text),
    }
}

/// Report a clap parse error. Humans get clap's own message; agents get the
/// error, clap's tips, and the valid flags or subcommands at the failing level.
fn usage_exit(e: clap::Error, args: &[String]) -> ! {
    use clap::error::ErrorKind;
    let has = |flags: &[&str]| args.iter().any(|a| flags.contains(&a.as_str()));
    let mode = if has(&["--json"]) {
        OutputMode::Json
    } else if has(&["--toon", "--axi"]) {
        OutputMode::Porcelain
    } else {
        OutputMode::Text
    };
    let (cmd, path) = subcommand_at(args);
    let before_dashes = args.iter().take_while(|a| *a != "--");
    if e.kind() == ErrorKind::DisplayHelp && before_dashes.clone().any(|a| a == "-h")
        || e.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            && mode == OutputMode::Text
    {
        print_raw(&short_usage(&cmd, &path));
        exit(129);
    }
    if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        e.exit();
    }
    if mode == OutputMode::Text {
        let (text, code) = git_usage_error(&e, args, &cmd, &path);
        eprint!("{text}");
        exit(code);
    }
    if e.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        e.exit();
    }

    let rendered = e.render().to_string();
    let mut rendered_lines = rendered.lines();
    let message = rendered_lines
        .next()
        .unwrap_or_default()
        .trim_start_matches("error: ")
        .to_owned();
    let mut help: Vec<String> = rendered_lines
        .filter_map(|l| l.trim().strip_prefix("tip: "))
        .filter(|tip| !tip.starts_with("to pass "))
        .map(str::to_owned)
        .collect();

    let offending = e.context().find_map(|(kind, value)| match (kind, value) {
        (
            clap::error::ContextKind::InvalidArg | clap::error::ContextKind::InvalidSubcommand,
            clap::error::ContextValue::String(s),
        ) => Some(s.split_whitespace().next().unwrap_or(s).to_owned()),
        _ => None,
    });

    if let Some(arg) = &offending
        && let Some(hint) = renamed(&path, arg)
    {
        help.insert(0, hint.to_owned());
    }
    let wants_subcommand = matches!(
        e.kind(),
        ErrorKind::InvalidSubcommand | ErrorKind::MissingSubcommand
    );
    if wants_subcommand && cmd.has_subcommands() {
        let subs: Vec<&str> = cmd
            .get_subcommands()
            .filter(|c| !c.is_hide_set())
            .map(|c| c.get_name())
            .collect();
        help.push(format!(
            "valid subcommands for `{path}`: {}",
            subs.join(", ")
        ));
    } else {
        let flags: Vec<String> = cmd
            .get_arguments()
            .filter(|a| !a.is_hide_set() && !a.is_global_set())
            .filter_map(|a| a.get_long())
            .filter(|l| !matches!(*l, "help" | "version"))
            .map(|l| format!("--{l}"))
            .collect();
        if !flags.is_empty() {
            help.push(format!("valid flags for `{path}`: {}", flags.join(", ")));
        }
    }
    fail(message, help, 2, mode)
}

/// Write `text` to stdout, ignoring a closed pipe (`rgit help | head`).
fn print_raw(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

/// The deepest subcommand `args` name, and its path (`rgit stash push`).
fn subcommand_at(args: &[String]) -> (clap::Command, String) {
    let mut cmd = Cli::command();
    let mut path = String::from("rgit");
    for arg in args.iter().filter(|a| !a.starts_with('-')) {
        match cmd.find_subcommand(arg) {
            Some(sub) => {
                path.push(' ');
                path.push_str(sub.get_name());
                cmd = sub.clone();
            }
            None => break,
        }
    }
    (cmd, path)
}

/// git's `-h` text: the usage lines, then one line per option with its help
/// in a column at 26, and a blank line.
fn short_usage(cmd: &clap::Command, path: &str) -> String {
    let usage = cmd.clone().bin_name(path).render_usage().to_string();
    // clap's `[PATHS]...` in git's spelling, `[<paths>...]`.
    let optional = regex::Regex::new(r"\[([A-Z][A-Z0-9_]*)\](\.\.\.)?").expect("valid regex");
    let usage = optional.replace_all(&usage, |c: &regex::Captures| {
        format!(
            "[<{}>{}]",
            c[1].to_lowercase(),
            c.get(2).map_or("", |m| m.as_str())
        )
    });
    let required = regex::Regex::new(r"<([A-Z][A-Z0-9_]*)>").expect("valid regex");
    let usage = required.replace_all(&usage, |c: &regex::Captures| {
        format!("<{}>", c[1].to_lowercase())
    });
    let mut out = String::new();
    for (i, line) in usage.lines().enumerate() {
        let line = line.trim_start_matches("Usage: ").trim_start();
        let lead = if i == 0 { "usage: " } else { "   or: " };
        out.push_str(&format!("{lead}{line}\n"));
    }
    out.push('\n');
    let mut any = false;
    for a in cmd.get_arguments() {
        if a.is_hide_set() || a.is_global_set() || a.is_positional() || a.get_id() == "help" {
            continue;
        }
        let mut left = String::from("    ");
        if let Some(s) = a.get_short() {
            left.push_str(&format!("-{s}"));
            if a.get_long().is_some() {
                left.push_str(", ");
            }
        }
        if let Some(l) = a.get_long() {
            left.push_str(&format!("--{l}"));
        }
        if a.get_action().takes_values() {
            let value = a
                .get_value_names()
                .and_then(|v| v.first())
                .map_or("value".to_owned(), |v| v.to_lowercase());
            if a.get_num_args().is_some_and(|n| n.min_values() == 0) {
                left.push_str(&format!("[=<{value}>]"));
            } else {
                left.push_str(&format!(" <{value}>"));
            }
        }
        let about = a.get_help().map(|h| h.to_string()).unwrap_or_default();
        let about = about.lines().next().unwrap_or("");
        let about = about.split(". ").next().unwrap_or("").trim_end_matches('.');
        let mut chars = about.chars();
        let about = match chars.next() {
            Some(c) => c.to_lowercase().chain(chars).collect(),
            None => String::new(),
        };
        if left.len() < 26 {
            out.push_str(&format!("{left:<26}{about}\n"));
        } else {
            out.push_str(&format!("{left}\n{:26}{about}\n", ""));
        }
        any = true;
    }
    if any {
        out.push('\n');
    }
    out
}

/// A clap parse error as git reports a bad command line: the stderr text and
/// git's exit code (129 with the usage, 128 for a fatal, 1 for an unknown
/// command).
fn git_usage_error(
    e: &clap::Error,
    args: &[String],
    cmd: &clap::Command,
    path: &str,
) -> (String, i32) {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    let context = |want: ContextKind| {
        e.context().find_map(|(k, v)| match v {
            ContextValue::String(s) if k == want => Some(s.clone()),
            _ => None,
        })
    };
    let arg = context(ContextKind::InvalidArg)
        .map(|s| s.split_whitespace().next().unwrap_or(&s).to_owned())
        .unwrap_or_default();
    let usage = || short_usage(cmd, path);
    let first = e.render().to_string();
    let first = first
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("error: ")
        .to_owned();
    let top = path == "rgit";
    let sub = path.rsplit(' ').next().unwrap_or_default();
    match e.kind() {
        ErrorKind::InvalidSubcommand if top => {
            let name = context(ContextKind::InvalidSubcommand).unwrap_or_default();
            let mut text = format!("rgit: '{name}' is not a git command. See 'rgit --help'.\n");
            let similar: Vec<String> = e
                .context()
                .filter(|(k, _)| *k == ContextKind::SuggestedSubcommand)
                .flat_map(|(_, v)| match v {
                    ContextValue::String(s) => vec![s.clone()],
                    ContextValue::Strings(s) => s.clone(),
                    _ => Vec::new(),
                })
                .collect();
            if !similar.is_empty() {
                text.push_str(if similar.len() == 1 {
                    "\nThe most similar command is\n"
                } else {
                    "\nThe most similar commands are\n"
                });
                for s in similar {
                    text.push_str(&format!("\t{s}\n"));
                }
            }
            (text, 1)
        }
        ErrorKind::InvalidSubcommand => {
            let name = context(ContextKind::InvalidSubcommand).unwrap_or_default();
            (
                format!("error: unknown subcommand: `{name}'\n{}", usage()),
                129,
            )
        }
        ErrorKind::UnknownArgument if top && arg.starts_with('-') => {
            (format!("unknown option: {arg}\n{}", usage()), 129)
        }
        ErrorKind::UnknownArgument if arg.starts_with('-') => match sub {
            "log" | "show" | "whatchanged" => {
                (format!("fatal: unrecognized argument: {arg}\n"), 128)
            }
            "diff" => (format!("error: invalid option: {arg}\n{}", usage()), 129),
            "rev-list" => (usage(), 129),
            _ => {
                let (kind, name) = match arg.strip_prefix("--") {
                    Some(long) => ("option", long.split('=').next().unwrap_or(long)),
                    None => ("switch", arg.get(1..2).unwrap_or_default()),
                };
                (format!("error: unknown {kind} `{name}'\n{}", usage()), 129)
            }
        },
        ErrorKind::InvalidValue
            if context(ContextKind::InvalidValue).is_some_and(|v| v.is_empty()) =>
        {
            let typed = args
                .iter()
                .take_while(|a| *a != "--")
                .filter(|a| a.starts_with('-'))
                .last()
                .map_or(arg.as_str(), String::as_str);
            let text = match typed.strip_prefix("--") {
                Some(long) => format!("error: option `{long}' requires a value\n"),
                None => format!(
                    "error: switch `{}' requires a value\n",
                    typed.chars().last().unwrap_or('?')
                ),
            };
            (text, 129)
        }
        ErrorKind::ArgumentConflict => {
            let other = context(ContextKind::PriorArg)
                .map(|s| s.split_whitespace().next().unwrap_or(&s).to_owned())
                .unwrap_or_default();
            (
                format!("fatal: options '{arg}' and '{other}' cannot be used together\n"),
                128,
            )
        }
        _ => (format!("error: {first}\n{}", usage()), 129),
    }
}

/// `rgit help [-a | -g | <command>]` and `rgit version`, which git runs as
/// builtins: a command's help becomes `<command> --help` for clap to print.
fn help_and_version(mut args: Vec<String>) -> Vec<String> {
    let Some(at) = globals::command_index(&args) else {
        return args;
    };
    let owned = args[at + 1..].to_vec();
    let rest: Vec<&str> = owned.iter().map(String::as_str).collect();
    match args[at].as_str() {
        "version" => match rest.as_slice() {
            [] | ["--build-options"] => {
                println!("rgit version {}", env!("CARGO_PKG_VERSION"));
                if !rest.is_empty() {
                    println!("cpu: {}", std::env::consts::ARCH);
                    println!("no commit associated with this build");
                    println!("sizeof-long: {}", std::mem::size_of::<std::ffi::c_long>());
                    println!("sizeof-size_t: {}", std::mem::size_of::<usize>());
                    println!("shell-path: /bin/sh");
                }
                exit(0)
            }
            _ => {
                print!(
                    "usage: rgit version [--[no-]build-options]\n\n    --[no-]build-options  also print build options\n\n"
                );
                exit(129)
            }
        },
        "help" => {
            let names: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|a| !matches!(*a, "-m" | "--man" | "-w" | "--web" | "-i" | "--info"))
                .collect();
            match names.as_slice() {
                [] | ["-a" | "--all", ..] => {
                    let cli = Cli::command();
                    let subs: Vec<&clap::Command> =
                        cli.get_subcommands().filter(|c| !c.is_hide_set()).collect();
                    let width = subs.iter().map(|c| c.get_name().len()).max().unwrap_or(0);
                    let mut out = "See 'rgit help <command>' to read about a specific \
                                   subcommand\n\nAvailable rgit commands\n"
                        .to_owned();
                    for c in subs {
                        let about = c.get_about().map(|a| a.to_string()).unwrap_or_default();
                        let about = about.split(". ").next().unwrap_or("").trim_end_matches('.');
                        out.push_str(&format!("   {:width$}   {about}\n", c.get_name()));
                    }
                    print_raw(&out);
                    exit(0)
                }
                ["-g" | "--guides"] => {
                    println!(
                        "rgit ships no concept guides; `git help -g` lists git's, which describe \
                         rgit too."
                    );
                    exit(0)
                }
                [name] if !name.starts_with('-') => {
                    if Cli::command().find_subcommand(name).is_none() {
                        eprintln!("rgit: '{name}' is not a git command. See 'rgit --help'.");
                        exit(1)
                    }
                    args.truncate(at);
                    args.push((*name).to_owned());
                    args.push("--help".to_owned());
                    args
                }
                _ => {
                    print!(
                        "usage: rgit help [-a|--all]\n   or: rgit help [[-i|--info] [-m|--man] \
                         [-w|--web]] [<command>]\n   or: rgit help [-g|--guides]\n\n"
                    );
                    exit(129)
                }
            }
        }
        _ => args,
    }
}

/// A targeted hint for git spellings that rgit names differently.
fn renamed(path: &str, arg: &str) -> Option<&'static str> {
    cli::GIT_SPELLINGS
        .iter()
        .find(|(p, args, _)| *p == path && args.contains(&arg))
        .map(|(_, _, hint)| *hint)
}

/// The home view loads into every agent session, so its file list stays short.
const HOME_FILES: usize = 10;
/// How many op-log entries the home view shows, newest first.
const HOME_OPS: usize = 3;

/// The no-argument view for agents: who rgit is, then the repo's live state.
/// `rgit.compact` from the repo's config, when there is a repo.
fn compact_config() -> bool {
    discover_or_init(false)
        .ok()
        .and_then(|b| b.config_get("rgit.compact").ok().flatten())
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1"))
}

/// The home view is what session hooks show an agent, so it says how to keep
/// structured output.
const OUTPUT_HINT: &str =
    "Pass `--toon` (or `--json`) to every rgit command; without it rgit prints git's human text";

fn home() -> Output {
    let bin = std::env::current_exe()
        .map(|p| tilde(&p))
        .unwrap_or_default();
    let mut data = obj! { "bin" => bin, "description" => cli::DESCRIPTION };
    match discover_or_init(false).and_then(|b| Ok(b.status()?)) {
        Ok(status) => {
            let mut out = axi::run_status(&status);
            let mut help = if out.help.is_empty() {
                cli::HOME_HELP.iter().map(|h| h.to_string()).collect()
            } else {
                out.help.clone()
            };
            if let Some(Node::List(files)) = out.get_mut("files")
                && files.len() > HOME_FILES
            {
                let total = files.len();
                files.truncate(HOME_FILES);
                out.set("count", format!("{HOME_FILES} of {total} changed files"));
                help.insert(0, format!("Run `rgit status` to see all {total} files"));
            }
            data.append(&mut out.data);
            if let Ok(ops) = discover_or_init(false).and_then(|b| Ok(b.oplog()?))
                && !ops.is_empty()
            {
                let recent: Vec<Node> = ops
                    .iter()
                    .take(HOME_OPS)
                    .map(|o| {
                        Node::Obj(obj! { "id" => o.short_id, "label" => o.label, "when" => o.when })
                    })
                    .collect();
                data.push(("recent".to_owned(), Node::List(recent)));
            }
            out.data = data;
            help.push(OUTPUT_HINT.to_owned());
            out.help = help;
            out
        }
        Err(error) => {
            let (message, help, _) = output::translate(&error);
            data.push(("status".to_owned(), sanitize(message).into()));
            let mut out = Output::new(String::new());
            out.data = data;
            out.help = help;
            out
        }
    }
}

fn tilde(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
            format!("~{}", &shown[home.len()..])
        }
        _ => shown,
    }
}

fn json(value: Obj) -> String {
    serde_json::to_string(&Node::Obj(value)).unwrap_or_default()
}

use output::sanitize;

fn discover_or_exit() -> Arc<dyn GitBackend> {
    let discovered = std::env::current_dir()
        .map_err(anyhow::Error::from)
        .and_then(|cwd| Ok(Git2Backend::open_env(&cwd)?));
    match discovered {
        Ok(backend) => Arc::new(backend),
        Err(e) => exit_human(&e),
    }
}

fn discover_or_init_or_exit(can_prompt: bool) -> Arc<dyn GitBackend> {
    discover_or_init(can_prompt).unwrap_or_else(|e| exit_human(&e))
}

/// Report `error` on stderr as git words it, and exit with git's code.
fn exit_human(error: &anyhow::Error) -> ! {
    let (text, code) = output::human(error, None);
    eprintln!("{text}");
    exit(code)
}

fn discover_or_init(can_prompt: bool) -> anyhow::Result<Arc<dyn GitBackend>> {
    let cwd = std::env::current_dir()?;
    match Git2Backend::open_env(&cwd) {
        Ok(backend) => Ok(Arc::new(backend)),
        Err(rgit_git::GitError::NotARepository(_)) if can_prompt => {
            if !crate::interactive::confirm(&format!(
                "No git repository found. Initialize one in {}?",
                cwd.display()
            ))? {
                anyhow::bail!("cancelled");
            }
            rgit_git::init(&cwd, &Default::default())?;
            Ok(Arc::new(Git2Backend::open_env(&cwd)?))
        }
        Err(rgit_git::GitError::NotARepository(_)) => Err(CliError::not_a_repo()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_keep_the_structured_output_flag() {
        let hint = "Run `rgit stash pop`, then `rgit --full --toon log` or `rgit`";
        assert_eq!(
            keep_mode(hint, OutputMode::Porcelain),
            "Run `rgit --toon stash pop`, then `rgit --full --toon log` or `rgit --toon`"
        );
        let names = "valid flags for `rgit diff`: --stat";
        assert_eq!(keep_mode(names, OutputMode::Porcelain), names);
        assert_eq!(
            keep_mode("Run `rgit status`", OutputMode::Json),
            "Run `rgit --json status`"
        );
        assert_eq!(keep_mode(hint, OutputMode::Text), hint);
    }

    fn render(out: Output) -> String {
        toon::encode(&out.finalize(&[], false, "rgit").unwrap())
    }

    #[test]
    fn forge_listing_reports_range_and_next_page() {
        let text = r#"{"items":[{"number":7,"title":"t","state":"open","html_url":"u","head_branch":"h","base_branch":"main","draft":false}],"page":2,"more":true,"total":150}"#;
        let out = render(forge_output(
            text.to_owned(),
            Some("rgit forge pr list o/r"),
        ));
        assert!(out.contains("count: 101-101 of 150 total"), "{out}");
        assert!(
            out.contains("pull_requests[1]{number,title,state,head_branch}:"),
            "{out}"
        );
        assert!(out.contains("rgit forge pr list o/r --page 3"), "{out}");
    }

    #[test]
    fn forge_listing_empty_page_is_explicit() {
        let text = r#"{"items":[],"page":3,"more":false,"total":null}"#;
        let out = render(forge_output(
            text.to_owned(),
            Some("rgit forge branch list"),
        ));
        assert_eq!(out, "branches: 0 remote branches on page 3");
    }

    #[test]
    fn forge_hints_keep_flags_and_repo() {
        let cmd = cli::ForgeCmd::Pr {
            cmd: cli::PrCmd::Close {
                number: 1,
                target: Some("o/r".to_owned()),
                repo: None,
                yes: true,
            },
        };
        let (hints, list) = forge_hints(&cmd, "rgit forge --profile work");
        assert_eq!(list, None);
        assert_eq!(
            hints,
            ["Run `rgit forge --profile work pr list o/r` to see the remaining pull requests"]
        );
    }
}
