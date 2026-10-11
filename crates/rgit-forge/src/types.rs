use std::fmt;

use serde::{Deserialize, Serialize};

use super::ForgeError;

/// A forge provider rgit can talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Github,
    Gitlab,
}

impl Provider {
    /// Every provider rgit supports, in a stable order.
    pub const ALL: [Self; 2] = [Self::Github, Self::Gitlab];

    /// Parse a provider name as it appears in a target prefix, a config
    /// profile or a command argument.
    pub fn parse(value: &str) -> Result<Self, ForgeError> {
        if value.eq_ignore_ascii_case("github") {
            Ok(Self::Github)
        } else if value.eq_ignore_ascii_case("gitlab") {
            Ok(Self::Gitlab)
        } else {
            Err(ForgeError::UnsupportedProvider(value.to_owned()))
        }
    }

    /// The name of this provider as config, a target prefix and output write it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
        }
    }

    /// The provider behind a remote host. `gitlab_host` is a configured
    /// self-hosted GitLab, such as the profile host or `GITLAB_HOST`.
    pub fn from_host(host: &str, gitlab_host: Option<&str>) -> Option<Self> {
        if host.eq_ignore_ascii_case("github.com") {
            return Some(Self::Github);
        }
        let is_gitlab = |configured: Option<&str>| {
            configured.is_some_and(|value| value.trim_end_matches('/').contains(host))
        };
        if host.eq_ignore_ascii_case("gitlab.com")
            || is_gitlab(gitlab_host)
            || is_gitlab(std::env::var("GITLAB_HOST").ok().as_deref())
        {
            return Some(Self::Gitlab);
        }
        None
    }

    /// The host this provider is reached at when nothing configures another.
    pub fn default_host(self) -> &'static str {
        match self {
            Self::Github => "https://github.com",
            Self::Gitlab => "https://gitlab.com",
        }
    }

    /// The host this provider reads from the environment, when it has one.
    /// Only a self-hosted GitLab is placed in the environment.
    pub fn env_host(self) -> Option<String> {
        let name = match self {
            Self::Github => return None,
            Self::Gitlab => "GITLAB_HOST",
        };
        std::env::var(name)
            .ok()
            .filter(|host| !host.trim().is_empty())
    }

    /// The environment variables that can carry this provider's token, in the
    /// order they are read.
    pub fn token_env_vars(self) -> &'static [&'static str] {
        match self {
            Self::Github => &["GITHUB_TOKEN", "GH_TOKEN"],
            Self::Gitlab => &["GITLAB_TOKEN", "GITLAB_ACCESS_TOKEN"],
        }
    }

    /// The token this provider reads from the environment, if any.
    pub fn env_token(self) -> Option<String> {
        self.token_env_vars()
            .iter()
            .find_map(|name| std::env::var(name).ok())
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.as_str())
    }
}

/// A repository on a forge, as `OWNER/REPO` (a GitLab group path may nest more
/// than one level under `owner`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    pub fn parse(value: &str) -> Result<Self, ForgeError> {
        let value = value.trim().trim_end_matches(".git");
        let (owner, name) = value
            .split_once('/')
            .ok_or_else(|| ForgeError::InvalidRepository(value.to_owned()))?;
        if owner.is_empty() || name.is_empty() {
            return Err(ForgeError::InvalidRepository(value.to_owned()));
        }
        Ok(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }
}

impl fmt::Display for RepoRef {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}/{}", self.owner, self.name)
    }
}

/// Items per page for forge list calls, the largest both forges allow.
pub const PAGE_SIZE: u32 = 100;

/// One page of a forge list. `more` says whether later pages exist; `total` is
/// the item count across all pages when the forge reports or implies it.
#[derive(Debug, Clone, Serialize)]
pub struct Listing<T> {
    pub items: Vec<T>,
    pub page: u32,
    pub more: bool,
    pub total: Option<u64>,
}

impl<T> Listing<T> {
    pub(crate) fn map<U>(self, f: impl FnMut(T) -> U) -> Listing<U> {
        Listing {
            items: self.items.into_iter().map(f).collect(),
            page: self.page,
            more: self.more,
            total: self.total,
        }
    }

