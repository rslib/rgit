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
    /// CI rollup: `passing`, `failing`, or `pending`; `None` when unknown (no
    /// checks, or a source that does not report them).
    #[serde(default)]
    pub checks: Option<String>,
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
    let mut out = Vec::with_capacity(page.items.len());
    for p in page.items {
        let checks = github_checks(&octo, owner, repo, &p.head.sha).await;
        out.push(PullRequest {
            number: p.number,
            title: p.title.unwrap_or_default(),
            state: p
                .state
                .map(|s| format!("{s:?}").to_lowercase())
                .unwrap_or_default(),
            branch: p.head.ref_field,
            checks,
        });
    }
    Ok(out)
}

/// The check-runs rollup for a commit via the GitHub API, or `None` on any error
/// or when the ref has no check runs. Best-effort: never fails the PR listing.
async fn github_checks(
    octo: &octocrab::Octocrab,
    owner: &str,
    repo: &str,
    sha: &str,
) -> Option<String> {
    #[derive(Deserialize)]
    struct Run {
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        conclusion: Option<String>,
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        check_runs: Vec<Run>,
    }
    let url = format!("/repos/{owner}/{repo}/commits/{sha}/check-runs");
    let resp: Resp = octo.get(url, None::<&()>).await.ok()?;
    rollup(
        resp.check_runs
            .iter()
            .map(|r| (r.conclusion.as_deref(), r.status.as_deref())),
    )
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
    #[derive(Deserialize)]
    struct Check {
        #[serde(default)]
        conclusion: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        state: Option<String>,
    }
    #[derive(Deserialize)]
    struct Row {
        number: u64,
        title: String,
        state: String,
        #[serde(rename = "headRefName")]
        branch: String,
        #[serde(rename = "statusCheckRollup", default)]
        checks: Vec<Check>,
    }
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,title,state,headRefName,statusCheckRollup",
            "--limit",
            "50",
        ])
        .current_dir(workdir)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    let rows: Vec<Row> = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let checks = r.checks.iter().map(|c| {
                (
                    c.conclusion.as_deref().or(c.state.as_deref()),
                    c.status.as_deref(),
                )
            });
            PullRequest {
                number: r.number,
                title: r.title,
                state: r.state,
                branch: r.branch,
                checks: rollup(checks),
            }
        })
        .collect())
}

/// Fold per-check `(conclusion-or-state, status)` values into one label:
/// `failing` if any failed, else `pending` if any is unfinished, else `passing`.
/// `None` when there are no checks at all.
fn rollup<'a>(checks: impl Iterator<Item = (Option<&'a str>, Option<&'a str>)>) -> Option<String> {
    let (mut any, mut fail, mut pending, mut pass) = (false, false, false, false);
    for (result, status) in checks {
        any = true;
        let running = matches!(
            status.map(str::to_ascii_uppercase).as_deref(),
            Some("IN_PROGRESS" | "QUEUED" | "PENDING" | "WAITING" | "REQUESTED")
        );
        match result.map(str::to_ascii_uppercase).as_deref() {
            Some("SUCCESS") => pass = true,
            Some("FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED") => {
                fail = true
            }
            _ => pending = true,
        }
        if running {
            pending = true;
        }
    }
    if !any {
        return None;
    }
    Some(
        if fail {
            "failing"
        } else if pending {
            "pending"
        } else if pass {
            "passing"
        } else {
            "pending"
        }
        .to_owned(),
    )
}

fn glab(workdir: &Path) -> Result<Vec<PullRequest>, String> {
    #[derive(Deserialize)]
    struct Pipeline {
        #[serde(default)]
        status: Option<String>,
    }
    #[derive(Deserialize)]
    struct Mr {
        iid: u64,
        title: String,
        state: String,
        source_branch: String,
        #[serde(default)]
        pipeline: Option<Pipeline>,
        #[serde(default)]
        head_pipeline: Option<Pipeline>,
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
        .map(|m| {
            let status = m
                .pipeline
                .or(m.head_pipeline)
                .and_then(|p| p.status);
            PullRequest {
                number: m.iid,
                title: m.title,
                state: m.state,
                branch: m.source_branch,
                checks: status.as_deref().and_then(gitlab_pipeline_label),
            }
        })
        .collect())
}

/// A GitLab pipeline status mapped to the shared `passing`/`failing`/`pending`
/// labels, or `None` for states with no clear result (canceled, skipped, ...).
fn gitlab_pipeline_label(status: &str) -> Option<String> {
    let label = match status.to_ascii_lowercase().as_str() {
        "success" => "passing",
        "failed" => "failing",
        "running" | "pending" | "created" | "preparing" | "scheduled"
        | "waiting_for_resource" => "pending",
        _ => return None,
    };
    Some(label.to_owned())
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
            let mut spans = vec![
                Span::new(format!("#{:<5}", pr.number), Style::Hash),
                Span::new(format!(" {} ", pr.state), state_style),
            ];
            if let Some(checks) = &pr.checks {
                let (sym, style) = match checks.as_str() {
                    "passing" => ("\u{2714}", Style::Added),
                    "failing" => ("\u{2718}", Style::Deleted),
                    _ => ("\u{2022}", Style::Dim),
                };
                spans.push(Span::new(format!(" {sym} {checks} "), style));
            }
            spans.push(Span::plain(format!(" {}", pr.title)));
            spans.push(Span::new(format!("  {}", pr.branch), Style::Branch));
            Section::leaf(format!("forge/{}", pr.number), NodeKind::Commit, spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_rollup_prioritizes_failure_then_pending() {
        let r = |v: Vec<(Option<&str>, Option<&str>)>| rollup(v.into_iter());
        assert_eq!(r(vec![]), None);
        assert_eq!(
            r(vec![(Some("SUCCESS"), Some("COMPLETED"))]).as_deref(),
            Some("passing")
        );
        assert_eq!(
            r(vec![
                (Some("SUCCESS"), Some("COMPLETED")),
                (Some("FAILURE"), Some("COMPLETED")),
            ])
            .as_deref(),
            Some("failing")
        );
        assert_eq!(
            r(vec![
                (Some("SUCCESS"), Some("COMPLETED")),
                (None, Some("IN_PROGRESS")),
            ])
            .as_deref(),
            Some("pending")
        );
    }

    #[test]
    fn gitlab_pipeline_maps_to_shared_labels() {
        assert_eq!(gitlab_pipeline_label("success").as_deref(), Some("passing"));
        assert_eq!(gitlab_pipeline_label("failed").as_deref(), Some("failing"));
        assert_eq!(gitlab_pipeline_label("running").as_deref(), Some("pending"));
        assert_eq!(gitlab_pipeline_label("canceled"), None);
    }

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
