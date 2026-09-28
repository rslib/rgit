use std::io::IsTerminal;
use std::process::exit;
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches};
use rgit_git::{Git2Backend, GitBackend};
use serde_json::Value;

use crate::cli::{Cli, CliError, Command, HooksCmd, OutputMode};
use crate::output::Output;
use crate::toon::{Node, Obj};

mod axi;
mod cli;
mod creds;
mod date;
mod examples;
mod forge;
mod globals;
mod graph;
mod interactive;
mod lanes;
mod logging;
mod mcp;
mod output;
mod plumbing;
mod pretty;
mod prompt;
mod render;
mod setup;
mod stack;
mod toon;

/// How results are printed for this invocation.
struct Emit {
    mode: OutputMode,
    fields: Vec<String>,
    full: bool,
    rerun: String,
}

fn main() -> ! {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag] = args.as_slice()
        && flag == "-V"
    {
        println!("{}", env!("CARGO_PKG_VERSION"));
        exit(0);
    }
    let (args, paginate) = globals::apply(args);
    let args = globals::dispatch(args);
    logging::init();
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let parsed = examples::apply(Cli::command())
        .try_get_matches_from(
            std::iter::once("rgit".to_owned())
                .chain(blame_scores(count_shorthand(&plumbing::grep_tokens(&args)))),
        )
        .and_then(|m| Cli::from_arg_matches(&m));
    let cli = match parsed {
        Ok(cli) => cli,
        Err(e) => usage_exit(e, &args, stdout_is_terminal),
    };
    let output_mode = cli.output_mode(stdout_is_terminal);
    let structured_output = output_mode != OutputMode::Text;
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
    };

    match cli.command {
        // The MCP server takes over stdio for the process lifetime.
        Some(Command::Mcp) => exit(mcp::serve(discover_or_exit())),
        Some(Command::Skills { cmd }) => finish(cli::run_skills(cmd), &emit),
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
        Some(Command::Status {
            porcelain,
            short,
            branch,
            z,
            untracked,
            ignored,
            verbose,
            ahead_behind,
            no_ahead_behind,
            paths,
        }) if porcelain.is_some()
            || short
            || branch
            || z
            || verbose > 0
            || ahead_behind
            || no_ahead_behind =>
        {
            if let Err(error) = discover_or_init(false) {
                die(error, &emit);
            }
            let mut args = vec!["status".to_owned()];
            args.extend(porcelain.map(|v| format!("--porcelain={v}")));
            args.extend(short.then(|| "--short".to_owned()));
            args.extend(branch.then(|| "--branch".to_owned()));
            args.extend(z.then(|| "-z".to_owned()));
            args.extend(untracked.map(|m| format!("--untracked-files={m}")));
            args.extend(ignored.then(|| "--ignored".to_owned()));
            args.extend((0..verbose).map(|_| "-v".to_owned()));
            args.extend(ahead_behind.then(|| "--ahead-behind".to_owned()));
            args.extend(no_ahead_behind.then(|| "--no-ahead-behind".to_owned()));
            args.push("--".to_owned());
            args.extend(paths);
            // Run in the current folder so paths read and print as git's do.
            match std::process::Command::new("git").args(&args).status() {
                Ok(s) => exit(s.code().unwrap_or(1)),
                Err(e) => die(anyhow::anyhow!("could not run git: {e}"), &emit),
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
            // Color and prompts only on a real terminal.
            render::init_color(
                output_mode == OutputMode::Text,
                if cli.no_color {
                    Some("never")
                } else {
                    cli.color.as_deref()
                },
            );
            if output_mode == OutputMode::Text {
                globals::start_pager(globals::command_name(&args).as_deref(), paginate);
            }
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
            let result = if structured_output {
                axi::run(&backend, command, can_prompt)
            } else {
                cli::run(&backend, command, can_prompt).map(Output::from)
            };
            let _ = backend.smudge_racy(since);
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
    if emit.mode == OutputMode::Json {
        value.push(("ok".to_owned(), Node::Bool(true)));
        println!("{}", json(value));
    } else {
        println!("{}", toon::encode(&value));
    }
    exit(cli::exit_code());
}

fn die(error: anyhow::Error, emit: &Emit) -> ! {
    let (message, help, code) = output::translate(&error);
    fail(message, help, code, emit.mode)
}

/// Report an error on stdout in the output format, or on stderr for humans.
fn fail(message: String, help: Vec<String>, code: i32, mode: OutputMode) -> ! {
    match mode {
        OutputMode::Text if message.is_empty() => {}
        // A multi-line message is a report in git's own words (a refused push).
        OutputMode::Text if message.contains('\n') => {
            eprintln!("{message}");
            for h in &help {
                eprintln!("hint: {h}");
            }
        }
        OutputMode::Text => {
            eprintln!("rgit: {message}");
            let advice = std::env::var("GIT_ADVICE").map_or(true, |v| v != "0" && v != "false");
            for h in help.iter().filter(|_| advice) {
                eprintln!("hint: {h}");
            }
        }
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
fn usage_exit(e: clap::Error, args: &[String], stdout_is_terminal: bool) -> ! {
    use clap::error::ErrorKind;
    let has = |flags: &[&str]| args.iter().any(|a| flags.contains(&a.as_str()));
    let mode = if has(&["--json"]) {
        OutputMode::Json
    } else if has(&["--toon", "--axi"]) || !stdout_is_terminal && !has(&["--human", "--text"]) {
        OutputMode::Porcelain
    } else {
        OutputMode::Text
    };
    if mode == OutputMode::Text
        || matches!(
            e.kind(),
            ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        )
    {
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
        .map_err(|e| e.to_string())
        .and_then(|cwd| Git2Backend::open_env(&cwd).map_err(|e| e.to_string()));
    match discovered {
        Ok(backend) => Arc::new(backend),
        Err(e) => {
            eprintln!("rgit: {e}");
            exit(1);
        }
    }
}

fn discover_or_init_or_exit(can_prompt: bool) -> Arc<dyn GitBackend> {
    match discover_or_init(can_prompt) {
        Ok(backend) => backend,
        Err(e) => {
            eprintln!("rgit: {e}");
            exit(1);
        }
    }
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
