//! Optional forge integration. Prefers the native GitHub API (octocrab) when a
//! token is present, and falls back to the `gh` / `glab` CLIs otherwise. A
//! missing token and missing CLI surface as an error toast, never a hard fail.

use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_model::{NodeKind, Section, Span, Style};
use serde::Deserialize;

/// A pull or merge request, normalized across sources.
#[derive(Debug, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    #[serde(rename = "headRefName")]
    pub branch: String,
}

/// Load open requests: the native API when `GITHUB_TOKEN`/`GH_TOKEN` and a
/// GitHub origin are both available, else the forge CLI.
pub async fn load(origin: Option<String>, workdir: PathBuf) -> Result<Vec<PullRequest>, String> {
    let token = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok();
    if let (Some(token), Some((owner, repo))) =
        (token, origin.as_deref().and_then(parse_github_repo))
    {
        return github_prs(&owner, &repo, token).await;
    }
    tokio::task::spawn_blocking(move || cli_prs(&workdir))
        .await
        .map_err(|e| e.to_string())?
}

/// Owner and repo parsed from a GitHub remote URL (https or ssh forms).
pub fn parse_github_repo(url: &str) -> Option<(String, String)> {
    let s = url.trim();
    let rest = [
        "git@github.com:",
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
    ]
    .iter()
    .find_map(|prefix| s.strip_prefix(prefix))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let owner = parts.next()?.to_owned();
    let repo = parts.next()?.to_owned();
    (!owner.is_empty() && !repo.is_empty()).then_some((owner, repo))
}

async fn github_prs(owner: &str, repo: &str, token: String) -> Result<Vec<PullRequest>, String> {
    let octo = octocrab::Octocrab::builder()
        .personal_token(token)
        .build()
        .map_err(|e| e.to_string())?;
    let page = octo
        .pulls(owner, repo)
        .list()
        .state(octocrab::params::State::Open)
        .per_page(50)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(page
        .items
        .into_iter()
        .map(|p| PullRequest {
            number: p.number,
            title: p.title.unwrap_or_default(),
            state: p
                .state
                .map(|s| format!("{s:?}").to_lowercase())
                .unwrap_or_default(),
            branch: p.head.ref_field,
        })
        .collect())
}

/// Open requests via `gh`, or `glab` as a fallback.
fn cli_prs(workdir: &Path) -> Result<Vec<PullRequest>, String> {
    match gh(workdir) {
        Ok(prs) => Ok(prs),
        Err(gh_err) => {
            glab(workdir).map_err(|glab_err| format!("no forge: gh: {gh_err}; glab: {glab_err}"))
        }
    }
}

fn gh(workdir: &Path) -> Result<Vec<PullRequest>, String> {
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,title,state,headRefName",
            "--limit",
            "50",
        ])
        .current_dir(workdir)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

fn glab(workdir: &Path) -> Result<Vec<PullRequest>, String> {
    #[derive(Deserialize)]
    struct Mr {
        iid: u64,
        title: String,
        state: String,
        source_branch: String,
    }
    let out = Command::new("glab")
        .args(["mr", "list", "-F", "json"])
        .current_dir(workdir)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    let mrs: Vec<Mr> = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    Ok(mrs
        .into_iter()
        .map(|m| PullRequest {
            number: m.iid,
            title: m.title,
            state: m.state,
            branch: m.source_branch,
        })
        .collect())
}

/// Render the pull-request list as view content.
pub fn build_view(prs: &[PullRequest]) -> Vec<Section> {
    if prs.is_empty() {
        return vec![Section::leaf(
            "forge/empty",
            NodeKind::DiffLine,
            vec![Span::new("no open pull requests".to_owned(), Style::Dim)],
        )];
    }
    prs.iter()
        .map(|pr| {
            let state_style = if pr.state.eq_ignore_ascii_case("open") {
                Style::Added
            } else {
                Style::Dim
            };
            Section::leaf(
                format!("forge/{}", pr.number),
                NodeKind::Commit,
                vec![
                    Span::new(format!("#{:<5}", pr.number), Style::Hash),
                    Span::new(format!(" {} ", pr.state), state_style),
                    Span::plain(format!(" {}", pr.title)),
                    Span::new(format!("  {}", pr.branch), Style::Branch),
                ],
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_urls() {
        assert_eq!(
            parse_github_repo("https://github.com/ray/rgit.git"),
            Some(("ray".into(), "rgit".into()))
        );
        assert_eq!(
            parse_github_repo("git@github.com:ray/rgit.git"),
            Some(("ray".into(), "rgit".into()))
        );
        assert_eq!(parse_github_repo("https://gitlab.com/ray/rgit"), None);
    }
}
