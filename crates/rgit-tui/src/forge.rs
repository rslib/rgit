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
    /// The request body, reviewers and draft flag, from the listing.
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub reviewers: Vec<String>,
    #[serde(default)]
    pub draft: bool,
    /// One row per CI check on the head commit, filled lazily after the list.
    #[serde(default)]
    pub check_list: Vec<ForgeCheck>,
    /// The comment thread, loaded with one extra call when the detail opens.
    #[serde(default)]
    pub thread: ThreadState,
}

/// One CI check on a pull request's head commit.
#[derive(Debug, Deserialize, Clone)]
pub struct ForgeCheck {
    pub name: String,
    pub status: String,
}

/// The comment thread a detail view carries: unloaded until the detail
/// opens, then loaded with one extra call.
#[derive(Debug, Clone, Default, Deserialize)]
pub enum ThreadState {
    #[default]
    Unloaded,
    Loading,
    Loaded(Vec<rgit_forge::ThreadEntry>),
    Failed(String),
}

/// One pull request's CI: the rollup plus one row per check.
#[derive(Debug, Clone)]
pub struct PullRequestCi {
    pub number: u64,
    pub rollup: Option<String>,
    pub checks: Vec<ForgeCheck>,
}

/// One issue on the forge, normalized across providers.
#[derive(Debug, Clone)]
pub struct ForgeIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
    pub body: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    /// The comment thread, loaded with one extra call when the detail opens.
    pub thread: ThreadState,
}

/// Which list the forge view shows: pull requests or issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ForgeTab {
    #[default]
    Requests,
    Issues,
}

impl ForgeTab {
    /// The other list.
    pub fn toggle(self) -> Self {
        match self {
            Self::Requests => Self::Issues,
            Self::Issues => Self::Requests,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Requests => "pull requests",
            Self::Issues => "issues",
        }
    }
}

/// The loaded forge state: the account and repository, plus its open requests
/// and issues.
#[derive(Debug, Clone)]
pub struct ForgeSnapshot {
    pub target: ForgeTarget,
    pub requests: Vec<PullRequest>,
    pub issues: Vec<ForgeIssue>,
    /// Why the issues could not load; the pull requests still show.
    pub issues_error: Option<String>,
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
            body: pr.body,
            reviewers: pr.reviewers,
            draft: pr.draft,
            check_list: Vec::new(),
            thread: ThreadState::Unloaded,
        })
        .collect();
    let (issues, issues_error) = match forge
        .issues(
            target.repo().expect("remote names a repository"),
            1,
            rgit_forge::IssueState::Open,
            None,
            None,
            None,
        )
        .await
    {
        Ok(listing) => (
            listing
                .items
                .into_iter()
                .map(|issue| ForgeIssue {
                    number: issue.number,
                    title: issue.title,
                    state: issue.state,
                    url: issue.url,
                    body: issue.body,
                    labels: issue.labels,
                    assignees: issue.assignees,
                    thread: ThreadState::Unloaded,
                })
                .collect(),
            None,
        ),
        Err(error) => (Vec::new(), Some(forge_error(&target, error))),
    };
    Ok(ForgeSnapshot {
        target,
        requests,
        issues,
        issues_error,
    })
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
    let above = match pairs.last() {
        Some((top, _)) => rgit_forge::branches_above(
            &backend.stack_parents().map_err(|error| error.to_string())?,
            top,
        ),
        None => Vec::new(),
    };
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
            .ensure_pull_requests(&repo, &entries, &above)
            .await
            .map_err(|error| forge_error(&target, error))
    })
}

/// Fetch the CI of `numbers` against `target`, skipping the requests that
/// fail so one bad request cannot hide the rest.
pub async fn load_checks(target: &ForgeTarget, numbers: Vec<u64>) -> Vec<PullRequestCi> {
    let Ok(repo) = target.repo().cloned() else {
        return Vec::new();
    };
    let Ok(forge) = rgit_forge::Forge::connect(target).await else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(numbers.len());
    for number in numbers {
        let Ok(checks) = forge.pull_request_checks(&repo, number).await else {
            continue;
        };
        let rollup = (!checks.checks.is_empty()).then(|| rollup_name(checks.rollup));
        out.push(PullRequestCi {
            number,
            rollup,
            checks: checks
                .checks
                .into_iter()
                .map(|check| ForgeCheck {
                    name: check.name,
                    status: check.status,
                })
                .collect(),
        });
    }
    out
}

/// The comment thread of one pull request or issue, oldest first;
/// `is_issue` picks the issue thread.
pub async fn load_thread(
    target: &ForgeTarget,
    number: u64,
    is_issue: bool,
) -> Result<Vec<rgit_forge::ThreadEntry>, String> {
    let repo = target.repo().map_err(|error| error.to_string())?.clone();
    let forge = rgit_forge::Forge::connect(target)
        .await
        .map_err(|error| forge_error(target, error))?;
    if is_issue {
        forge.issue_thread(&repo, number).await
    } else {
        forge.pull_request_thread(&repo, number).await
    }
    .map_err(|error| forge_error(target, error))
}

