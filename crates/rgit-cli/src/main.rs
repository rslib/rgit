use std::process::exit;
use std::sync::Arc;

use clap::Parser;
use rgit_git::{Git2Backend, GitBackend};

use crate::cli::{Cli, Command};

mod cli;
mod creds;
mod interactive;
mod lanes;
mod logging;
mod mcp;
mod prompt;
mod render;
mod stack;

fn main() -> ! {
    logging::init();
    // The in-process ssh transport falls back to a password prompt when key auth
    // fails and no ControlMaster socket exists to reuse.
    rgit_git::set_password_provider(creds::ssh_password);
    let cli = Cli::parse();

    match cli.command {
        // The MCP server takes over stdio for the process lifetime.
        Some(Command::Mcp) => exit(mcp::serve(discover_or_exit())),
        // Repo creation runs before discovery (there is no repo yet); both use
        // libgit2 directly rather than shelling out.
        Some(Command::Init {
            path,
            initial_branch,
            bare,
        }) => {
            let path = path.unwrap_or_else(|| ".".to_owned());
            exit(report_result(
                rgit_git::init(
                    std::path::Path::new(&path),
                    initial_branch.as_deref(),
                    bare,
                ),
                "ok",
            ));
        }
        Some(Command::Clone {
            url,
            dir,
            branch,
            depth,
        }) => {
            // Default the target directory to the repo name, as git does.
            let dir = dir.unwrap_or_else(|| {
                url.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("repo")
                    .trim_end_matches(".git")
                    .to_owned()
            });
            let result = rgit_git::clone(
                &url,
                std::path::Path::new(&dir),
                branch.as_deref(),
                depth,
                &|_| {},
            );
            exit(report_result(result, &format!("cloned into {dir}")));
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
                Err(e) => {
                    eprintln!("rgit: bad --host/--port ({e})");
                    exit(1);
                }
            };
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
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
                    runtime.block_on(rgit_web::serve_root(
                        root_path,
                        addr,
                        clone_base,
                        Some(mcp),
                    ))
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
                Err(e) => {
                    eprintln!("rgit: {e}");
                    exit(1);
                }
            }
        }
        // The `git` escape hatch is a transparent passthrough: inherit stdin,
        // stdout, and stderr so stdin-reading subcommands (commit-tree,
        // hash-object --stdin, apply) and interactive ones behave exactly like
        // git, and exit with git's own status.
        Some(Command::Git { args }) => {
            let backend = discover_or_exit();
            let status = std::process::Command::new("git")
                .args(&args)
                .current_dir(backend.workdir())
                .status();
            match status {
                Ok(s) => exit(s.code().unwrap_or(if s.success() { 0 } else { 1 })),
                Err(e) => {
                    eprintln!("rgit: could not run git: {e}");
                    exit(1);
                }
            }
        }
        // Every other subcommand runs one operation and prints compact output.
        Some(command) => {
            // Color and prompts only on a real terminal.
            render::set_color(std::io::IsTerminal::is_terminal(&std::io::stdout()));
            let backend = discover_or_exit();
            let interactive = interactive::enabled(cli.no_input);
            match cli::run(&backend, command, interactive) {
                Ok(out) => {
                    println!("{out}");
                    exit(0);
                }
                Err(e) => {
                    eprintln!("rgit: {e}");
                    exit(1);
                }
            }
        }
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
            exit(runtime.block_on(run_tui(cli.no_preview)));
        }
    }
}

async fn run_tui(no_preview: bool) -> i32 {
    match rgit_tui::run(discover_or_exit(), no_preview).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("rgit: {e}");
            1
        }
    }
}

/// Print `ok_msg` and exit 0 on success, or the error to stderr and exit 1.
fn report_result(result: Result<(), rgit_git::GitError>, ok_msg: &str) -> i32 {
    match result {
        Ok(()) => {
            println!("{ok_msg}");
            0
        }
        Err(e) => {
            eprintln!("rgit: {e}");
            1
        }
    }
}

fn discover_or_exit() -> Arc<dyn GitBackend> {
    let discovered = std::env::current_dir()
        .map_err(|e| e.to_string())
        .and_then(|cwd| Git2Backend::discover(&cwd).map_err(|e| e.to_string()));
    match discovered {
        Ok(backend) => Arc::new(backend),
        Err(e) => {
            eprintln!("rgit: {e}");
            exit(1);
        }
    }
}
