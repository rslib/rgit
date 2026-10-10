//! Native forge integration detected from the current Git remote.

use rgit_forge::{ForgeError, ForgeSelection, ForgeTarget};
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

/// The loaded forge state: the account and repository, plus its open requests.
#[derive(Debug, Clone)]
pub struct ForgeSnapshot {
    pub target: ForgeTarget,
    pub requests: Vec<PullRequest>,
}

pub async fn load(
    origin: Option<String>,
    selection: ForgeSelection,
) -> Result<ForgeSnapshot, String> {
    let selection = selection.resolve().map_err(|error| error.to_string())?;
    let Some(origin) = origin else {
        return Err(unsupported_remote());
    };
    let target = ForgeTarget::from_remote(&selection, &origin).map_err(|error| {
        if remote_failure(&error) {
            unsupported_remote()
        } else {
            error.to_string()
        }
    })?;
    let forge = rgit_forge::Forge::connect(&target)
        .await
        .map_err(|error| forge_error(&target, error))?;
    let requests = forge
        .pull_requests(
            target.repo().expect("remote names a repository"),
            1,
            rgit_forge::PullRequestState::Open,
            None,
            None,
        )
        .await
        .map_err(|error| forge_error(&target, error))?
        .items
        .into_iter()
        .map(|pr| PullRequest {
            number: pr.number,
            title: pr.title,
            state: pr.state,
            branch: pr.head_branch,
            checks: None,
            url: pr.html_url,
        })
        .collect();
    Ok(ForgeSnapshot { target, requests })
}

/// Ensure a pull request per pushed `(branch, base)` pair against the origin
/// remote's forge, each filled from its branch's tip commit. Sync on the
/// caller's runtime handle, so the submit and lane operations can call it from
/// their blocking tasks (a runtime cannot be built inside one).
pub fn submit_pull_requests(
    handle: &tokio::runtime::Handle,
    backend: &dyn rgit_git::GitBackend,
    selection: ForgeSelection,
    pairs: Vec<(String, String)>,
) -> Result<Vec<String>, String> {
    let origin = backend
        .config_get("remote.origin.url")
        .ok()
        .flatten()
        .ok_or_else(|| "no origin remote is configured".to_owned())?;
    let selection = selection.resolve().map_err(|error| error.to_string())?;
    let mut entries = Vec::with_capacity(pairs.len());
    for (branch, base) in pairs {
        let message = backend
            .commit_message(&branch)
            .map_err(|error| error.to_string())?;
        entries.push(rgit_forge::StackEntry {
            branch,
            base,
            message,
        });
    }
    handle.block_on(async {
        let target = ForgeTarget::from_remote(&selection, origin.trim())
            .map_err(|error| error.to_string())?;
        let repo = target.repo().map_err(|error| error.to_string())?.clone();
        let forge = rgit_forge::Forge::connect(&target)
            .await
            .map_err(|error| forge_error(&target, error))?;
        forge
            .ensure_pull_requests(&repo, &entries)
            .await
            .map_err(|error| forge_error(&target, error))
    })
}

/// Whether the remote itself, rather than the config or the account, is at
/// fault.
fn remote_failure(error: &ForgeError) -> bool {
    matches!(
        error,
        ForgeError::InvalidRemote(_)
            | ForgeError::UnsupportedHost(_)
            | ForgeError::InvalidRepository(_)
    )
}

fn unsupported_remote() -> String {
    "the current remote is not a supported forge repository".to_owned()
}

fn forge_error(target: &ForgeTarget, error: impl std::fmt::Display) -> String {
    format!(
        "{} forge ({}, account {}): {error}",
        target.provider, target.host, target.account
    )
}

fn repository(target: &ForgeTarget) -> String {
    target
        .repo
        .as_ref()
        .map(rgit_forge::RepoRef::to_string)
        .unwrap_or_default()
}

/// Render the forge context and pull-request list as view content.
pub fn build_view(snapshot: &ForgeSnapshot) -> Vec<Section> {
    let mut sections = vec![Section::leaf(
        "forge/context",
        NodeKind::DiffLine,
        vec![Span::new(
            format!(
                "{} {} ({}) \u{b7} account {}",
                snapshot.target.provider,
                repository(&snapshot.target),
                snapshot.target.host,
                snapshot.target.account
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
            "forge data is not loaded \u{b7} press Ctrl-G to load".to_owned(),
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
                "{} \u{b7} {} \u{b7} account {}",
                snapshot.target.provider, snapshot.target.host, snapshot.target.account
            ),
        ),
        row(
            "forge/detail/repository",
            "Repository",
            repository(&snapshot.target),
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
        let selection = ForgeSelection::default();
        let target = ForgeTarget::from_remote(&selection, "https://github.com/ray/rgit.git")
            .expect("GitHub remote");
        assert_eq!(target.provider, rgit_forge::Provider::Github);
        assert_eq!(target.host, "https://github.com");
        assert_eq!(target.repo().expect("repo").to_string(), "ray/rgit");

        let target =
            ForgeTarget::from_remote(&selection, "git@gitlab.com:group/subgroup/project.git")
                .expect("GitLab remote");
        assert_eq!(target.provider, rgit_forge::Provider::Gitlab);
        assert_eq!(target.repo().expect("repo").owner, "group");
        assert_eq!(target.repo().expect("repo").name, "subgroup/project");

        assert!(ForgeTarget::from_remote(&selection, "https://example.com/a/b").is_err());

        let self_hosted = ForgeSelection {
            host: Some("https://gitlab.internal.example".to_owned()),
            ..ForgeSelection::default()
        };
        assert!(
            ForgeTarget::from_remote(
                &self_hosted,
                "https://gitlab.internal.example/group/project"
            )
            .is_ok()
        );
    }
}