/// A check rollup as the list and detail rows print it.
fn rollup_name(rollup: rgit_forge::CheckRollup) -> String {
    match rollup {
        rgit_forge::CheckRollup::Passing => "passing",
        rgit_forge::CheckRollup::Failing => "failing",
        rgit_forge::CheckRollup::Pending => "pending",
        rgit_forge::CheckRollup::None => "none",
    }
    .to_owned()
}

/// The mark and style a check status prints with, shared by the list and the
/// detail view.
fn check_mark(status: &str) -> (&'static str, Style) {
    match status {
        "passing" => ("\u{2714}", Style::Added),
        "failing" => ("\u{2718}", Style::Deleted),
        _ => ("\u{2022}", Style::Dim),
    }
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

/// Render the forge context and the active list as view content: pull
/// requests or issues, one row per item (`i` switches the list).
pub fn build_view(snapshot: &ForgeSnapshot, tab: ForgeTab) -> Vec<Section> {
    let mut sections = vec![Section::leaf(
        "forge/context",
        NodeKind::DiffLine,
        vec![Span::new(
            format!(
                "{} {} ({}) \u{b7} account {} \u{b7} {} (i to switch)",
                snapshot.target.provider,
                repository(&snapshot.target),
                snapshot.target.host,
                snapshot.target.account,
                tab.name(),
            ),
            Style::Dim,
        )],
    )];
    match tab {
        ForgeTab::Requests => {
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
                    let (sym, style) = check_mark(checks);
                    spans.push(Span::new(format!(" {sym} {checks} "), style));
                }
                spans.push(Span::plain(format!(" {}", pr.title)));
                spans.push(Span::new(format!("  {}", pr.branch), Style::Branch));
                Section::leaf(format!("forge/{}", pr.number), NodeKind::Commit, spans)
            }));
            sections
        }
        ForgeTab::Issues => {
            if let Some(error) = &snapshot.issues_error {
                sections.push(Section::leaf(
                    "forge/empty",
                    NodeKind::DiffLine,
                    vec![Span::new(error.clone(), Style::Deleted)],
                ));
                return sections;
            }
            if snapshot.issues.is_empty() {
                sections.push(Section::leaf(
                    "forge/empty",
                    NodeKind::DiffLine,
                    vec![Span::new("no open issues".to_owned(), Style::Dim)],
                ));
                return sections;
            }
            sections.extend(snapshot.issues.iter().map(|issue| {
                let state_style = if issue.state.eq_ignore_ascii_case("open")
                    || issue.state.eq_ignore_ascii_case("opened")
                {
                    Style::Added
                } else {
                    Style::Dim
                };
                Section::leaf(
                    format!("forge/issue/{}", issue.number),
                    NodeKind::Commit,
                    vec![
                        Span::new(format!("#{:<5}", issue.number), Style::Hash),
                        Span::new(format!(" {} ", issue.state), state_style),
                        Span::plain(format!(" {}", issue.title)),
                    ],
                )
            }));
            sections
        }
    }
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

/// The comment thread under a detail view: one header per entry (author,
/// time, `path:line` for inline entries, review state) with its body below,
/// or the load error when the thread failed. Nothing shows until the thread
/// loads.
fn thread_sections(state: &ThreadState) -> Vec<Section> {
    let entries = match state {
        ThreadState::Loaded(entries) => entries,
        ThreadState::Failed(error) => {
            return vec![Section::leaf(
                "forge/detail/thread/error",
                NodeKind::Info,
                vec![Span::new(
                    format!("could not load comments: {error}"),
                    Style::Deleted,
                )],
            )];
        }
        ThreadState::Unloaded | ThreadState::Loading => return Vec::new(),
    };
    let mut sections = vec![Section::leaf(
        "forge/detail/thread",
        NodeKind::Info,
        vec![Span::new("Comments:", Style::Dim)],
    )];
    for (i, entry) in entries.iter().enumerate() {
        let mut header = Vec::new();
        if entry.in_reply_to.is_some() {
            header.push(Span::plain("  "));
        }
        header.push(Span::plain(
            entry.author.as_deref().unwrap_or("").to_owned(),
        ));
        if let Some(when) = entry.created_at.as_deref() {
            header.push(Span::new(format!("  {when}"), Style::Dim));
        }
        match (entry.path.as_deref(), entry.line) {
            (Some(path), Some(line)) => {
                header.push(Span::new(format!("  {path}:{line}"), Style::Branch));
            }
            (Some(path), None) => header.push(Span::new(format!("  {path}"), Style::Branch)),
            _ => {}
        }
        if let Some(state) = entry.state.as_deref() {
            let style = match state {
                "approved" => Style::Added,
                "changes_requested" => Style::Deleted,
                _ => Style::Dim,
            };
            header.push(Span::new(format!("  [{state}]"), style));
        }
        sections.push(Section::leaf(
            format!("forge/detail/thread/{i}"),
            NodeKind::Info,
            header,
        ));
        let indent = if entry.in_reply_to.is_some() {
            "    "
        } else {
            "  "
        };
        for (j, line) in entry.body.lines().enumerate() {
            sections.push(Section::leaf(
                format!("forge/detail/thread/{i}/{j}"),
                NodeKind::Info,
                vec![Span::plain(format!("{indent}{line}"))],
            ));
        }
    }
    sections
}

