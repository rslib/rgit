//! Native forge integration: GitHub, GitLab and Forgejo API clients, one remote parser
//! and one profile loader, behind a single [`Forge`] client.

mod auth;
mod forgejo;
mod github;
mod gitlab;
mod profile;
mod remote;
mod types;

use std::fmt;
use std::path::PathBuf;

pub use auth::{
    DeviceFlowConfig, GITHUB_OAUTH_CLIENT_ID, credential_identities, delete_credential_for,
    delete_provider_token, delete_token, load_credential, load_credential_for, load_provider_token,
    login_browser, login_device, read_token_stdin, save_credential, save_credential_for,
    save_provider_token,
};
pub use forgejo::ForgejoClient;
pub use github::GithubClient;
pub use gitlab::GitlabClient;
pub use profile::{ForgeConfig, ForgeProfile, ForgeSelection, ForgeTarget};
pub use remote::RemoteTarget;
pub use types::{
    Branch, CheckRollup, CheckRun, CreateIssue, CreatePullRequest, CreateRepository,
    CredentialIdentity, EnsureOutcome, EnsuredPullRequest, Issue, IssueState, Listing, MergeMethod,
    PAGE_SIZE, Provider, PullRequest, PullRequestChecks, PullRequestComment, PullRequestReview,
    PullRequestState, RepoRef, Repository, ReviewAction, STACK_END, STACK_START, StackEntry,
    StoredCredential, ThreadEntry, ThreadKind, User, branches_above, fill_from_commit,
    render_stack_block, upsert_stack_block,
};

/// An error from a forge operation. Every variant names the forge it came from
/// where the forge is known, so a GitLab failure never reads as a GitHub one.
#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("forge authentication required; run `rgit forge login <provider>`")]
    AuthenticationRequired,
    #[error("credential store error: {0}")]
    CredentialStore(String),
    #[error("{provider} API error: {message}")]
    Api { provider: Provider, message: String },
    #[error("invalid repository reference {0:?}; expected OWNER/REPO")]
    InvalidRepository(String),
    #[error("invalid response from the forge: {0}")]
    Response(String),
    #[error("OAuth error: {0}")]
    OAuth(String),
    #[error("unsupported forge provider {0:?}")]
    UnsupportedProvider(String),
    #[error("cannot parse git remote {0:?}")]
    InvalidRemote(String),
    #[error("unsupported forge remote host {0:?}")]
    UnsupportedHost(String),
    #[error("a repository target is required before OWNER/REPO")]
    MissingTarget,
    #[error("the forge target names no repository")]
    MissingRepository,
    #[error("forge profile {0:?} is not configured")]
    UnknownProfile(String),
    #[error("cannot read forge profile config {}: {detail}", path.display())]
    ConfigRead { path: PathBuf, detail: String },
    #[error("cannot parse forge profile config {}: {detail}", path.display())]
    ConfigParse { path: PathBuf, detail: String },
}

impl ForgeError {
    pub(crate) fn api(provider: Provider, message: impl fmt::Display) -> Self {
        Self::Api {
            provider,
            message: message.to_string(),
        }
    }
}

/// One client per provider behind one API, so a caller never dispatches on a
/// provider name.
pub enum Forge {
    Github(GithubClient),
    Gitlab(GitlabClient),
    Forgejo(ForgejoClient),
}

impl Forge {
    /// Connect to the forge of `target` with its account and host.
    pub async fn connect(target: &ForgeTarget) -> Result<Self, ForgeError> {
        let github_base = (target.provider == Provider::Github
            && target.host != Provider::Github.default_host())
        .then_some(target.host.as_str());
        match target.provider {
            Provider::Github => Ok(Self::Github(
                GithubClient::from_environment_for(&target.account, github_base).await?,
            )),
            Provider::Gitlab => Ok(Self::Gitlab(GitlabClient::from_environment_for(
                &target.host,
                &target.account,
            )?)),
            Provider::Forgejo => Ok(Self::Forgejo(ForgejoClient::from_environment_for(
                &target.host,
                &target.account,
            )?)),
        }
    }

