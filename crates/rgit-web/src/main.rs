//! Standalone launcher for the viewer over the repo in the current directory.
//! `rgit serve` (the CLI subcommand) is the primary entry point; this binary is
//! for running the viewer on its own.

use std::net::SocketAddr;
use std::sync::Arc;

use rgit_git::{Git2Backend, GitBackend};

#[tokio::main]
async fn main() {
    let cwd = std::env::current_dir().expect("cwd");
    let backend: Arc<dyn GitBackend> = match Git2Backend::discover(&cwd) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            eprintln!("rgit serve: not a git repository ({e})");
            std::process::exit(1);
        }
    };
    let repo = rgit_web::repo_name(backend.as_ref());
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    if let Err(e) = rgit_web::serve(backend, repo, addr, None).await {
        eprintln!("rgit serve: {e}");
        std::process::exit(1);
    }
}