    pub(crate) fn map_filter<U>(self, f: impl FnMut(T) -> Option<U>) -> Listing<U> {
        Listing {
            items: self.items.into_iter().filter_map(f).collect(),
            page: self.page,
            more: self.more,
            total: self.total,
        }
    }
}

/// The item count implied by the last page: every earlier page is full.
pub(crate) fn total_from_last_page(page: u32, items_on_page: usize) -> Option<u64> {
    (page == 1 || items_on_page > 0)
        .then(|| u64::from(page - 1) * u64::from(PAGE_SIZE) + items_on_page as u64)
}

pub(crate) fn page_query(route: &str, page: u32) -> String {
    let sep = if route.contains('?') { '&' } else { '?' };
    format!("{route}{sep}per_page={PAGE_SIZE}&page={}", page.max(1))
}

/// One CI check on a pull request's head commit, its status normalized to
/// `passing`, `failing`, `pending` or `skipped`.
#[derive(Debug, Clone, Serialize)]
pub struct CheckRun {
    pub name: String,
    pub status: String,
    pub url: Option<String>,
}

/// The CI rollup of a pull request's checks: `Failing` when any check fails,
/// `Pending` when none has failed but some has not finished, `Passing` when
/// they all pass, `None` when there are none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckRollup {
    Passing,
    Failing,
    Pending,
    None,
}

/// The checks of a pull request's head commit.
#[derive(Debug, Clone, Serialize)]
pub struct PullRequestChecks {
    pub rollup: CheckRollup,
    pub checks: Vec<CheckRun>,
}

/// The normalized status of a provider check status or conclusion.
pub(crate) fn normalize_check_status(value: &str) -> &'static str {
    match value {
        "success" | "passed" => "passing",
        "failure" | "failed" | "error" | "timed_out" | "startup_failure" | "action_required"
        | "stale" => "failing",
        "queued"
        | "pending"
        | "running"
        | "created"
        | "in_progress"
        | "waiting"
        | "waiting_for_resource"
        | "preparing"
        | "scheduled"
        | "manual" => "pending",
        _ => "skipped",
    }
}

/// The rollup over a batch of normalized check statuses: a failing check
/// loses, an unfinished one makes it pending, and an empty batch is `None`.
pub(crate) fn rollup_from(statuses: impl IntoIterator<Item = String>) -> CheckRollup {
    use CheckRollup::*;
    let mut failing = false;
    let mut pending = false;
    let mut count = 0;
    for status in statuses {
        count += 1;
        match status.as_str() {
            "failing" => failing = true,
            "pending" => pending = true,
            _ => {}
        }
    }
    if failing {
        Failing
    } else if pending {
        Pending
    } else if count == 0 {
        None
    } else {
        Passing
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub name: String,
    pub full_name: String,
    pub html_url: String,
    pub clone_url: Option<String>,
    pub ssh_url: Option<String>,
    pub private: bool,
    pub default_branch: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub sha: String,
    pub protected: bool,
}

/// The state filter of a pull request list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullRequestState {
    #[default]
    Open,
    Closed,
    Merged,
    All,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub draft: bool,
    /// The head author's login or username.
    pub author: Option<String>,
    pub body: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    /// The commit id the head branch points at.
    pub head_sha: Option<String>,
    /// Whether the forge will accept a merge of this branch; `None` when it
    /// has not computed a verdict (GitHub list entries never carry one).
    pub mergeable: Option<bool>,
    pub merged: bool,
    pub labels: Vec<String>,
    /// The requested reviewers' logins or usernames.
    pub reviewers: Vec<String>,
}

/// How a pull request merge combines the head branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    /// The name of this method as the merge command spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }
}

impl fmt::Display for MergeMethod {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.as_str())
    }
}

/// What a pull request review asks: approval, changes, or a plain comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewAction {
    /// The review event name GitHub's API takes for this action.
    pub fn github_event(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
            Self::Comment => "COMMENT",
        }
    }
}

