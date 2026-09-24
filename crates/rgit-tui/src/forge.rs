//! Native forge integration detected from the current Git remote.

use std::path::PathBuf;

use rgit_model::{NodeKind, Section, Span, Style};
use serde::Deserialize;

/// A pull or merge request, normalized across sources.
#[derive(Debug, Deserialize, Clone)]
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
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct ForgeSnapshot {
    pub provider: String,
    pub host: String,
    pub account: String,
    pub repository: String,
    pub requests: Vec<PullRequest>,
}

pub async fn load(
    origin: Option<String>,
    _workdir: PathBuf,
    configured_account: Option<String>,
    configured_host: Option<String>,
) -> Result<ForgeSnapshot, String> {
    let configured_host_for_detection = configured_host.as_deref();
    let Some((provider, remote_host, repo)) = origin
        .as_deref()
        .and_then(|url| parse_forge_remote(url, configured_host_for_detection))
    else {
        return Err("the current remote is not a supported forge repository".to_owned());
    };
    let account = configured_account
        .or_else(|| std::env::var("RGIT_FORGE_ACCOUNT").ok())
        .unwrap_or_else(|| "default".to_owned());
    let host = configured_host.unwrap_or(remote_host);
    let repository = format!("{}/{}", repo.owner, repo.name);
    let requests = match provider.as_str() {
        "github" => {
            let client = rgit_forge::GithubClient::from_environment_for(&account)
                .await
                .map_err(|error| forge_error(&provider, &host, &account, error))?;
            client
                .pull_requests(&repo)
                .await
                .map_err(|error| forge_error(&provider, &host, &account, error))?
                .into_iter()
                .map(|pr| PullRequest {
                    number: pr.number,
                    title: pr.title,
                    state: pr.state,
                    branch: pr.head_branch,
                    checks: None,
                    url: pr.html_url,
                })
                .collect()
        }
        "gitlab" => {
            let client = rgit_forge::GitlabClient::from_environment_for(&host, &account)
                .map_err(|error| forge_error(&provider, &host, &account, error))?;
            client
                .merge_requests(&repo)
                .await
                .map_err(|error| forge_error(&provider, &host, &account, error))?
                .into_iter()
                .map(|mr| PullRequest {
                    number: mr.number,
                    title: mr.title,
                    state: mr.state,
                    branch: mr.head_branch,
                    checks: None,
                    url: mr.html_url,
                })
                .collect()
        }
        _ => return Err("the current remote is not a supported forge repository".to_owned()),
    };
    Ok(ForgeSnapshot {
        provider,
        host,
        account,
        repository,
        requests,
    })
}

fn forge_error(provider: &str, host: &str, account: &str, error: impl std::fmt::Display) -> String {
    format!("{provider} forge ({host}, account {account}): {error}")
}

/// Parse GitHub and GitLab remote URLs, including SSH and nested GitLab groups.
pub fn parse_forge_remote(
    url: &str,
    configured_gitlab_host: Option<&str>,
) -> Option<(String, String, rgit_forge::RepoRef)> {
    let value = url.trim().trim_end_matches(".git");
    let (host, path) = if value.contains("://") {
        let (_, authority_path) = value.split_once("://")?;
        let (authority, path) = authority_path.split_once('/')?;
        (
            authority
                .rsplit_once('@')
                .map(|(_, host)| host)
                .unwrap_or(authority),
            path,
        )
    } else {
        let (authority, path) = value.split_once(':')?;
        (authority.rsplit_once('@').map(|(_, host)| host)?, path)
    };
    let provider = if host.eq_ignore_ascii_case("github.com") {
        "github"
    } else if host.eq_ignore_ascii_case("gitlab.com")
        || configured_gitlab_host
            .is_some_and(|configured| configured.trim_end_matches('/').contains(host))
        || std::env::var("GITLAB_HOST")
            .ok()
            .is_some_and(|configured| configured.trim_end_matches('/').contains(host))
    {
        "gitlab"
    } else {
        return None;
    };
    Some((
        provider.to_owned(),
        format!("https://{host}"),
        rgit_forge::RepoRef::parse(path).ok()?,
    ))
}

/// Render the forge context and pull-request list as view content.
pub fn build_view(snapshot: &ForgeSnapshot) -> Vec<Section> {
    let mut sections = vec![Section::leaf(
        "forge/context",
        NodeKind::DiffLine,
        vec![Span::new(
            format!(
                "{} {} ({}) · account {}",
                snapshot.provider, snapshot.repository, snapshot.host, snapshot.account
            ),
            Style::Dim,
        )],
    )];
    if snapshot.requests.is_empty() {
        sections.push(Section::leaf(
            "forge/empty",
            NodeKind::DiffLine,
            vec![Span::new("no open pull requests".to_owned(), Style::Dim)],
        ));
        return sections;
    }
    sections.extend(snapshot.requests.iter().map(|pr| {
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
    }));
    sections
}
pub fn build_placeholder() -> Vec<Section> {
    vec![Section::leaf(
        "forge/loading",
        NodeKind::Info,
        vec![Span::new(
            "forge data is not loaded · press Ctrl-G to load".to_owned(),
            Style::Dim,
        )],
    )]
}

/// Render the selected request metadata for inspection before mutation.
pub fn build_detail(snapshot: &ForgeSnapshot, request: &PullRequest) -> Vec<Section> {
    let row = |id: &str, label: &str, value: String| {
        Section::leaf(
            id,
            NodeKind::Info,
            vec![
                Span::new(format!("{label}: "), Style::Dim),
                Span::plain(value),
            ],
        )
    };
    vec![
        Section::leaf(
            "forge/detail/title",
            NodeKind::Section,
            vec![Span::new(request.title.clone(), Style::SectionHeader)],
        ),
        row(
            "forge/detail/context",
            "Forge",
            format!(
                "{} · {} · account {}",
                snapshot.provider, snapshot.host, snapshot.account
            ),
        ),
        row(
            "forge/detail/repository",
            "Repository",
            snapshot.repository.clone(),
        ),
        row("forge/detail/state", "State", request.state.clone()),
        row("forge/detail/branch", "Branch", request.branch.clone()),
        row("forge/detail/url", "URL", request.url.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_remote_providers() {
        let (provider, host, repo) =
            parse_forge_remote("https://github.com/ray/rgit.git", None).expect("GitHub remote");
        assert_eq!(provider, "github");
        assert_eq!(host, "https://github.com");
        assert_eq!(repo.owner, "ray");
        assert_eq!(repo.name, "rgit");

        let (provider, _, repo) =
            parse_forge_remote("git@gitlab.com:group/subgroup/project.git", None)
                .expect("GitLab remote");
        assert_eq!(provider, "gitlab");
        assert_eq!(repo.owner, "group");
        assert_eq!(repo.name, "subgroup/project");
        assert!(parse_forge_remote("https://example.com/a/b", None).is_none());
        assert!(
            parse_forge_remote(
                "https://gitlab.internal.example/group/project",
                Some("https://gitlab.internal.example")
            )
            .is_some()
        );
    }
}