    pub fn provider(&self) -> Provider {
        match self {
            Self::Github(_) => Provider::Github,
            Self::Gitlab(_) => Provider::Gitlab,
            Self::Forgejo(_) => Provider::Forgejo,
        }
    }

    pub async fn repository(&self, repo: &RepoRef) -> Result<Repository, ForgeError> {
        match self {
            Self::Github(client) => client.repository(repo).await,
            Self::Gitlab(client) => client.repository(repo).await,
            Self::Forgejo(client) => client.repository(repo).await,
        }
    }

    pub async fn create_repository(
        &self,
        request: &CreateRepository,
        organization: Option<&str>,
    ) -> Result<Repository, ForgeError> {
        match self {
            Self::Github(client) => client.create_repository(request, organization).await,
            Self::Gitlab(client) => client.create_repository(request, organization).await,
            Self::Forgejo(client) => client.create_repository(request, organization).await,
        }
    }

    pub async fn delete_repository(&self, repo: &RepoRef) -> Result<(), ForgeError> {
        match self {
            Self::Github(client) => client.delete_repository(repo).await,
            Self::Gitlab(client) => client.delete_repository(repo).await,
            Self::Forgejo(client) => client.delete_repository(repo).await,
        }
    }

    pub async fn branches(&self, repo: &RepoRef, page: u32) -> Result<Listing<Branch>, ForgeError> {
        match self {
            Self::Github(client) => client.branches(repo, page).await,
            Self::Gitlab(client) => client.branches(repo, page).await,
            Self::Forgejo(client) => client.branches(repo, page).await,
        }
    }

    pub async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> Result<(), ForgeError> {
        match self {
            Self::Github(client) => client.delete_branch(repo, branch).await,
            Self::Gitlab(client) => client.delete_branch(repo, branch).await,
            Self::Forgejo(client) => client.delete_branch(repo, branch).await,
        }
    }

    /// One page of pull requests, by `state`, head branch and head author;
    /// `head` and `author` filter without matching anything when unset.
    pub async fn pull_requests(
        &self,
        repo: &RepoRef,
        page: u32,
        state: PullRequestState,
        head: Option<&str>,
        author: Option<&str>,
    ) -> Result<Listing<PullRequest>, ForgeError> {
        match self {
            Self::Github(client) => client.pull_requests(repo, page, state, head, author).await,
            Self::Forgejo(client) => client.pull_requests(repo, page, state, head, author).await,
            Self::Gitlab(client) => client.merge_requests(repo, page, state, head, author).await,
        }
    }