/// One comment on a pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestComment {
    pub id: u64,
    pub body: String,
    pub author: Option<String>,
    pub url: Option<String>,
    pub created_at: Option<String>,
}

/// One review of a pull request. `state` is the action taken (`approved`,
/// `changes_requested`, `commented`); GitLab approvals carry no id or body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestReview {
    pub id: Option<u64>,
    pub state: String,
    pub body: Option<String>,
    pub author: Option<String>,
    pub url: Option<String>,
}

/// What `Forge::ensure_pull_request` did: opened a new pull request, or worked
/// on the existing one (`retargeted` when it moved its base branch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureOutcome {
    Created,
    Updated { retargeted: bool },
}

/// An existing or newly created pull request, with what ensure did to it.
#[derive(Debug, Clone)]
pub struct EnsuredPullRequest {
    pub pull_request: PullRequest,
    pub outcome: EnsureOutcome,
}

/// Markers around the stack table `rgit submit` writes into each pull
/// request body. Text outside the markers is never touched.
pub const STACK_START: &str = "<!-- rgit-stack -->";
pub const STACK_END: &str = "<!-- /rgit-stack -->";

/// The stack table for pull request `current`: one `- #number branch` row
/// per stacked request, in stack order, with the current row marked.
pub fn render_stack_block(entries: &[(u64, String)], current: u64) -> String {
    let mut out = String::from(STACK_START);
    for (number, branch) in entries {
        out.push_str(&format!("\n- #{number} {branch}"));
        if *number == current {
            out.push_str("  <- this PR");
        }
    }
    out.push_str(&format!("\n{STACK_END}"));
    out
}

/// The stack branches above `branch`, nearest first, from `(branch, parent)`
/// pairs.
pub fn branches_above(parents: &[(String, Option<String>)], branch: &str) -> Vec<String> {
    let mut above: Vec<String> = Vec::new();
    let mut queue = std::collections::VecDeque::from([branch.to_owned()]);
    while let Some(cur) = queue.pop_front() {
        for (child, parent) in parents {
            if parent.as_deref() == Some(cur.as_str()) && !above.contains(child) {
                above.push(child.clone());
                queue.push_back(child.clone());
            }
        }
    }
    above
}

/// The body `body` carries with the stack `block` in it: a well-formed marked
/// section (the first end marker and the last start marker before it) is
/// replaced, else the block is appended after a blank line. Without such a
/// pair, lines that are only a stray marker are dropped so a later run cannot
/// pair them with the new block. Other text is left alone.
pub fn upsert_stack_block(body: Option<&str>, block: &str) -> String {
    let body = body.unwrap_or("");
    if let Some(end) = body.find(STACK_END)
        && let Some(start) = body[..end].rfind(STACK_START)
    {
        let end = end + STACK_END.len();
        return format!("{}{block}{}", &body[..start], &body[end..]);
    }
    // Rejoining lines turns CRLF into LF, so only a body with a stray marker
    // goes through it.
    let kept = if body.contains(STACK_START) || body.contains(STACK_END) {
        body.lines()
            .filter(|line| {
                let line = line.trim();
                line != STACK_START && line != STACK_END
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        body.to_owned()
    };
    let trimmed = kept.trim_end();
    if trimmed.is_empty() {
        block.to_owned()
    } else {
        format!("{trimmed}\n\n{block}")
    }
}

/// The state filter of an issue list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IssueState {
    #[default]
    Open,
    Closed,
    All,
}

/// One issue: a pull request never appears here (GitHub lists pull requests
/// as issues, so that filter drops them before mapping).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: Option<String>,
    pub body: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub url: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// A new issue: labels and assignees are logins or usernames.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateIssue {
    pub title: String,
    pub body: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
}

/// One branch to ensure a pull request for: its head branch, base branch and
/// the tip commit message that fills the title and body.
#[derive(Debug, Clone)]
pub struct StackEntry {
    pub branch: String,
    pub base: String,
    pub message: String,
}