/// Render the selected request for inspection before mutation: its metadata,
/// body, reviewers and check list.
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
    let mut sections = vec![
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
    ];
    if request.draft {
        sections.push(row("forge/detail/draft", "Draft", "yes".to_owned()));
    }
    let reviewers = if request.reviewers.is_empty() {
        "none".to_owned()
    } else {
        request.reviewers.join(", ")
    };
    sections.push(row("forge/detail/reviewers", "Reviewers", reviewers));
    if let Some(body) = request
        .body
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty())
    {
        sections.push(Section::leaf(
            "forge/detail/body",
            NodeKind::Info,
            vec![Span::new("Body:", Style::Dim)],
        ));
        for (i, line) in body.lines().enumerate() {
            sections.push(Section::leaf(
                format!("forge/detail/body/{i}"),
                NodeKind::Info,
                vec![Span::plain(line)],
            ));
        }
    }
    if request.check_list.is_empty() {
        sections.push(row("forge/detail/checks", "Checks", "none".to_owned()));
    } else {
        sections.push(Section::leaf(
            "forge/detail/checks",
            NodeKind::Info,
            vec![Span::new("Checks:", Style::Dim)],
        ));
        for (i, check) in request.check_list.iter().enumerate() {
            let (sym, style) = check_mark(&check.status);
            sections.push(Section::leaf(
                format!("forge/detail/check/{i}"),
                NodeKind::Info,
                vec![
                    Span::new(format!("{sym} "), style),
                    Span::plain(check.name.clone()),
                ],
            ));
        }
    }
    sections.extend(thread_sections(&request.thread));
    sections
}

/// Render the selected issue for inspection: its metadata, body and labels.
pub fn build_issue_detail(snapshot: &ForgeSnapshot, issue: &ForgeIssue) -> Vec<Section> {
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
    let mut sections = vec![
        Section::leaf(
            "forge/detail/title",
            NodeKind::Section,
            vec![Span::new(issue.title.clone(), Style::SectionHeader)],
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
        row("forge/detail/state", "State", issue.state.clone()),
        row("forge/detail/url", "URL", issue.url.clone()),
    ];
    let labels = if issue.labels.is_empty() {
        "none".to_owned()
    } else {
        issue.labels.join(", ")
    };
    sections.push(row("forge/detail/labels", "Labels", labels));
    let assignees = if issue.assignees.is_empty() {
        "none".to_owned()
    } else {
        issue.assignees.join(", ")
    };
    sections.push(row("forge/detail/assignees", "Assignees", assignees));
    if let Some(body) = issue
        .body
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty())
    {
        sections.push(Section::leaf(
            "forge/detail/body",
            NodeKind::Info,
            vec![Span::new("Body:", Style::Dim)],
        ));
        for (i, line) in body.lines().enumerate() {
            sections.push(Section::leaf(
                format!("forge/detail/body/{i}"),
                NodeKind::Info,
                vec![Span::plain(line)],
            ));
        }
    }
    sections.extend(thread_sections(&issue.thread));
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_name(prefix: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .subsec_nanos();
        format!("{prefix}-{nanos:x}")
    }

    #[test]
    fn detects_supported_remote_providers() {
        let selection = ForgeSelection::default();
        let owner = random_name("owner");
        let remote = format!("https://github.com/{owner}/rgit.git");
        let target = ForgeTarget::from_remote(&selection, &remote).expect("GitHub remote");
        assert_eq!(target.provider, rgit_forge::Provider::Github);
        assert_eq!(target.host, "https://github.com");
        assert_eq!(
            target.repo().expect("repo").to_string(),
            format!("{owner}/rgit")
        );

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

    #[test]
    fn detects_a_forgejo_remote_by_profile_host() {
        let selection = ForgeSelection {
            provider: Some("forgejo".to_owned()),
            host: Some("https://git.rs.ht".to_owned()),
            ..ForgeSelection::default()
        };
        let target = ForgeTarget::from_remote(&selection, "forgejo@git.rs.ht:rayandrew/lab.git")
            .expect("Forgejo remote");
        assert_eq!(target.provider, rgit_forge::Provider::Forgejo);
        assert_eq!(target.host, "https://git.rs.ht");
        assert_eq!(target.repo().expect("repo").to_string(), "rayandrew/lab");
    }
}