    /// One pull request by its number (a GitLab merge request's `iid`).
    pub async fn pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => client.pull_request(repo, number).await,
            Self::Forgejo(client) => client.pull_request(repo, number).await,
            Self::Gitlab(client) => client.pull_merge_request(repo, number).await,
        }
    }

    /// The pull request whose head branch is `branch`, in any state; `None`
    /// when the branch has no pull request.
    pub async fn pull_request_for_branch(
        &self,
        repo: &RepoRef,
        branch: &str,
    ) -> Result<Option<PullRequest>, ForgeError> {
        let mut page = 1;
        loop {
            let listing = self
                .pull_requests(repo, page, PullRequestState::All, Some(branch), None)
                .await?;
            if let Some(found) = listing.items.iter().find(|pr| pr.head_branch == branch) {
                return Ok(Some(found.clone()));
            }
            if !listing.more {
                return Ok(None);
            }
            page += 1;
        }
    }

    /// The remote ref that carries the pull request's head commit: GitHub's
    /// `refs/pull/N/head`, GitLab's `refs/merge-requests/N/head`; fetchable
    /// with a refspec like `+<this>:refs/rgit/pr/N`.
    pub fn pull_request_ref(&self, number: u64) -> String {
        match self {
            Self::Github(_) => format!("refs/pull/{number}/head"),
            Self::Gitlab(_) => format!("refs/merge-requests/{number}/head"),
            Self::Forgejo(_) => format!("refs/pull/{number}/head"),
        }
    }

    /// The CI checks of the pull request's head commit, with a rollup.
    pub async fn pull_request_checks(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequestChecks, ForgeError> {
        match self {
            Self::Github(client) => client.pull_request_checks(repo, number).await,
            Self::Forgejo(client) => client.pull_request_checks(repo, number).await,
            Self::Gitlab(client) => client.pull_request_checks(repo, number).await,
        }
    }

    pub async fn create_pull_request(
        &self,
        repo: &RepoRef,
        request: &CreatePullRequest,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => client.create_pull_request(repo, request).await,
            Self::Forgejo(client) => client.create_pull_request(repo, request).await,
            Self::Gitlab(client) => client.create_merge_request(repo, request).await,
        }
    }

    /// Find the open pull request for `head`: retarget its base when it points
    /// elsewhere, and open one filled with `title`, `body` and `draft` when no
    /// open pull request for the branch exists.
    pub async fn ensure_pull_request(
        &self,
        repo: &RepoRef,
        head: &str,
        base: &str,
        title: &str,
        body: Option<&str>,
        draft: bool,
    ) -> Result<EnsuredPullRequest, ForgeError> {
        let mut page = 1;
        loop {
            let listing = self
                .pull_requests(repo, page, PullRequestState::Open, Some(head), None)
                .await?;
            if let Some(existing) = listing.items.iter().find(|pr| pr.head_branch == head) {
                return if existing.base_branch != base {
                    let pull_request = self
                        .retarget_pull_request(repo, existing.number, base)
                        .await?;
                    Ok(EnsuredPullRequest {
                        pull_request,
                        outcome: EnsureOutcome::Updated { retargeted: true },
                    })
                } else {
                    Ok(EnsuredPullRequest {
                        pull_request: existing.clone(),
                        outcome: EnsureOutcome::Updated { retargeted: false },
                    })
                };
            }
            if !listing.more {
                break;
            }
            page += 1;
        }
        let request = CreatePullRequest {
            title: title.to_owned(),
            head: head.to_owned(),
            base: base.to_owned(),
            body: body.map(str::to_owned),
            draft,
        };
        Ok(EnsuredPullRequest {
            pull_request: self.create_pull_request(repo, &request).await?,
            outcome: EnsureOutcome::Created,
        })
    }

    /// Ensure a pull request per stack entry, filling each title and body from
    /// the entry's tip commit message the way `gh pr create --fill` does,
    /// then write the stack table into each body. `above` names the stack
    /// branches above the entries: their open pull requests join the table
    /// and get it too, so a submit from the middle of a stack keeps the whole
    /// stack listed. Returns one `opened` or `updated` line per entry, for
    /// submit reports.
    pub async fn ensure_pull_requests(
        &self,
        repo: &RepoRef,
        entries: &[StackEntry],
        above: &[String],
    ) -> Result<Vec<String>, ForgeError> {
        let mut ensured = Vec::with_capacity(entries.len());
        for entry in entries {
            let (title, body) = fill_from_commit(&entry.message);
            let one = self
                .ensure_pull_request(
                    repo,
                    &entry.branch,
                    &entry.base,
                    &title,
                    body.as_deref(),
                    false,
                )
                .await?;
            ensured.push(one);
        }
        let mut stacked: Vec<&PullRequest> = ensured.iter().map(|one| &one.pull_request).collect();
        let mut upper = Vec::new();
        for branch in above {
            if let Some(pr) = self.pull_request_for_branch(repo, branch).await?
                && !pr.merged
                && matches!(pr.state.as_str(), "open" | "opened")
            {
                upper.push(pr);
            }
        }
        stacked.extend(upper.iter());
        let table: Vec<(u64, String)> = stacked
            .iter()
            .map(|pr| (pr.number, pr.head_branch.clone()))
            .collect();
        if table.len() >= 2 {
            for pr in &stacked {
                let block = render_stack_block(&table, pr.number);
                let body = upsert_stack_block(pr.body.as_deref(), &block);
                self.edit_pull_request(repo, pr.number, None, Some(&body), None)
                    .await?;
            }
        }
        let mut lines = Vec::with_capacity(entries.len());
        for (entry, one) in entries.iter().zip(ensured.iter()) {
            let word = match one.outcome {
                EnsureOutcome::Created => "opened",
                EnsureOutcome::Updated { .. } => "updated",
            };
            lines.push(format!(
                "{word} {} ({})",
                one.pull_request.html_url, entry.branch
            ));
        }
        Ok(lines)
    }

    pub async fn retarget_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        base: &str,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => client.retarget_pull_request(repo, number, base).await,
            Self::Forgejo(client) => client.retarget_pull_request(repo, number, base).await,
            Self::Gitlab(client) => client.retarget_pull_request(repo, number, base).await,
        }
    }

    pub async fn close_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => client.close_pull_request(repo, number).await,
            Self::Forgejo(client) => client.close_pull_request(repo, number).await,
            Self::Gitlab(client) => client.close_merge_request(repo, number).await,
        }
    }

    /// Merge pull request `number` with `method`, deleting its head branch
    /// when `delete_branch` is set.
    pub async fn merge_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => {
                client
                    .merge_pull_request(repo, number, method, delete_branch)
                    .await
            }
            Self::Forgejo(client) => {
                client
                    .merge_pull_request(repo, number, method, delete_branch)
                    .await
            }
            Self::Gitlab(client) => {
                client
                    .merge_pull_request(repo, number, method, delete_branch)
                    .await
            }
        }
    }

    /// Change the title, body and base branch pull request `number` carries;
    /// each `None` leaves that field alone.
    pub async fn edit_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        title: Option<&str>,
        body: Option<&str>,
        base: Option<&str>,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => {
                client
                    .edit_pull_request(repo, number, title, body, base)
                    .await
            }
            Self::Forgejo(client) => {
                client
                    .edit_pull_request(repo, number, title, body, base)
                    .await
            }
            Self::Gitlab(client) => {
                client
                    .edit_merge_request(repo, number, title, body, base)
                    .await
            }
        }
    }

    /// Mark pull request `number` ready for review.
    pub async fn mark_pull_request_ready(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => client.set_pull_request_draft(repo, number, false).await,
            Self::Forgejo(_) => Err(ForgeError::UnsupportedProvider("forgejo".to_owned())),
            Self::Gitlab(client) => client.set_merge_request_draft(repo, number, false).await,
        }
    }

    /// Mark pull request `number` a draft.
    pub async fn mark_pull_request_draft(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => client.set_pull_request_draft(repo, number, true).await,
            Self::Forgejo(_) => Err(ForgeError::UnsupportedProvider("forgejo".to_owned())),
            Self::Gitlab(client) => client.set_merge_request_draft(repo, number, true).await,
        }
    }

    /// Reopen pull request `number`.
    pub async fn reopen_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => client.reopen_pull_request(repo, number).await,
            Self::Forgejo(client) => client.reopen_pull_request(repo, number).await,
            Self::Gitlab(client) => client.reopen_merge_request(repo, number).await,
        }
    }

    /// Comment `body` on pull request `number`.
    pub async fn comment_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
    ) -> Result<PullRequestComment, ForgeError> {
        match self {
            Self::Github(client) => client.comment_pull_request(repo, number, body).await,
            Self::Forgejo(client) => client.comment_pull_request(repo, number, body).await,
            Self::Gitlab(client) => client.comment_merge_request(repo, number, body).await,
        }
    }

    /// Review pull request `number` with `action`, carrying `body` as the
    /// review text.
    pub async fn review_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        action: ReviewAction,
        body: Option<&str>,
    ) -> Result<PullRequestReview, ForgeError> {
        match self {
            Self::Github(client) => client.review_pull_request(repo, number, action, body).await,
            Self::Forgejo(_) => Err(ForgeError::UnsupportedProvider("forgejo".to_owned())),
            Self::Gitlab(client) => {
                client
                    .review_merge_request(repo, number, action, body)
                    .await
            }
        }
    }

    /// The whole conversation on pull request `number`: plain comments,
    /// inline code comments and reviews, oldest first.
    pub async fn pull_request_thread(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<Vec<ThreadEntry>, ForgeError> {
        match self {
            Self::Github(client) => client.pull_request_thread(repo, number).await,
            Self::Forgejo(_) => Err(ForgeError::UnsupportedProvider("forgejo".to_owned())),
            Self::Gitlab(client) => client.pull_request_thread(repo, number).await,
        }
    }

    /// One page of issues, by `state`, author login, label and assignee
    /// login; each filter is skipped when unset.
    pub async fn issues(
        &self,
        repo: &RepoRef,
        page: u32,
        state: IssueState,
        author: Option<&str>,
        label: Option<&str>,
        assignee: Option<&str>,
    ) -> Result<Listing<Issue>, ForgeError> {
        match self {
            Self::Github(client) => {
                client
                    .issues(repo, page, state, author, label, assignee)
                    .await
            }
            Self::Forgejo(client) => {
                client
                    .issues(repo, page, state, author, label, assignee)
                    .await
            }
            Self::Gitlab(client) => {
                client
                    .issues(repo, page, state, author, label, assignee)
                    .await
            }
        }
    }

    /// One issue by its number (a GitLab issue's `iid`).
    pub async fn issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, ForgeError> {
        match self {
            Self::Github(client) => client.issue(repo, number).await,
            Self::Forgejo(client) => client.issue(repo, number).await,
            Self::Gitlab(client) => client.issue(repo, number).await,
        }
    }

    /// Open an issue from `request`.
    pub async fn create_issue(
        &self,
        repo: &RepoRef,
        request: &CreateIssue,
    ) -> Result<Issue, ForgeError> {
        match self {
            Self::Github(client) => client.create_issue(repo, request).await,
            Self::Forgejo(client) => client.create_issue(repo, request).await,
            Self::Gitlab(client) => client.create_issue(repo, request).await,
        }
    }

    /// Close issue `number`.
    pub async fn close_issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, ForgeError> {
        match self {
            Self::Github(client) => client.close_issue(repo, number).await,
            Self::Forgejo(client) => client.close_issue(repo, number).await,
            Self::Gitlab(client) => client.close_issue(repo, number).await,
        }
    }

    /// Reopen issue `number`.
    pub async fn reopen_issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, ForgeError> {
        match self {
            Self::Github(client) => client.reopen_issue(repo, number).await,
            Self::Forgejo(client) => client.reopen_issue(repo, number).await,
            Self::Gitlab(client) => client.reopen_issue(repo, number).await,
        }
    }

    /// Comment `body` on issue `number`.
    pub async fn comment_issue(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
    ) -> Result<PullRequestComment, ForgeError> {
        match self {
            Self::Github(client) => client.comment_issue(repo, number, body).await,
            Self::Forgejo(client) => client.comment_issue(repo, number, body).await,
            Self::Gitlab(client) => client.comment_issue(repo, number, body).await,
        }
    }

    /// The whole conversation on issue `number`: its comments, oldest first.
    pub async fn issue_thread(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<Vec<ThreadEntry>, ForgeError> {
        match self {
            Self::Github(client) => client.issue_thread(repo, number).await,
            Self::Forgejo(client) => client.issue_thread(repo, number).await,
            Self::Gitlab(client) => client.issue_thread(repo, number).await,
        }
    }

    /// Change the title, body, labels and assignees issue `number` carries;
    /// each `None` (or empty list) leaves that field alone.
    pub async fn edit_issue(
        &self,
        repo: &RepoRef,
        number: u64,
        title: Option<&str>,
        body: Option<&str>,
        labels: Option<&[String]>,
        assignees: Option<&[String]>,
    ) -> Result<Issue, ForgeError> {
        match self {
            Self::Github(client) => {
                client
                    .edit_issue(repo, number, title, body, labels, assignees)
                    .await
            }
            Self::Forgejo(client) => {
                client
                    .edit_issue(repo, number, title, body, labels, assignees)
                    .await
            }
            Self::Gitlab(client) => {
                client
                    .edit_issue(repo, number, title, body, labels, assignees)
                    .await
            }
        }
    }

    pub async fn current_user(&self) -> Result<User, ForgeError> {
        match self {
            Self::Github(client) => client.current_user().await,
            Self::Gitlab(client) => client.current_user().await,
            Self::Forgejo(client) => client.current_user().await,
        }
    }
}