/// The PR title and body a tip commit message fills in, as `gh pr create
/// --fill` does: the subject as the title, the rest of the message as the body.
pub fn fill_from_commit(message: &str) -> (String, Option<String>) {
    let message = message.trim();
    let (subject, body) = match message.split_once('\n') {
        Some((subject, body)) => (subject.trim(), body.trim()),
        None => (message, ""),
    };
    let body = (!body.is_empty()).then(|| body.to_owned());
    (subject.to_owned(), body)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub login: String,
    pub name: Option<String>,
    pub html_url: String,
    pub avatar_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRepository {
    pub name: String,
    pub description: Option<String>,
    pub private: bool,
    pub auto_init: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePullRequest {
    pub title: String,
    pub head: String,
    pub base: String,
    pub body: Option<String>,
    pub draft: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub access_expires_at: Option<u64>,
    pub refresh_expires_at: Option<u64>,
}

impl StoredCredential {
    /// A stored value that is either the JSON credential or a bare access token
    /// written by an earlier version.
    pub(crate) fn from_json_or_token(value: String) -> Self {
        match serde_json::from_str(&value) {
            Ok(credential) => credential,
            Err(_) => Self {
                access_token: value,
                refresh_token: None,
                access_expires_at: None,
                refresh_expires_at: None,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialIdentity {
    pub provider: String,
    pub host: String,
    pub account: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_block_marks_the_current_request() {
        let entries = vec![(12, "feat-a".to_owned()), (13, "feat-b".to_owned())];
        assert_eq!(
            render_stack_block(&entries, 13),
            "<!-- rgit-stack -->\n- #12 feat-a\n- #13 feat-b  <- this PR\n<!-- /rgit-stack -->"
        );
    }

    #[test]
    fn stack_block_appends_when_missing() {
        let block = render_stack_block(&[(12, "feat-a".to_owned())], 12);
        assert_eq!(upsert_stack_block(None, &block), block);
        assert_eq!(
            upsert_stack_block(Some("Longer story."), &block),
            format!("Longer story.\n\n{block}")
        );
    }

    #[test]
    fn stack_block_replaces_without_touching_outside_text() {
        let old = render_stack_block(&[(12, "feat-a".to_owned())], 12);
        let new = render_stack_block(&[(12, "feat-a".to_owned()), (13, "feat-b".to_owned())], 12);
        let body = format!("Intro.\n\n{old}\n\nOutro.");
        assert_eq!(
            upsert_stack_block(Some(&body), &new),
            format!("Intro.\n\n{new}\n\nOutro.")
        );
    }

    #[test]
    fn stack_block_survives_an_orphan_start_marker() {
        let block = render_stack_block(&[(12, "feat-a".to_owned())], 12);
        let body = format!("Intro.\n{STACK_START}\nMy notes.");
        let once = upsert_stack_block(Some(&body), &block);
        assert_eq!(once, format!("Intro.\nMy notes.\n\n{block}"));
        let twice = upsert_stack_block(Some(&once), &block);
        assert_eq!(twice, once);
        assert_eq!(twice.matches(STACK_START).count(), 1);
    }

    #[test]
    fn stack_block_ignores_an_end_marker_before_the_start() {
        let block = render_stack_block(&[(12, "feat-a".to_owned())], 12);
        let body = format!("A\n{STACK_END}\nB\n{STACK_START}\nC");
        let out = upsert_stack_block(Some(&body), &block);
        assert_eq!(out, format!("A\nB\nC\n\n{block}"));
    }

    #[test]
    fn lists_stack_branches_above_nearest_first() {
        let pair = |b: &str, p: Option<&str>| (b.to_owned(), p.map(str::to_owned));
        let parents = [
            pair("c", Some("b")),
            pair("a", Some("main")),
            pair("b", Some("a")),
            pair("other", None),
        ];
        assert_eq!(branches_above(&parents, "a"), ["b", "c"]);
        assert!(branches_above(&parents, "c").is_empty());
    }

    #[test]
    fn stack_block_keeps_crlf_line_ends() {
        let block = render_stack_block(&[(12, "feat-a".to_owned())], 12);
        let out = upsert_stack_block(Some("One.\r\nTwo.\r\n"), &block);
        assert_eq!(out, format!("One.\r\nTwo.\n\n{block}"));
    }

    #[test]
    fn stack_block_keeps_a_prose_mention_of_the_marker() {
        let old = render_stack_block(&[(12, "feat-a".to_owned())], 12);
        let new = render_stack_block(&[(13, "feat-b".to_owned())], 13);
        let prose = format!("Write {STACK_START} to start a table.");
        let body = format!("{prose}\n\n{old}");
        assert_eq!(
            upsert_stack_block(Some(&body), &new),
            format!("{prose}\n\n{new}")
        );
    }

    #[test]
    fn parses_repository_reference() {
        let repo = RepoRef::parse("owner/project.git").expect("valid repository");
        assert_eq!(repo.owner, "owner");
        assert_eq!(repo.name, "project");
        assert!(RepoRef::parse("owner").is_err());
    }

    #[test]
    fn maps_hosts_to_providers() {
        assert_eq!(
            Provider::from_host("github.com", None),
            Some(Provider::Github)
        );
        assert_eq!(
            Provider::from_host("gitlab.com", None),
            Some(Provider::Gitlab)
        );
        assert_eq!(
            Provider::from_host("code.example.com", Some("https://code.example.com")),
            Some(Provider::Gitlab)
        );
        assert_eq!(Provider::from_host("code.example.com", None), None);
    }

    #[test]
    fn reads_a_provider_name_case_insensitively() {
        assert_eq!(Provider::parse("GitHub").unwrap(), Provider::Github);
        assert_eq!(Provider::parse("gitlab").unwrap(), Provider::Gitlab);
        assert!(Provider::parse("gitea").is_err());
    }

    #[test]
    fn a_commit_message_fills_the_title_and_body() {
        let (title, body) = fill_from_commit("Add forge PRs\n\nLonger story.\n");
        assert_eq!(title, "Add forge PRs");
        assert_eq!(body.as_deref(), Some("Longer story."));
        let (title, body) = fill_from_commit("Single line\n");
        assert_eq!(title, "Single line");
        assert_eq!(body, None);
        let (title, body) = fill_from_commit("\n\nLead sentence\n\nMore.\n");
        assert_eq!(title, "Lead sentence");
        assert_eq!(body.as_deref(), Some("More."));
    }

    #[test]
    fn last_page_implies_the_total() {
        assert_eq!(total_from_last_page(1, 0), Some(0));
        assert_eq!(total_from_last_page(1, 42), Some(42));
        assert_eq!(total_from_last_page(4, 7), Some(307));
        assert_eq!(total_from_last_page(5, 0), None);
    }

    #[test]
    fn classifies_provider_check_statuses() {
        assert_eq!(normalize_check_status("success"), "passing");
        assert_eq!(normalize_check_status("failure"), "failing");
        assert_eq!(normalize_check_status("timed_out"), "failing");
        assert_eq!(normalize_check_status("error"), "failing");
        assert_eq!(normalize_check_status("in_progress"), "pending");
        assert_eq!(normalize_check_status("running"), "pending");
        assert_eq!(normalize_check_status("cancelled"), "skipped");
        assert_eq!(normalize_check_status("neutral"), "skipped");
    }

    #[test]
    fn rolls_up_check_statuses() {
        use CheckRollup::*;
        assert_eq!(rollup_from(Vec::<String>::new()), None);
        assert_eq!(rollup_from(["passing".to_owned()]), Passing);
        assert_eq!(
            rollup_from(["skipped".to_owned(), "passing".to_owned()]),
            Passing
        );
        assert_eq!(
            rollup_from(["passing".to_owned(), "pending".to_owned()]),
            Pending
        );
        assert_eq!(
            rollup_from(["pending".to_owned(), "failing".to_owned()]),
            Failing
        );
    }

    #[test]
    fn page_query_appends_to_existing_query() {
        assert_eq!(page_query("/r/b", 2), "/r/b?per_page=100&page=2");
        assert_eq!(
            page_query("/r/p?state=open", 0),
            "/r/p?state=open&per_page=100&page=1"
        );
    }
}
