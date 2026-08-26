use std::process::exit;
use std::sync::Arc;

use clap::Parser;
use rgit_git::{Git2Backend, GitBackend};

use crate::cli::{Cli, Command};

mod cli;
mod interactive;
mod logging;
mod mcp;
mod prompt;
mod render;
mod stack;

fn main() -> ! {
    logging::init();
    let cli = Cli::parse();

    match cli.command {
        // The MCP server takes over stdio for the process lifetime.
        Some(Command::Mcp) => exit(mcp::serve(discover_or_exit())),
        // Repo creation runs before discovery (there is no repo yet); both use
        // libgit2 directly rather than shelling out.
        Some(Command::Init { path }) => {
            let path = path.unwrap_or_else(|| ".".to_owned());
            exit(report_result(
                rgit_git::init(std::path::Path::new(&path)),
                "ok",
            ));
        }
        Some(Command::Clone { url, dir }) => {
            // Default the target directory to the repo name, as git does.
            let dir = dir.unwrap_or_else(|| {
                url.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("repo")
                    .trim_end_matches(".git")
                    .to_owned()
            });
            let result = rgit_git::clone(&url, std::path::Path::new(&dir), &|_| {});
            exit(report_result(result, &format!("cloned into {dir}")));
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
            exit(runtime.block_on(run_tui()));
        }
    }
}

async fn run_tui() -> i32 {
    match rgit_tui::run(discover_or_exit()).await {
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
